use std::{
    fmt, fs,
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
};

use super::{
    AccountProfile, AudioSource, ChromeCookieSource, DownloadDecision, DownloadIndex,
    DownloadOutcome, DownloadPaths, LoginPoll, LoginStatus, LoginTicket, NeteaseError,
    NeteaseProvider, NeteaseSession, Quality, RookieChromeCookieSource, SessionStore, Song,
    SongPage, WebNeteaseProvider, chrome_cookie_header, commit_download, retry_transient,
    tag_audio,
};

#[derive(Clone)]
pub struct NeteaseClient {
    provider: std::sync::Arc<dyn NeteaseProvider>,
    session_store: SessionStore,
}

impl fmt::Debug for NeteaseClient {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NeteaseClient")
            .field("session_store", &self.session_store)
            .finish_non_exhaustive()
    }
}

impl NeteaseClient {
    pub fn new(session_store: SessionStore) -> Self {
        Self::with_provider(
            std::sync::Arc::new(WebNeteaseProvider::new()),
            session_store,
        )
    }

    pub(super) fn with_provider(
        provider: std::sync::Arc<dyn NeteaseProvider>,
        session_store: SessionStore,
    ) -> Self {
        Self {
            provider,
            session_store,
        }
    }

    pub fn begin_login(&self) -> Result<LoginTicket, NeteaseError> {
        self.provider.begin_login()
    }

    pub fn poll_login(&self, ticket: &LoginTicket) -> Result<LoginStatus, NeteaseError> {
        match self.provider.poll_login(&ticket.key)? {
            LoginPoll::WaitingScan => Ok(LoginStatus::WaitingScan),
            LoginPoll::WaitingConfirmation => Ok(LoginStatus::WaitingConfirmation),
            LoginPoll::Expired => Ok(LoginStatus::Expired),
            LoginPoll::Authorized(authorized) => {
                let profile = self.provider.account(&authorized.cookie)?;
                self.save_authenticated_session(authorized.cookie, profile)?;
                Ok(LoginStatus::LoggedIn)
            }
        }
    }

    pub fn import_chrome_session(&self) -> Result<NeteaseSession, NeteaseError> {
        self.import_chrome_session_from(&RookieChromeCookieSource)
    }

    pub(super) fn import_chrome_session_from(
        &self,
        source: &dyn ChromeCookieSource,
    ) -> Result<NeteaseSession, NeteaseError> {
        let profiles = source.profiles()?;
        let mut candidates_found = 0_usize;
        for profile in profiles {
            let cookies = source.cookies(&profile)?;
            let Some(cookie) = chrome_cookie_header(&cookies) else {
                continue;
            };
            candidates_found += 1;
            let account = match self.provider.account(&cookie) {
                Ok(account) => account,
                Err(NeteaseError::LoginRequired) => continue,
                Err(error) => return Err(error),
            };
            return self.save_authenticated_session(cookie, account);
        }
        let message = if candidates_found == 0 {
            "no usable music.163.com login cookie was found; sign in with Chrome first"
        } else {
            "Chrome login cookies were found but NetEase rejected them; refresh the Chrome login and retry"
        };
        Err(NeteaseError::ChromeLogin(message.into()))
    }

    fn save_authenticated_session(
        &self,
        cookie: String,
        account: AccountProfile,
    ) -> Result<NeteaseSession, NeteaseError> {
        let session = NeteaseSession {
            cookie,
            user_id: account.user_id,
            nickname: account.nickname,
        };
        self.session_store.save(&session)?;
        Ok(session)
    }

    pub fn session(&self) -> Result<Option<NeteaseSession>, NeteaseError> {
        self.session_store.load()
    }

    pub fn logout(&self) -> Result<(), NeteaseError> {
        self.session_store.clear()
    }

    pub fn search(
        &self,
        query: &str,
        offset: usize,
        limit: usize,
    ) -> Result<SongPage, NeteaseError> {
        let session = self
            .session_store
            .load()?
            .ok_or(NeteaseError::LoginRequired)?;
        self.provider.search(&session.cookie, query, offset, limit)
    }

    pub fn liked_songs(&self) -> Result<Vec<Song>, NeteaseError> {
        let session = self
            .session_store
            .load()?
            .ok_or(NeteaseError::LoginRequired)?;
        self.provider.liked_songs(&session.cookie, session.user_id)
    }

