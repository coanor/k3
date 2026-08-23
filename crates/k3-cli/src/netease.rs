use std::{
    collections::BTreeMap,
    fmt,
    fs::{self, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

const NETEASE_BASE_URL: &str = "https://music.163.com";
const NETEASE_USER_AGENT: &str = "Mozilla/5.0 (K3 experimental NetEase source)";
const NETEASE_WEAPI_USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) \
AppleWebKit/605.1.15 (KHTML, like Gecko) NeteaseMusicDesktop/3.0.12.2443";
const NETEASE_WEAPI_COOKIE_CONTEXT: &str = "channel=appstore; ntes_kaola_ad=1; WEVNSM=1.0; \
appver=3.0.12; os=osx; osver=15.3.2; mode=MacBookPro16,1; _iuqxldmzr_=33; \
__remember_me=true";

use serde::{Deserialize, Serialize};

mod weapi;

#[derive(Debug, thiserror::Error)]
pub enum NeteaseError {
    #[error("local NetEase data error: {0}")]
    Io(#[from] io::Error),
    #[error("invalid local NetEase data: {0}")]
    Json(#[from] serde_json::Error),
    #[error("NetEase request failed: {0}")]
    Http(String),
    #[error("NetEase protocol error: {0}")]
    Protocol(String),
    #[error("NetEase login is required")]
    LoginRequired,
    #[error("cannot import NetEase login from Chrome: {0}")]
    ChromeLogin(String),
    #[error("cannot write NetEase audio metadata: {0}")]
    Metadata(String),
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NeteaseSession {
    cookie: String,
    user_id: u64,
    nickname: String,
}

impl NeteaseSession {
    pub fn nickname(&self) -> &str {
        &self.nickname
    }
}

impl fmt::Debug for NeteaseSession {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NeteaseSession")
            .field("cookie", &"[REDACTED]")
            .field("user_id", &self.user_id)
            .field("nickname", &self.nickname)
            .finish()
    }
}

#[derive(Clone, Debug)]
pub struct SessionStore {
    path: PathBuf,
}

impl SessionStore {
    pub fn at(path: PathBuf) -> Self {
        Self { path }
    }

    #[cfg(test)]
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load(&self) -> Result<Option<NeteaseSession>, NeteaseError> {
        match fs::read(&self.path) {
            Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    pub fn save(&self, session: &NeteaseSession) -> Result<(), NeteaseError> {
        write_private_json(&self.path, session)
    }

    pub fn clear(&self) -> Result<(), NeteaseError> {
        match fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }
}

#[derive(Clone, Debug)]
pub struct RiskStore {
    path: PathBuf,
}

#[derive(Default, Deserialize, Serialize)]
struct LocalPreferences {
    #[serde(default)]
    unofficial_source_accepted: bool,
}

impl RiskStore {
    pub fn at(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn accepted(&self) -> Result<bool, NeteaseError> {
        match fs::read(&self.path) {
            Ok(bytes) => {
                Ok(serde_json::from_slice::<LocalPreferences>(&bytes)?.unofficial_source_accepted)
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error.into()),
        }
    }

    pub fn accept(&self) -> Result<(), NeteaseError> {
        write_private_json(
            &self.path,
            &LocalPreferences {
                unofficial_source_accepted: true,
            },
        )
    }
}

pub fn local_stores() -> Result<(SessionStore, RiskStore), NeteaseError> {
    let directory = platform_config_dir()?.join("k3");
    Ok((
        SessionStore::at(directory.join("netease-session.json")),
        RiskStore::at(directory.join("netease-preferences.json")),
    ))
}

fn platform_config_dir() -> Result<PathBuf, NeteaseError> {
    #[cfg(windows)]
    if let Some(path) = std::env::var_os("APPDATA").filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(path));
    }

    #[cfg(target_os = "macos")]
    if let Some(path) = std::env::var_os("HOME").filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(path).join("Library/Application Support"));
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        if let Some(path) = std::env::var_os("XDG_CONFIG_HOME").filter(|value| !value.is_empty()) {
            return Ok(PathBuf::from(path));
        }
        if let Some(path) = std::env::var_os("HOME").filter(|value| !value.is_empty()) {
            return Ok(PathBuf::from(path).join(".config"));
        }
    }

    Err(io::Error::new(
        io::ErrorKind::NotFound,
        "cannot determine the K3 configuration directory",
    )
    .into())
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LoginTicket {
    key: String,
    qr_url: String,
}

impl LoginTicket {
    fn new(key: impl Into<String>, qr_url: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            qr_url: qr_url.into(),
        }
    }

    #[cfg(test)]
    pub fn qr_url(&self) -> &str {
        &self.qr_url
    }

    pub fn qr_lines(&self) -> Result<Vec<String>, NeteaseError> {
        use qrcode::render::unicode;

        let code = qrcode::QrCode::new(self.qr_url.as_bytes())
            .map_err(|error| NeteaseError::Protocol(format!("cannot render QR code: {error}")))?;
        Ok(code
            .render::<unicode::Dense1x2>()
            .quiet_zone(true)
            .build()
            .lines()
            .map(str::to_owned)
            .collect())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthorizedLogin {
    cookie: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LoginPoll {
    WaitingScan,
    WaitingConfirmation,
    Expired,
    Authorized(AuthorizedLogin),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LoginStatus {
    WaitingScan,
    WaitingConfirmation,
    Expired,
    LoggedIn,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AccountProfile {
    user_id: u64,
    nickname: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct BrowserCookie {
    name: String,
    value: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ChromeCookieProfile {
    id: String,
}

trait ChromeCookieSource: Send + Sync {
    fn profiles(&self) -> Result<Vec<ChromeCookieProfile>, NeteaseError>;
    fn cookies(&self, profile: &ChromeCookieProfile) -> Result<Vec<BrowserCookie>, NeteaseError>;
}

struct RookieChromeCookieSource;

impl ChromeCookieSource for RookieChromeCookieSource {
    fn profiles(&self) -> Result<Vec<ChromeCookieProfile>, NeteaseError> {
        rookie_cookies::chrome_profiles()
            .map(|descriptors| {
                descriptors
                    .into_iter()
                    .map(|descriptor| ChromeCookieProfile {
                        id: descriptor.profile.profile_id.to_string(),
                    })
                    .collect()
            })
            .map_err(|error| {
                NeteaseError::ChromeLogin(format!("Chrome profiles could not be read: {error}"))
            })
    }

    fn cookies(&self, profile: &ChromeCookieProfile) -> Result<Vec<BrowserCookie>, NeteaseError> {
        let report =
            rookie_cookies::chrome_profile(&profile.id, Some(vec!["music.163.com".to_owned()]))
                .map_err(|error| {
                    NeteaseError::ChromeLogin(format!(
                        "a Chrome profile could not be read: {error}"
                    ))
                })?;
        Ok(report
            .profiles
            .into_iter()
            .flat_map(|profile| profile.sources)
            .filter(|source| {
                source.selected
                    && source.status == rookie_cookies::report::SourceStatusCode::succeeded()
            })
            .flat_map(|source| source.cookies)
            .filter(|cookie| matches!(cookie.name.as_str(), "MUSIC_U" | "__csrf"))
            .map(|cookie| BrowserCookie {
                name: cookie.name,
                value: cookie.value,
            })
            .collect())
    }
}

trait NeteaseProvider: Send + Sync {
    fn begin_login(&self) -> Result<LoginTicket, NeteaseError>;
    fn poll_login(&self, key: &str) -> Result<LoginPoll, NeteaseError>;
    fn account(&self, cookie: &str) -> Result<AccountProfile, NeteaseError>;
    fn search(
        &self,
        _cookie: &str,
        _query: &str,
        _offset: usize,
        _limit: usize,
    ) -> Result<SongPage, NeteaseError> {
        Err(NeteaseError::Protocol(
            "this NetEase provider does not support search".into(),
        ))
    }
    fn liked_songs(&self, _cookie: &str, _user_id: u64) -> Result<Vec<Song>, NeteaseError> {
        Err(NeteaseError::Protocol(
            "this NetEase provider does not support liked songs".into(),
        ))
    }
    fn audio_source(
        &self,
        _cookie: &str,
        _song_id: u64,
        _quality: Quality,
    ) -> Result<Option<AudioSource>, NeteaseError> {
        Err(NeteaseError::Protocol(
            "this NetEase provider does not support downloads".into(),
        ))
    }
    fn fetch_bytes(
        &self,
        _url: &str,
        _cookie: Option<&str>,
        _maximum: usize,
    ) -> Result<Vec<u8>, NeteaseError> {
        Err(NeteaseError::Protocol(
            "this NetEase provider cannot fetch media".into(),
        ))
    }
    fn fetch_audio(
        &self,
        url: &str,
        destination: &Path,
        maximum: usize,
    ) -> Result<u64, NeteaseError> {
        let audio = self.fetch_bytes(url, None, maximum)?;
        write_bytes(destination, &audio)?;
        Ok(audio.len() as u64)
    }
}

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

    fn with_provider(
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

    fn import_chrome_session_from(
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
    ) -> Result<DownloadOutcome, NeteaseError> {
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
        let Some(source) = self.select_audio_source(&session.cookie, song)? else {
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
        self.fetch_and_tag(song, &source, &paths.temporary)?;
        commit_download(&mut index, song, &source, &paths)?;
        Ok(DownloadOutcome::Downloaded {
            path: paths.destination,
            quality: source.quality,
        })
    }

    fn select_audio_source(
        &self,
        cookie: &str,
        song: &Song,
    ) -> Result<Option<AudioSource>, NeteaseError> {
        for quality in Quality::HIGHEST_FIRST
            .into_iter()
            .filter(|quality| *quality <= song.max_quality)
        {
            let source = retry_transient(|| self.provider.audio_source(cookie, song.id, quality))?;
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
    ) -> Result<(), NeteaseError> {
        let result = (|| {
            let maximum = usize::try_from(source.size)
                .unwrap_or(usize::MAX)
                .saturating_add(1_048_576)
                .clamp(16 * 1_024 * 1_024, 1_024 * 1_024 * 1_024);
            let downloaded =
                retry_transient(|| self.provider.fetch_audio(&source.url, temporary, maximum))?;
            if downloaded == 0 || (source.size > 0 && downloaded != source.size) {
                return Err(NeteaseError::Protocol(format!(
                    "downloaded audio size for song {} did not match the response",
                    song.id
                )));
            }

            let cover = song.cover_url.as_deref().map(|url| {
                retry_transient(|| self.provider.fetch_bytes(url, None, 20 * 1_024 * 1_024))
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

fn chrome_cookie_header(cookies: &[BrowserCookie]) -> Option<String> {
    const COOKIE_NAMES: [&str; 2] = ["MUSIC_U", "__csrf"];

    let mut values = BTreeMap::new();
    for cookie in cookies {
        if COOKIE_NAMES.contains(&cookie.name.as_str()) && safe_cookie_value(&cookie.value) {
            values.insert(cookie.name.as_str(), cookie.value.as_str());
        }
    }
    values.get("MUSIC_U")?;
    Some(
        COOKIE_NAMES
            .into_iter()
            .filter_map(|name| values.get(name).map(|value| format!("{name}={value}")))
            .collect::<Vec<_>>()
            .join("; "),
    )
}

fn safe_cookie_value(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte > b' ' && byte != b';' && byte != 0x7f)
}

struct DownloadPaths {
    filename: String,
    previous: Option<PathBuf>,
    destination: PathBuf,
    temporary: PathBuf,
    backup: PathBuf,
}

impl DownloadPaths {
    fn new(
        output_root: &Path,
        song: &Song,
        source: &AudioSource,
        decision: &DownloadDecision,
    ) -> Self {
        let base_filename = download_filename(
            song.primary_artist(),
            &song.title,
            &source.extension,
            song.id,
        );
        let mut filename = base_filename.clone();
        let previous = match decision {
            DownloadDecision::Upgrade(path) => Some(output_root.join(path)),
            DownloadDecision::Download | DownloadDecision::Skip => None,
        };
        let mut destination = output_root.join(&filename);
        if !download_destination_available(&destination, previous.as_ref()) {
            let filename_with_id = filename_with_id(&base_filename, song.id);
            filename.clone_from(&filename_with_id);
            destination = output_root.join(&filename);
            let mut collision = 2;
            while !download_destination_available(&destination, previous.as_ref()) {
                filename = filename_with_collision_index(&filename_with_id, collision);
                destination = output_root.join(&filename);
                collision += 1;
            }
        }
        Self {
            filename,
            previous,
            destination,
            temporary: output_root.join(format!(
                ".{}-{}.part.{}",
                std::process::id(),
                song.id,
                sanitize_extension(&source.extension)
            )),
            backup: output_root.join(format!(".{}-{}.backup", std::process::id(), song.id)),
        }
    }
}

fn download_destination_available(destination: &Path, previous: Option<&PathBuf>) -> bool {
    !destination.exists() || previous.is_some_and(|path| path == destination)
}

fn commit_download(
    index: &mut DownloadIndex,
    song: &Song,
    source: &AudioSource,
    paths: &DownloadPaths,
) -> Result<(), NeteaseError> {
    let backed_up = paths.destination.exists();
    if backed_up {
        let _ = fs::remove_file(&paths.backup);
        fs::rename(&paths.destination, &paths.backup)?;
    }
    if let Err(error) = fs::rename(&paths.temporary, &paths.destination) {
        if backed_up {
            let _ = fs::rename(&paths.backup, &paths.destination);
        }
        return Err(error.into());
    }

    index.record(DownloadRecord {
        song_id: song.id,
        quality: source.quality,
        path: PathBuf::from(&paths.filename),
    });
    if let Err(error) = index.save() {
        let _ = fs::remove_file(&paths.destination);
        if backed_up {
            let _ = fs::rename(&paths.backup, &paths.destination);
        }
        return Err(error);
    }
    if backed_up {
        let _ = fs::remove_file(&paths.backup);
    }
    if let Some(previous) = &paths.previous
        && *previous != paths.destination
    {
        let _ = fs::remove_file(previous);
    }
    Ok(())
}

struct WebNeteaseProvider {
    agent: ureq::Agent,
    base_url: String,
}

impl WebNeteaseProvider {
    fn new() -> Self {
        Self::at_base_url(NETEASE_BASE_URL)
    }

    fn at_base_url(base_url: impl Into<String>) -> Self {
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(std::time::Duration::from_secs(30)))
            .build()
            .into();
        Self {
            agent,
            base_url: base_url.into(),
        }
    }
}

#[derive(Deserialize)]
struct UniqueKeyResponse {
    code: i32,
    unikey: Option<String>,
}

#[derive(Deserialize)]
struct LoginPollResponse {
    code: i32,
    cookie: Option<String>,
    message: Option<String>,
}

#[derive(Deserialize)]
struct AccountResponse {
    profile: Option<WebAccountProfile>,
}

#[derive(Deserialize)]
struct WebAccountProfile {
    #[serde(rename = "userId")]
    user_id: u64,
    nickname: String,
}

#[derive(Deserialize)]
struct SearchResponse {
    code: i32,
    result: Option<SearchResult>,
}

#[derive(Deserialize)]
struct SearchResult {
    #[serde(default)]
    songs: Vec<SearchSong>,
    #[serde(rename = "songCount", default)]
    song_count: usize,
}

#[derive(Deserialize)]
struct SearchSong {
    id: u64,
}

#[derive(Deserialize)]
struct UserPlaylistsResponse {
    code: i32,
    #[serde(default)]
    playlist: Vec<WebPlaylist>,
}

#[derive(Deserialize)]
struct WebPlaylist {
    id: u64,
    #[serde(rename = "specialType")]
    special_type: i32,
}

#[derive(Deserialize)]
struct PlaylistDetailResponse {
    code: i32,
    playlist: Option<PlaylistDetail>,
}

#[derive(Deserialize)]
struct PlaylistDetail {
    #[serde(rename = "trackIds", default)]
    track_ids: Vec<TrackId>,
}

#[derive(Deserialize)]
struct TrackId {
    id: u64,
}

#[derive(Deserialize)]
struct SongDetailResponse {
    code: i32,
    #[serde(default)]
    songs: Vec<WebSong>,
}

#[derive(Deserialize)]
struct WebSong {
    id: u64,
    name: String,
    status: i32,
    #[serde(default)]
    artists: Vec<WebArtist>,
    album: WebAlbum,
    #[serde(rename = "hrMusic")]
    hi_res: Option<WebAudioQuality>,
    #[serde(rename = "sqMusic")]
    lossless: Option<WebAudioQuality>,
    #[serde(rename = "hMusic")]
    high: Option<WebAudioQuality>,
    #[serde(rename = "mMusic")]
    medium: Option<WebAudioQuality>,
}

#[derive(Deserialize)]
struct WebArtist {
    name: String,
}

#[derive(Deserialize)]
struct WebAlbum {
    name: String,
    #[serde(rename = "picUrl")]
    pic_url: Option<String>,
}

#[derive(Deserialize)]
struct WebAudioQuality {}

#[derive(Deserialize)]
struct DownloadUrlResponse {
    code: i32,
    data: Option<WebAudioSource>,
}

#[derive(Serialize)]
struct DownloadUrlRequest<'a> {
    id: &'a str,
    br: &'a str,
}

#[derive(Deserialize)]
struct WebAudioSource {
    #[serde(default)]
    code: i32,
    url: Option<String>,
    #[serde(rename = "type")]
    extension: Option<String>,
    #[serde(default)]
    size: u64,
    level: Option<Quality>,
}

impl NeteaseProvider for WebNeteaseProvider {
    fn begin_login(&self) -> Result<LoginTicket, NeteaseError> {
        let timestamp = now_millis().to_string();
        let mut response = self
            .agent
            .get(format!("{NETEASE_BASE_URL}/api/login/qrcode/unikey"))
            .header("User-Agent", NETEASE_USER_AGENT)
            .header("Referer", NETEASE_BASE_URL)
            .query("type", "1")
            .query("timestamp", &timestamp)
            .call()
            .map_err(http_error)?;
        let body: UniqueKeyResponse = response.body_mut().read_json().map_err(body_error)?;
        if body.code != 200 {
            return Err(NeteaseError::Protocol(format!(
                "QR login initialization returned code {}",
                body.code
            )));
        }
        let key = body
            .unikey
            .ok_or_else(|| NeteaseError::Protocol("QR login key is missing".into()))?;
        let qr_url = format!("{NETEASE_BASE_URL}/login?codekey={key}");
        Ok(LoginTicket::new(key, qr_url))
    }

    fn poll_login(&self, key: &str) -> Result<LoginPoll, NeteaseError> {
        let timestamp = now_millis().to_string();
        let mut response = self
            .agent
            .get(format!("{NETEASE_BASE_URL}/api/login/qrcode/client/login"))
            .header("User-Agent", NETEASE_USER_AGENT)
            .header("Referer", NETEASE_BASE_URL)
            .query("key", key)
            .query("type", "1")
            .query("timestamp", &timestamp)
            .call()
            .map_err(http_error)?;
        let body: LoginPollResponse = response.body_mut().read_json().map_err(body_error)?;
        match body.code {
            800 => Ok(LoginPoll::Expired),
            801 => Ok(LoginPoll::WaitingScan),
            802 => Ok(LoginPoll::WaitingConfirmation),
            803 => body.cookie.map_or_else(
                || {
                    Err(NeteaseError::Protocol(
                        "authorized QR login did not return a cookie".into(),
                    ))
                },
                |cookie| Ok(LoginPoll::Authorized(AuthorizedLogin { cookie })),
            ),
            code => Err(NeteaseError::Protocol(format!(
                "QR login returned code {code}: {}",
                body.message.unwrap_or_else(|| "unknown response".into())
            ))),
        }
    }

    fn account(&self, cookie: &str) -> Result<AccountProfile, NeteaseError> {
        let mut response = self
            .agent
            .get(format!("{NETEASE_BASE_URL}/api/nuser/account/get"))
            .header("User-Agent", NETEASE_USER_AGENT)
            .header("Referer", NETEASE_BASE_URL)
            .header("Cookie", cookie)
            .call()
            .map_err(http_error)?;
        let body: AccountResponse = response.body_mut().read_json().map_err(body_error)?;
        let profile = body.profile.ok_or(NeteaseError::LoginRequired)?;
        Ok(AccountProfile {
            user_id: profile.user_id,
            nickname: profile.nickname,
        })
    }

    fn search(
        &self,
        cookie: &str,
        query: &str,
        offset: usize,
        limit: usize,
    ) -> Result<SongPage, NeteaseError> {
        let offset_text = offset.to_string();
        let limit_text = limit.to_string();
        let mut response = self
            .agent
            .get(format!("{NETEASE_BASE_URL}/api/search/get/web"))
            .header("User-Agent", NETEASE_USER_AGENT)
            .header("Referer", NETEASE_BASE_URL)
            .header("Cookie", cookie)
            .query("s", query)
            .query("type", "1")
            .query("offset", &offset_text)
            .query("limit", &limit_text)
            .query("total", "true")
            .call()
            .map_err(http_error)?;
        let body: SearchResponse = response.body_mut().read_json().map_err(body_error)?;
        if body.code == 301 {
            return Err(NeteaseError::LoginRequired);
        }
        if body.code != 200 {
            return Err(NeteaseError::Protocol(format!(
                "song search returned code {}",
                body.code
            )));
        }
        let result = body.result.unwrap_or(SearchResult {
            songs: Vec::new(),
            song_count: 0,
        });
        let ids = result
            .songs
            .into_iter()
            .map(|song| song.id)
            .collect::<Vec<_>>();
        Ok(SongPage {
            songs: self.song_details(cookie, &ids)?,
            total: result.song_count,
            offset,
        })
    }

    fn liked_songs(&self, cookie: &str, user_id: u64) -> Result<Vec<Song>, NeteaseError> {
        let user_id = user_id.to_string();
        let mut response = self
            .agent
            .get(format!("{NETEASE_BASE_URL}/api/user/playlist/"))
            .header("User-Agent", NETEASE_USER_AGENT)
            .header("Referer", NETEASE_BASE_URL)
            .header("Cookie", cookie)
            .query("uid", &user_id)
            .query("offset", "0")
            .query("limit", "100")
            .call()
            .map_err(http_error)?;
        let body: UserPlaylistsResponse = response.body_mut().read_json().map_err(body_error)?;
        if body.code == 301 {
            return Err(NeteaseError::LoginRequired);
        }
        if body.code != 200 {
            return Err(NeteaseError::Protocol(format!(
                "user playlists returned code {}",
                body.code
            )));
        }
        let playlist_id = body
            .playlist
            .into_iter()
            .find(|playlist| playlist.special_type == 5)
            .map(|playlist| playlist.id)
            .ok_or_else(|| NeteaseError::Protocol("liked songs playlist is missing".into()))?;
        let playlist_id = playlist_id.to_string();
        let mut response = self
            .agent
            .get(format!("{NETEASE_BASE_URL}/api/v6/playlist/detail"))
            .header("User-Agent", NETEASE_USER_AGENT)
            .header("Referer", NETEASE_BASE_URL)
            .header("Cookie", cookie)
            .query("id", &playlist_id)
            .query("n", "100000")
            .query("s", "0")
            .call()
            .map_err(http_error)?;
        let body: PlaylistDetailResponse = response.body_mut().read_json().map_err(body_error)?;
        if body.code == 301 {
            return Err(NeteaseError::LoginRequired);
        }
        if body.code != 200 {
            return Err(NeteaseError::Protocol(format!(
                "liked songs playlist returned code {}",
                body.code
            )));
        }
        let ids = body
            .playlist
            .ok_or_else(|| NeteaseError::Protocol("liked songs playlist is empty".into()))?
            .track_ids
            .into_iter()
            .map(|track| track.id)
            .collect::<Vec<_>>();
        self.song_details(cookie, &ids)
    }

    fn audio_source(
        &self,
        cookie: &str,
        song_id: u64,
        quality: Quality,
    ) -> Result<Option<AudioSource>, NeteaseError> {
        let song_id_text = song_id.to_string();
        let bitrate = quality.download_bitrate().to_string();
        let encrypted = weapi::encrypt(&DownloadUrlRequest {
            id: &song_id_text,
            br: &bitrate,
        })
        .map_err(|error| {
            NeteaseError::Protocol(format!("cannot encrypt WEAPI request: {error}"))
        })?;
        let mut request = self
            .agent
            .post(format!("{}/weapi/song/enhance/download/url", self.base_url))
            .header("User-Agent", NETEASE_WEAPI_USER_AGENT)
            .header("Referer", NETEASE_BASE_URL)
            .header("Cookie", weapi_cookie_header(cookie));
        if let Some(csrf) = cookie_header_value(cookie, "__csrf") {
            request = request.query("csrf_token", csrf);
        }
        let mut response = request
            .send_form([
                ("params", encrypted.params.as_str()),
                ("encSecKey", encrypted.enc_sec_key.as_str()),
            ])
            .map_err(http_error)?;
        let body: DownloadUrlResponse = response.body_mut().read_json().map_err(body_error)?;
        if body.code == 301 {
            return Err(NeteaseError::LoginRequired);
        }
        if body.code != 200 {
            return Err(NeteaseError::Protocol(format!(
                "audio URL returned code {}",
                body.code
            )));
        }
        let Some(audio) = body.data else {
            return Ok(None);
        };
        if !matches!(audio.code, 0 | 200) {
            return Ok(None);
        }
        let Some(url) = audio.url else {
            return Ok(None);
        };
        Ok(Some(AudioSource {
            url,
            quality: audio.level.unwrap_or(quality),
            extension: audio.extension.unwrap_or_else(|| "mp3".into()),
            size: audio.size,
        }))
    }

    fn fetch_bytes(
        &self,
        url: &str,
        cookie: Option<&str>,
        maximum: usize,
    ) -> Result<Vec<u8>, NeteaseError> {
        let mut request = self
            .agent
            .get(url)
            .header("User-Agent", NETEASE_USER_AGENT)
            .header("Referer", NETEASE_BASE_URL);
        if let Some(cookie) = cookie {
            request = request.header("Cookie", cookie);
        }
        let mut response = request.call().map_err(http_error)?;
        response
            .body_mut()
            .with_config()
            .limit(maximum as u64)
            .read_to_vec()
            .map_err(body_error)
    }

    fn fetch_audio(
        &self,
        url: &str,
        destination: &Path,
        maximum: usize,
    ) -> Result<u64, NeteaseError> {
        let mut response = self
            .agent
            .get(url)
            .header("User-Agent", NETEASE_USER_AGENT)
            .header("Referer", NETEASE_BASE_URL)
            .call()
            .map_err(http_error)?;
        let mut reader = response
            .body_mut()
            .with_config()
            .limit(maximum.saturating_add(1) as u64)
            .reader();
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(destination)?;
        let mut buffer = vec![0_u8; 64 * 1_024].into_boxed_slice();
        let mut downloaded = 0_u64;
        loop {
            let count = io::Read::read(&mut reader, &mut buffer)
                .map_err(|error| NeteaseError::Http(error.to_string()))?;
            if count == 0 {
                break;
            }
            downloaded = downloaded.saturating_add(count as u64);
            if downloaded > maximum as u64 {
                return Err(NeteaseError::Protocol(format!(
                    "media response exceeded {maximum} bytes"
                )));
            }
            file.write_all(&buffer[..count])?;
        }
        file.sync_all()?;
        Ok(downloaded)
    }
}

fn cookie_header_value<'a>(cookie: &'a str, name: &str) -> Option<&'a str> {
    cookie.split(';').find_map(|part| {
        let (candidate, value) = part.trim().split_once('=')?;
        (candidate == name).then_some(value)
    })
}

fn weapi_cookie_header(cookie: &str) -> String {
    let mut header = cookie.trim().trim_end_matches(';').to_owned();
    for default in NETEASE_WEAPI_COOKIE_CONTEXT.split(';').map(str::trim) {
        let name = default.split_once('=').map_or(default, |(name, _)| name);
        if cookie_header_value(cookie, name).is_none() {
            if !header.is_empty() {
                header.push_str("; ");
            }
            header.push_str(default);
        }
    }
    header
}

impl WebNeteaseProvider {
    fn song_details(&self, cookie: &str, ids: &[u64]) -> Result<Vec<Song>, NeteaseError> {
        let mut songs_by_id = BTreeMap::new();
        for chunk in ids.chunks(200) {
            let ids_json = serde_json::to_string(chunk)?;
            let mut response = self
                .agent
                .get(format!("{NETEASE_BASE_URL}/api/song/detail"))
                .header("User-Agent", NETEASE_USER_AGENT)
                .header("Referer", NETEASE_BASE_URL)
                .header("Cookie", cookie)
                .query("ids", &ids_json)
                .call()
                .map_err(http_error)?;
            let body: SongDetailResponse = response.body_mut().read_json().map_err(body_error)?;
            if body.code == 301 {
                return Err(NeteaseError::LoginRequired);
            }
            if body.code != 200 {
                return Err(NeteaseError::Protocol(format!(
                    "song details returned code {}",
                    body.code
                )));
            }
            for song in body.songs {
                songs_by_id.insert(song.id, song.into());
            }
        }
        Ok(ids.iter().filter_map(|id| songs_by_id.remove(id)).collect())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Song {
    pub id: u64,
    pub title: String,
    pub artists: Vec<String>,
    pub album: String,
    pub cover_url: Option<String>,
    pub max_quality: Quality,
    pub available: bool,
}

impl Song {
    pub fn primary_artist(&self) -> &str {
        self.artists
            .first()
            .map_or("Unknown Artist", String::as_str)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SongPage {
    pub songs: Vec<Song>,
    pub total: usize,
    pub offset: usize,
}

impl From<WebSong> for Song {
    fn from(song: WebSong) -> Self {
        let max_quality = if song.hi_res.is_some() {
            Quality::HiRes
        } else if song.lossless.is_some() {
            Quality::Lossless
        } else if song.high.is_some() {
            Quality::ExHigh
        } else if song.medium.is_some() {
            Quality::Higher
        } else {
            Quality::Standard
        };
        Self {
            id: song.id,
            title: song.name,
            artists: song.artists.into_iter().map(|artist| artist.name).collect(),
            album: song.album.name,
            cover_url: song.album.pic_url,
            max_quality,
            available: song.status == 0,
        }
    }
}

fn now_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn http_error(error: ureq::Error) -> NeteaseError {
    match error {
        ureq::Error::StatusCode(code) if code < 500 && !matches!(code, 408 | 429) => {
            NeteaseError::Protocol(format!("request returned HTTP {code}"))
        }
        error => NeteaseError::Http(error.to_string()),
    }
}

fn body_error(error: ureq::Error) -> NeteaseError {
    let message = error.to_string();
    drop(error);
    NeteaseError::Http(message)
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Quality {
    Standard,
    Higher,
    ExHigh,
    Lossless,
    HiRes,
    Jyeffect,
    Sky,
    Jymaster,
}

impl Quality {
    pub const HIGHEST_FIRST: [Self; 8] = [
        Self::Jymaster,
        Self::Sky,
        Self::Jyeffect,
        Self::HiRes,
        Self::Lossless,
        Self::ExHigh,
        Self::Higher,
        Self::Standard,
    ];

    pub const fn as_level(self) -> &'static str {
        match self {
            Self::Standard => "standard",
            Self::Higher => "higher",
            Self::ExHigh => "exhigh",
            Self::Lossless => "lossless",
            Self::HiRes => "hires",
            Self::Jyeffect => "jyeffect",
            Self::Sky => "sky",
            Self::Jymaster => "jymaster",
        }
    }

    const fn download_bitrate(self) -> u64 {
        match self {
            Self::Standard => 128_000,
            Self::Higher => 192_000,
            Self::ExHigh => 320_000,
            Self::Lossless => 999_000,
            Self::HiRes | Self::Jyeffect | Self::Sky | Self::Jymaster => 9_999_999,
        }
    }
}

impl fmt::Display for Quality {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_level())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AudioSource {
    url: String,
    quality: Quality,
    extension: String,
    size: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DownloadOutcome {
    Downloaded { path: PathBuf, quality: Quality },
    Skipped { song_id: u64, quality: Quality },
    Unavailable { song_id: u64, reason: String },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DownloadRecord {
    pub song_id: u64,
    pub quality: Quality,
    pub path: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DownloadDecision {
    Download,
    Skip,
    Upgrade(PathBuf),
}

#[derive(Debug, Default, Deserialize, Serialize)]
struct DownloadIndexData {
    #[serde(default)]
    songs: BTreeMap<u64, DownloadRecord>,
}

#[derive(Debug)]
pub struct DownloadIndex {
    path: PathBuf,
    data: DownloadIndexData,
}

impl DownloadIndex {
    pub fn load(path: &Path) -> Result<Self, NeteaseError> {
        let data = match fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => DownloadIndexData::default(),
            Err(error) => return Err(error.into()),
        };
        Ok(Self {
            path: path.to_path_buf(),
            data,
        })
    }

    pub fn decision(&self, song_id: u64, quality: Quality) -> DownloadDecision {
        let Some(existing) = self.data.songs.get(&song_id) else {
            return DownloadDecision::Download;
        };
        let Some(filename) = existing.path.file_name() else {
            return DownloadDecision::Download;
        };
        if existing.path != Path::new(filename)
            || !self
                .path
                .parent()
                .is_some_and(|parent| parent.join(filename).is_file())
        {
            return DownloadDecision::Download;
        }
        if existing.quality >= quality {
            DownloadDecision::Skip
        } else {
            DownloadDecision::Upgrade(existing.path.clone())
        }
    }

    pub fn record(&mut self, record: DownloadRecord) {
        self.data.songs.insert(record.song_id, record);
    }

    pub fn save(&self) -> Result<(), NeteaseError> {
        write_private_json(&self.path, &self.data)
    }
}

pub fn download_filename(primary_artist: &str, title: &str, extension: &str, _id: u64) -> String {
    let artist = sanitize_filename_part(primary_artist);
    let title = sanitize_filename_part(title);
    let extension = sanitize_extension(extension);
    format!("{artist}-{title}.{extension}")
}

fn sanitize_extension(value: &str) -> String {
    let extension = value
        .trim_start_matches('.')
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(8)
        .collect::<String>()
        .to_ascii_lowercase();
    if extension.is_empty() {
        "mp3".into()
    } else {
        extension
    }
}

fn filename_with_id(filename: &str, id: u64) -> String {
    let path = Path::new(filename);
    let stem = path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("Song");
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("mp3");
    format!("{stem}[{id}].{extension}")
}

fn filename_with_collision_index(filename: &str, collision: usize) -> String {
    let path = Path::new(filename);
    let stem = path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("Song");
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("mp3");
    format!("{stem}-{collision}.{extension}")
}

fn retry_transient<T>(
    mut operation: impl FnMut() -> Result<T, NeteaseError>,
) -> Result<T, NeteaseError> {
    let mut delay = std::time::Duration::from_millis(200);
    for attempt in 0..4 {
        match operation() {
            Ok(value) => return Ok(value),
            Err(NeteaseError::Http(_)) if attempt < 3 => {
                std::thread::sleep(delay);
                delay = delay.saturating_mul(2);
            }
            Err(error) => return Err(error),
        }
    }
    unreachable!("the retry loop always returns")
}

fn write_bytes(path: &Path, bytes: &[u8]) -> Result<(), NeteaseError> {
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

fn tag_audio(path: &Path, song: &Song, cover: Option<&[u8]>) -> Result<(), NeteaseError> {
    use lofty::{
        config::WriteOptions,
        file::{AudioFile, TaggedFileExt},
        picture::{Picture, PictureType},
        prelude::{Accessor, ItemKey},
        tag::{ItemValue, Tag, TagItem},
    };

    let mut tagged =
        lofty::read_from_path(path).map_err(|error| NeteaseError::Metadata(error.to_string()))?;
    let tag_type = tagged.primary_tag_type();
    if tagged.tag(tag_type).is_none() {
        tagged.insert_tag(Tag::new(tag_type));
    }
    let tag = tagged
        .tag_mut(tag_type)
        .ok_or_else(|| NeteaseError::Metadata("audio format has no writable tag".into()))?;
    tag.set_title(song.title.clone());
    tag.set_artist(song.artists.join(", "));
    tag.set_album(song.album.clone());
    tag.insert(TagItem::new(
        ItemKey::CatalogNumber,
        ItemValue::Text(format!("netease:{}", song.id)),
    ));
    if let Some(cover) = cover {
        let mut picture = Picture::from_reader(&mut std::io::Cursor::new(cover))
            .map_err(|error| NeteaseError::Metadata(error.to_string()))?;
        picture.set_pic_type(PictureType::CoverFront);
        tag.push_picture(picture);
    }
    tagged
        .save_to_path(path, WriteOptions::default())
        .map_err(|error| NeteaseError::Metadata(error.to_string()))
}

fn sanitize_filename_part(value: &str) -> String {
    let sanitized = value
        .chars()
        .map(|character| {
            if character.is_control()
                || matches!(
                    character,
                    '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*'
                )
            {
                '_'
            } else {
                character
            }
        })
        .collect::<String>();
    let sanitized = sanitized.trim().trim_end_matches(['.', ' ']);
    if sanitized.is_empty() {
        "Unknown".to_owned()
    } else {
        sanitized.to_owned()
    }
}

fn write_private_json<T: Serialize>(path: &Path, value: &T) -> Result<(), NeteaseError> {
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "NetEase data path has no parent",
        )
    })?;
    fs::create_dir_all(parent)?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    serde_json::to_writer_pretty(temporary.as_file_mut(), value)?;
    temporary.as_file_mut().write_all(b"\n")?;
    temporary.as_file_mut().sync_all()?;
    temporary
        .persist(path)
        .map(|_| ())
        .map_err(|error| NeteaseError::Io(error.error))
}

#[cfg(test)]
mod tests {
    use super::{
        AccountProfile, AudioSource, AuthorizedLogin, BrowserCookie, ChromeCookieProfile,
        ChromeCookieSource, DownloadDecision, DownloadIndex, DownloadOutcome, DownloadPaths,
        DownloadRecord, LoginPoll, LoginStatus, LoginTicket, NeteaseClient, NeteaseError,
        NeteaseProvider, NeteaseSession, Quality, SessionStore, Song, SongPage, download_filename,
    };
    use std::{
        fs,
        io::{Read, Write},
        net::TcpListener,
        path::Path,
        sync::{Arc, Mutex},
        thread,
    };

    struct LoginProvider;

    impl NeteaseProvider for LoginProvider {
        fn begin_login(&self) -> Result<LoginTicket, NeteaseError> {
            Ok(LoginTicket::new("ticket", "https://example.test/qr"))
        }

        fn poll_login(&self, _key: &str) -> Result<LoginPoll, NeteaseError> {
            Ok(LoginPoll::Authorized(AuthorizedLogin {
                cookie: "MUSIC_U=secret".into(),
            }))
        }

        fn account(&self, _cookie: &str) -> Result<AccountProfile, NeteaseError> {
            Ok(AccountProfile {
                user_id: 42,
                nickname: "Singer".into(),
            })
        }
    }

    struct ChromeLoginProvider {
        accepted_cookie: String,
        seen_cookies: Mutex<Vec<String>>,
    }

    impl NeteaseProvider for ChromeLoginProvider {
        fn begin_login(&self) -> Result<LoginTicket, NeteaseError> {
            unreachable!()
        }

        fn poll_login(&self, _key: &str) -> Result<LoginPoll, NeteaseError> {
            unreachable!()
        }

        fn account(&self, cookie: &str) -> Result<AccountProfile, NeteaseError> {
            self.seen_cookies.lock().unwrap().push(cookie.to_owned());
            if cookie != self.accepted_cookie {
                return Err(NeteaseError::LoginRequired);
            }
            Ok(AccountProfile {
                user_id: 42,
                nickname: "Singer".into(),
            })
        }
    }

    struct FakeChromeCookieSource {
        profiles: Vec<(ChromeCookieProfile, Vec<BrowserCookie>)>,
    }

    impl ChromeCookieSource for FakeChromeCookieSource {
        fn profiles(&self) -> Result<Vec<ChromeCookieProfile>, NeteaseError> {
            Ok(self
                .profiles
                .iter()
                .map(|(profile, _)| ChromeCookieProfile {
                    id: profile.id.clone(),
                })
                .collect())
        }

        fn cookies(
            &self,
            profile: &ChromeCookieProfile,
        ) -> Result<Vec<BrowserCookie>, NeteaseError> {
            Ok(self
                .profiles
                .iter()
                .find(|(candidate, _)| candidate.id == profile.id)
                .map_or_else(Vec::new, |(_, cookies)| cookies.clone()))
        }
    }

    fn chrome_profile(
        name: &str,
        cookies: &[(&str, &str)],
    ) -> (ChromeCookieProfile, Vec<BrowserCookie>) {
        (
            ChromeCookieProfile { id: name.into() },
            cookies
                .iter()
                .map(|(name, value)| BrowserCookie {
                    name: (*name).into(),
                    value: (*value).into(),
                })
                .collect(),
        )
    }

    struct OfflineAccountProvider;

    impl NeteaseProvider for OfflineAccountProvider {
        fn begin_login(&self) -> Result<LoginTicket, NeteaseError> {
            unreachable!()
        }

        fn poll_login(&self, _key: &str) -> Result<LoginPoll, NeteaseError> {
            unreachable!()
        }

        fn account(&self, _cookie: &str) -> Result<AccountProfile, NeteaseError> {
            Err(NeteaseError::Http("offline".into()))
        }
    }

    struct CatalogProvider;

    impl NeteaseProvider for CatalogProvider {
        fn begin_login(&self) -> Result<LoginTicket, NeteaseError> {
            unreachable!()
        }

        fn poll_login(&self, _key: &str) -> Result<LoginPoll, NeteaseError> {
            unreachable!()
        }

        fn account(&self, _cookie: &str) -> Result<AccountProfile, NeteaseError> {
            unreachable!()
        }

        fn search(
            &self,
            _cookie: &str,
            query: &str,
            offset: usize,
            _limit: usize,
        ) -> Result<SongPage, NeteaseError> {
            assert_eq!(query, "Beyond");
            assert_eq!(offset, 0);
            Ok(SongPage {
                songs: vec![song(7, "海阔天空")],
                total: 1,
                offset,
            })
        }

        fn liked_songs(&self, _cookie: &str, user_id: u64) -> Result<Vec<Song>, NeteaseError> {
            assert_eq!(user_id, 42);
            Ok(vec![song(8, "光辉岁月")])
        }
    }

    fn song(id: u64, title: &str) -> Song {
        Song {
            id,
            title: title.into(),
            artists: vec!["Beyond".into()],
            album: "精选".into(),
            cover_url: Some("https://example.test/cover.jpg".into()),
            max_quality: Quality::Lossless,
            available: true,
        }
    }

    struct DownloadProvider {
        audio: Vec<u8>,
    }

    impl NeteaseProvider for DownloadProvider {
        fn begin_login(&self) -> Result<LoginTicket, NeteaseError> {
            unreachable!()
        }

        fn poll_login(&self, _key: &str) -> Result<LoginPoll, NeteaseError> {
            unreachable!()
        }

        fn account(&self, _cookie: &str) -> Result<AccountProfile, NeteaseError> {
            unreachable!()
        }

        fn audio_source(
            &self,
            _cookie: &str,
            song_id: u64,
            quality: Quality,
        ) -> Result<Option<AudioSource>, NeteaseError> {
            assert_eq!(song_id, 7);
            if quality == Quality::Lossless {
                Ok(Some(AudioSource {
                    url: "https://example.test/audio".into(),
                    quality,
                    extension: "wav".into(),
                    size: self.audio.len() as u64,
                }))
            } else {
                Ok(None)
            }
        }

        fn fetch_bytes(
            &self,
            url: &str,
            cookie: Option<&str>,
            _maximum: usize,
        ) -> Result<Vec<u8>, NeteaseError> {
            assert!(
                cookie.is_none(),
                "media requests must not receive the login cookie"
            );
            match url {
                "https://example.test/audio" => Ok(self.audio.clone()),
                "https://example.test/cover.jpg" => Ok(vec![
                    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49,
                    0x48, 0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06,
                    0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4, 0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44,
                    0x41, 0x54, 0x08, 0xd7, 0x63, 0xf8, 0xcf, 0xc0, 0x00, 0x00, 0x03, 0x01, 0x01,
                    0x00, 0x18, 0xdd, 0x8d, 0xb1, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44,
                    0xae, 0x42, 0x60, 0x82,
                ]),
                _ => panic!("unexpected URL: {url}"),
            }
        }
    }

    struct ExpiredDownloadProvider;

    impl NeteaseProvider for ExpiredDownloadProvider {
        fn begin_login(&self) -> Result<LoginTicket, NeteaseError> {
            unreachable!()
        }

        fn poll_login(&self, _key: &str) -> Result<LoginPoll, NeteaseError> {
            unreachable!()
        }

        fn account(&self, _cookie: &str) -> Result<AccountProfile, NeteaseError> {
            Err(NeteaseError::LoginRequired)
        }

        fn audio_source(
            &self,
            _cookie: &str,
            _song_id: u64,
            _quality: Quality,
        ) -> Result<Option<AudioSource>, NeteaseError> {
            Ok(None)
        }
    }

    struct FailingStreamProvider {
        attempts: Arc<std::sync::atomic::AtomicUsize>,
    }

    impl NeteaseProvider for FailingStreamProvider {
        fn begin_login(&self) -> Result<LoginTicket, NeteaseError> {
            unreachable!()
        }

        fn poll_login(&self, _key: &str) -> Result<LoginPoll, NeteaseError> {
            unreachable!()
        }

        fn account(&self, _cookie: &str) -> Result<AccountProfile, NeteaseError> {
            unreachable!()
        }

        fn audio_source(
            &self,
            _cookie: &str,
            _song_id: u64,
            quality: Quality,
        ) -> Result<Option<AudioSource>, NeteaseError> {
            Ok((quality == Quality::Lossless).then(|| AudioSource {
                url: "https://example.test/audio".into(),
                quality,
                extension: "wav".into(),
                size: 100,
            }))
        }

        fn fetch_audio(
            &self,
            _url: &str,
            destination: &Path,
            _maximum: usize,
        ) -> Result<u64, NeteaseError> {
            self.attempts
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            fs::write(destination, b"partial audio")?;
            Err(NeteaseError::Http("stream interrupted".into()))
        }
    }

    fn wav_bytes() -> Vec<u8> {
        let sandbox = tempfile::tempdir().unwrap();
        let path = sandbox.path().join("fixture.wav");
        let mut writer = hound::WavWriter::create(
            &path,
            hound::WavSpec {
                channels: 1,
                sample_rate: 8_000,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            },
        )
        .unwrap();
        writer.write_sample::<i16>(0).unwrap();
        writer.finalize().unwrap();
        fs::read(path).unwrap()
    }

    #[test]
    fn download_uses_highest_available_quality_tags_and_deduplicates() {
        use lofty::prelude::{Accessor, TaggedFileExt};

        let sandbox = tempfile::tempdir().unwrap();
        let store = SessionStore::at(sandbox.path().join("session.json"));
        store
            .save(&NeteaseSession {
                cookie: "MUSIC_U=secret".into(),
                user_id: 42,
                nickname: "Singer".into(),
            })
            .unwrap();
        let client =
            NeteaseClient::with_provider(Arc::new(DownloadProvider { audio: wav_bytes() }), store);
        let music_root = sandbox.path().join("music");
        fs::create_dir_all(&music_root).unwrap();
        let track = song(7, "海阔天空");

        let outcome = client.download_song(&music_root, &track).unwrap();
        let DownloadOutcome::Downloaded { path, quality } = outcome else {
            panic!("expected a downloaded song")
        };
        assert_eq!(quality, Quality::Lossless);
        assert_eq!(path.file_name().unwrap(), "Beyond-海阔天空.wav");
        let tagged = lofty::read_from_path(&path).unwrap();
        let tag = tagged.primary_tag().unwrap();
        assert_eq!(tag.title().as_deref(), Some("海阔天空"));
        assert_eq!(tag.artist().as_deref(), Some("Beyond"));
        assert_eq!(tag.album().as_deref(), Some("精选"));
        assert_eq!(tag.pictures().len(), 1);

        assert!(matches!(
            client.download_song(&music_root, &track).unwrap(),
            DownloadOutcome::Skipped { .. }
        ));

        fs::remove_file(&path).unwrap();
        assert!(matches!(
            client.download_song(&music_root, &track).unwrap(),
            DownloadOutcome::Downloaded { .. }
        ));
    }

    #[test]
    fn download_paths_keep_both_existing_collision_candidates_intact() {
        let sandbox = tempfile::tempdir().unwrap();
        let output_root = sandbox.path();
        let base = output_root.join("Beyond-Song.wav");
        let with_id = output_root.join("Beyond-Song[7].wav");
        fs::write(&base, b"base file").unwrap();
        fs::write(&with_id, b"id file").unwrap();
        let source = AudioSource {
            url: "https://example.test/audio".into(),
            quality: Quality::Lossless,
            extension: "wav".into(),
            size: 1,
        };

        let paths = DownloadPaths::new(
            output_root,
            &song(7, "Song"),
            &source,
            &DownloadDecision::Download,
        );

        assert_eq!(
            paths.destination.file_name().unwrap(),
            "Beyond-Song[7]-2.wav"
        );
        assert_eq!(fs::read(base).unwrap(), b"base file");
        assert_eq!(fs::read(with_id).unwrap(), b"id file");
        assert!(!paths.destination.exists());
    }

    #[test]
    fn failed_stream_retries_and_removes_the_partial_audio_file() {
        let sandbox = tempfile::tempdir().unwrap();
        let store = SessionStore::at(sandbox.path().join("session.json"));
        store
            .save(&NeteaseSession {
                cookie: "MUSIC_U=secret".into(),
                user_id: 42,
                nickname: "Singer".into(),
            })
            .unwrap();
        let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let client = NeteaseClient::with_provider(
            Arc::new(FailingStreamProvider {
                attempts: Arc::clone(&attempts),
            }),
            store,
        );
        let music_root = sandbox.path().join("music");

        assert!(matches!(
            client.download_song(&music_root, &song(7, "Song")),
            Err(NeteaseError::Http(message)) if message == "stream interrupted"
        ));
        assert_eq!(attempts.load(std::sync::atomic::Ordering::Relaxed), 4);
        let output_root = music_root.join("NetEase");
        assert!(fs::read_dir(output_root).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains(".part.")
        }));
    }

    #[test]
    fn unavailable_audio_revalidates_the_session_before_reporting_rights_failure() {
        let sandbox = tempfile::tempdir().unwrap();
        let store = SessionStore::at(sandbox.path().join("session.json"));
        store
            .save(&NeteaseSession {
                cookie: "expired-cookie".into(),
                user_id: 42,
                nickname: "Singer".into(),
            })
            .unwrap();
        let client = NeteaseClient::with_provider(Arc::new(ExpiredDownloadProvider), store);

        assert!(matches!(
            client.download_song(sandbox.path(), &song(7, "Song")),
            Err(NeteaseError::LoginRequired)
        ));
    }

    #[test]
    fn logged_in_user_can_search_tracks_and_load_liked_songs() {
        let sandbox = tempfile::tempdir().unwrap();
        let store = SessionStore::at(sandbox.path().join("session.json"));
        store
            .save(&NeteaseSession {
                cookie: "MUSIC_U=secret".into(),
                user_id: 42,
                nickname: "Singer".into(),
            })
            .unwrap();
        let client = NeteaseClient::with_provider(Arc::new(CatalogProvider), store);

        let results = client.search("Beyond", 0, 50).unwrap();
        assert_eq!(results.total, 1);
        assert_eq!(results.songs[0].title, "海阔天空");
        assert_eq!(client.liked_songs().unwrap()[0].title, "光辉岁月");
    }

    #[test]
    fn catalog_requires_a_saved_login() {
        let sandbox = tempfile::tempdir().unwrap();
        let client = NeteaseClient::with_provider(
            Arc::new(CatalogProvider),
            SessionStore::at(sandbox.path().join("missing.json")),
        );

        assert!(matches!(
            client.search("Beyond", 0, 50),
            Err(NeteaseError::LoginRequired)
        ));
    }

    #[test]
    fn authorized_qr_login_persists_a_redacted_session() {
        let sandbox = tempfile::tempdir().unwrap();
        let store = SessionStore::at(sandbox.path().join("session.json"));
        let client = NeteaseClient::with_provider(Arc::new(LoginProvider), store.clone());

        let ticket = client.begin_login().unwrap();
        assert_eq!(ticket.qr_url(), "https://example.test/qr");
        assert_eq!(client.poll_login(&ticket).unwrap(), LoginStatus::LoggedIn);
        assert_eq!(client.session().unwrap().unwrap().nickname, "Singer");
        assert!(!format!("{:?}", client.session().unwrap().unwrap()).contains("secret"));
        assert!(store.path().is_file());
    }

    #[test]
    fn chrome_login_filters_credentials_validates_profiles_and_persists_the_first_valid_one() {
        let sandbox = tempfile::tempdir().unwrap();
        let store = SessionStore::at(sandbox.path().join("session.json"));
        let provider = Arc::new(ChromeLoginProvider {
            accepted_cookie: "MUSIC_U=valid; __csrf=token".into(),
            seen_cookies: Mutex::new(Vec::new()),
        });
        let client = NeteaseClient::with_provider(provider.clone(), store.clone());
        let source = FakeChromeCookieSource {
            profiles: vec![
                chrome_profile("Profile 1", &[("MUSIC_U", "expired")]),
                chrome_profile(
                    "Default",
                    &[
                        ("tracking_cookie", "must-not-leave-browser"),
                        ("__csrf", "token"),
                        ("MUSIC_U", "valid"),
                    ],
                ),
            ],
        };

        let session = client.import_chrome_session_from(&source).unwrap();

        assert_eq!(session.nickname(), "Singer");
        assert_eq!(
            provider.seen_cookies.lock().unwrap().as_slice(),
            ["MUSIC_U=expired", "MUSIC_U=valid; __csrf=token"]
        );
        assert_eq!(store.load().unwrap(), Some(session));
    }

    #[test]
    fn chrome_login_rejects_unsafe_cookie_values_without_contacting_netease() {
        let sandbox = tempfile::tempdir().unwrap();
        let store = SessionStore::at(sandbox.path().join("session.json"));
        let provider = Arc::new(ChromeLoginProvider {
            accepted_cookie: "never".into(),
            seen_cookies: Mutex::new(Vec::new()),
        });
        let client = NeteaseClient::with_provider(provider.clone(), store.clone());
        let source = FakeChromeCookieSource {
            profiles: vec![chrome_profile(
                "Default",
                &[("MUSIC_U", "secret\r\nInjected: value")],
            )],
        };

        assert!(matches!(
            client.import_chrome_session_from(&source),
            Err(NeteaseError::ChromeLogin(_))
        ));
        assert!(provider.seen_cookies.lock().unwrap().is_empty());
        assert_eq!(store.load().unwrap(), None);
    }

    #[test]
    fn chrome_login_preserves_account_service_failures_instead_of_calling_them_rejections() {
        let sandbox = tempfile::tempdir().unwrap();
        let store = SessionStore::at(sandbox.path().join("session.json"));
        let client = NeteaseClient::with_provider(Arc::new(OfflineAccountProvider), store.clone());
        let source = FakeChromeCookieSource {
            profiles: vec![chrome_profile("Default", &[("MUSIC_U", "valid")])],
        };

        assert!(matches!(
            client.import_chrome_session_from(&source),
            Err(NeteaseError::Http(message)) if message == "offline"
        ));
        assert_eq!(store.load().unwrap(), None);
    }

    #[test]
    fn session_round_trips_and_logout_removes_the_credential() {
        let sandbox = tempfile::tempdir().unwrap();
        let store = SessionStore::at(sandbox.path().join("session.json"));
        let session = NeteaseSession {
            cookie: "MUSIC_U=secret; __csrf=token".into(),
            user_id: 42,
            nickname: "Singer".into(),
        };

        store.save(&session).unwrap();
        assert_eq!(store.load().unwrap(), Some(session));

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(store.path()).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }

        store.clear().unwrap();
        assert_eq!(store.load().unwrap(), None);
    }

    #[test]
    fn risk_acceptance_is_persisted_separately_from_the_session() {
        let sandbox = tempfile::tempdir().unwrap();
        let risk = super::RiskStore::at(sandbox.path().join("preferences.json"));
        let session = SessionStore::at(sandbox.path().join("session.json"));

        assert!(!risk.accepted().unwrap());
        risk.accept().unwrap();
        assert!(risk.accepted().unwrap());
        assert_eq!(session.load().unwrap(), None);
    }

    #[test]
    fn retries_transient_failures_but_not_deterministic_protocol_errors() {
        let mut transient_attempts = 0;
        let value = super::retry_transient(|| {
            transient_attempts += 1;
            if transient_attempts < 3 {
                Err(NeteaseError::Http("temporary failure".into()))
            } else {
                Ok("recovered")
            }
        })
        .unwrap();
        assert_eq!(value, "recovered");
        assert_eq!(transient_attempts, 3);

        let mut deterministic_attempts = 0;
        assert!(matches!(
            super::retry_transient::<()>(|| {
                deterministic_attempts += 1;
                Err(NeteaseError::Protocol("not available".into()))
            }),
            Err(NeteaseError::Protocol(_))
        ));
        assert_eq!(deterministic_attempts, 1);
    }

    #[test]
    fn web_provider_uses_the_download_endpoint_with_the_requested_quality_bitrate() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = Vec::new();
            let mut buffer = [0_u8; 4_096];
            loop {
                let count = stream.read(&mut buffer).unwrap();
                assert!(count > 0, "client closed before sending the request body");
                request.extend_from_slice(&buffer[..count]);
                let Some(header_end) = request
                    .windows(4)
                    .position(|window| window == b"\r\n\r\n")
                    .map(|position| position + 4)
                else {
                    continue;
                };
                let headers = String::from_utf8_lossy(&request[..header_end]);
                let content_length = headers
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().unwrap())
                    })
                    .unwrap_or_default();
                if request.len() >= header_end + content_length {
                    break;
                }
            }
            let body = r#"{"code":200,"data":{"code":200,"url":"https://cdn.example.test/song.flac","type":"flac","size":22961285,"level":"lossless"}}"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
            String::from_utf8_lossy(&request).into_owned()
        });
        let provider = super::WebNeteaseProvider::at_base_url(base_url);

        let source = provider
            .audio_source("MUSIC_U=secret", 185_726, Quality::Lossless)
            .unwrap()
            .unwrap();
        let request = server.join().unwrap();

        assert!(request.starts_with("POST /weapi/song/enhance/download/url HTTP/1.1"));
        assert!(request.contains("content-type: application/x-www-form-urlencoded"));
        assert!(request.contains("params="));
        assert!(request.contains("encSecKey="));
        assert!(!request.contains("id=185726"));
        assert!(!request.contains("br=999000"));
        assert!(!request.contains("player/url"));
        assert_eq!(source.url, "https://cdn.example.test/song.flac");
        assert_eq!(source.quality, Quality::Lossless);
        assert_eq!(source.extension, "flac");
        assert_eq!(source.size, 22_961_285);
    }

    #[test]
    fn weapi_cookie_context_preserves_values_from_the_saved_session() {
        let header =
            super::weapi_cookie_header("MUSIC_U=secret; __csrf=token; os=pc; appver=9.9.9");

        assert!(header.contains("MUSIC_U=secret"));
        assert!(header.contains("__csrf=token"));
        assert!(header.contains("os=pc"));
        assert!(header.contains("appver=9.9.9"));
        assert!(!header.contains("os=osx"));
        assert!(!header.contains("appver=3.0.12"));
        assert!(header.contains("channel=appstore"));
        assert!(header.contains("__remember_me=true"));
    }

    #[test]
    #[ignore = "contacts the live undocumented NetEase endpoint"]
    fn live_weapi_download_url_returns_fetchable_media() {
        let session_path = std::env::var_os("K3_NETEASE_SESSION_PATH")
            .expect("K3_NETEASE_SESSION_PATH must point to a saved session");
        let session = SessionStore::at(session_path.into())
            .load()
            .unwrap()
            .expect("the saved session must exist");
        let provider = super::WebNeteaseProvider::new();

        let source = provider
            .audio_source(&session.cookie, 301_448, Quality::Lossless)
            .unwrap()
            .expect("翩翩飞起 should have a lossless source");
        let response = provider
            .agent
            .get(&source.url)
            .header("User-Agent", super::NETEASE_USER_AGENT)
            .header("Referer", super::NETEASE_BASE_URL)
            .call()
            .unwrap();

        assert_eq!(response.status(), 200);
        assert_eq!(
            response
                .headers()
                .get("Content-Length")
                .unwrap()
                .to_str()
                .unwrap()
                .parse::<u64>()
                .unwrap(),
            source.size
        );
    }

    #[test]
    #[ignore = "contacts the live undocumented NetEase endpoint"]
    fn live_endpoint_can_issue_a_qr_login_ticket() {
        let provider = super::WebNeteaseProvider::new();
        let ticket = provider.begin_login().unwrap();

        assert!(!ticket.qr_lines().unwrap().is_empty());
    }

    #[test]
    fn download_index_skips_equal_quality_and_replaces_lower_quality() {
        let sandbox = tempfile::tempdir().unwrap();
        let path = sandbox.path().join("index.json");
        let mut index = DownloadIndex::load(&path).unwrap();
        assert_eq!(
            index.decision(7, Quality::Lossless),
            DownloadDecision::Download
        );

        index.record(DownloadRecord {
            song_id: 7,
            quality: Quality::Lossless,
            path: "Artist-Song.flac".into(),
        });
        fs::write(sandbox.path().join("Artist-Song.flac"), b"audio").unwrap();
        index.save().unwrap();

        let loaded = DownloadIndex::load(&path).unwrap();
        assert_eq!(loaded.decision(7, Quality::ExHigh), DownloadDecision::Skip);
        assert_eq!(
            loaded.decision(7, Quality::Lossless),
            DownloadDecision::Skip
        );
        assert_eq!(
            loaded.decision(7, Quality::HiRes),
            DownloadDecision::Upgrade("Artist-Song.flac".into())
        );

        let mut tampered = DownloadIndex::load(&sandbox.path().join("tampered.json")).unwrap();
        tampered.record(DownloadRecord {
            song_id: 8,
            quality: Quality::Lossless,
            path: "../outside.flac".into(),
        });
        assert_eq!(
            tampered.decision(8, Quality::HiRes),
            DownloadDecision::Download
        );
    }

    #[test]
    fn download_filename_is_flat_portable_and_only_uses_primary_artist() {
        assert_eq!(
            download_filename("Artist", "Song: Live?", "flac", 123),
            "Artist-Song_ Live_.flac"
        );
        assert_eq!(download_filename("A/B", "Same", "mp3", 456), "A_B-Same.mp3");
        assert_eq!(
            download_filename("Artist", "Song", "../../FLAC", 789),
            "Artist-Song.flac"
        );
    }
}