    pub fn download_song(
        &self,
        music_root: &Path,
        song: &Song,
        cancellation: &AtomicBool,
    ) -> Result<DownloadOutcome, NeteaseError> {
        if cancellation.load(Ordering::Acquire) {
            return Err(NeteaseError::Cancelled);
        }
        if !song.available {
            return Ok(DownloadOutcome::Unavailable {
                song_id: song.id,
                reason: "not available for this account or region".into(),
            });
        }
        let session = self
            .session_store
            .load()?
            .ok_or(NeteaseError::LoginRequired)?;
        let Some(source) = self.select_audio_source(&session.cookie, song, cancellation)? else {
            return Ok(DownloadOutcome::Unavailable {
                song_id: song.id,
                reason: "no downloadable quality is available".into(),
            });
        };

        let output_root = music_root.join("NetEase");
        fs::create_dir_all(&output_root)?;
        let index_path = output_root.join(".k3-netease-downloads.json");
        let mut index = DownloadIndex::load(&index_path)?;
        let decision = index.decision(song.id, source.quality);
        if decision == DownloadDecision::Skip {
            return Ok(DownloadOutcome::Skipped {
                song_id: song.id,
                quality: source.quality,
            });
        }
        let paths = DownloadPaths::new(&output_root, song, &source, &decision);
        self.fetch_and_tag(song, &source, &paths.temporary, cancellation)?;
        if cancellation.load(Ordering::Acquire) {
            let _ = fs::remove_file(&paths.temporary);
            return Err(NeteaseError::Cancelled);
        }
        commit_download(&mut index, song, &source, &paths)?;
        Ok(DownloadOutcome::Downloaded {
            path: paths.destination,
            quality: source.quality,
        })
    }

    pub fn downloaded_song_path(
        &self,
        music_root: &Path,
        song_id: u64,
    ) -> Result<Option<std::path::PathBuf>, NeteaseError> {
        let index_path = music_root
            .join("NetEase")
            .join(".k3-netease-downloads.json");
        Ok(DownloadIndex::load(&index_path)?.cached_path(song_id))
    }

    fn select_audio_source(
        &self,
        cookie: &str,
        song: &Song,
        cancellation: &AtomicBool,
    ) -> Result<Option<AudioSource>, NeteaseError> {
        for quality in Quality::HIGHEST_FIRST
            .into_iter()
            .filter(|quality| *quality <= song.max_quality)
        {
            let source = retry_transient(|| {
                if cancellation.load(Ordering::Acquire) {
                    return Err(NeteaseError::Cancelled);
                }
                self.provider.audio_source(cookie, song.id, quality)
            })?;
            if source.is_some() {
                return Ok(source);
            }
        }
        self.provider.account(cookie)?;
        Ok(None)
    }

    fn fetch_and_tag(
        &self,
        song: &Song,
        source: &AudioSource,
        temporary: &Path,
        cancellation: &AtomicBool,
    ) -> Result<(), NeteaseError> {
        let result = (|| {
            let maximum = usize::try_from(source.size)
                .unwrap_or(usize::MAX)
                .saturating_add(1_048_576)
                .clamp(16 * 1_024 * 1_024, 1_024 * 1_024 * 1_024);
            let downloaded = retry_transient(|| {
                self.provider
                    .fetch_audio(&source.url, temporary, maximum, cancellation)
            })?;
            if cancellation.load(Ordering::Acquire) {
                return Err(NeteaseError::Cancelled);
            }
            if downloaded == 0 || (source.size > 0 && downloaded != source.size) {
                return Err(NeteaseError::Protocol(format!(
                    "downloaded audio size for song {} did not match the response",
                    song.id
                )));
            }

            let cover = song.cover_url.as_deref().map(|url| {
                retry_transient(|| {
                    if cancellation.load(Ordering::Acquire) {
                        return Err(NeteaseError::Cancelled);
                    }
                    self.provider.fetch_bytes(url, None, 20 * 1_024 * 1_024)
                })
            });
            let cover = match cover {
                Some(result) => Some(result?),
                None => None,
            };
            tag_audio(temporary, song, cover.as_deref())
        })();
        if result.is_err() {
            let _ = fs::remove_file(temporary);
        }
        result
    }
}
