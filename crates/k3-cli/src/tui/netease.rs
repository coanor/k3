use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    error::Error,
    fmt::Write as _,
    ops::Range,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, TryRecvError},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use crossterm::event::KeyCode;

use crate::netease::{
    DownloadOutcome, LoginStatus, NeteaseClient, NeteaseError, Quality, RiskStore, SessionStore,
    Song, SongPage,
};

use super::{MediaLibrary, display_name};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum NeteaseNoticeLevel {
    Info,
    Error,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct NeteaseNotice {
    level: NeteaseNoticeLevel,
    message: String,
}

impl NeteaseNotice {
    pub(super) fn info(message: impl Into<String>) -> Self {
        Self {
            level: NeteaseNoticeLevel::Info,
            message: message.into(),
        }
    }

    pub(super) fn error(message: impl Into<String>) -> Self {
        Self {
            level: NeteaseNoticeLevel::Error,
            message: message.into(),
        }
    }

    pub(super) const fn is_error(&self) -> bool {
        matches!(self.level, NeteaseNoticeLevel::Error)
    }

    pub(super) fn append_error(&mut self, message: impl AsRef<str>) {
        self.level = NeteaseNoticeLevel::Error;
        self.message.push('\n');
        self.message.push_str(message.as_ref());
    }
}

impl std::ops::Deref for NeteaseNotice {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        &self.message
    }
}

impl std::fmt::Display for NeteaseNotice {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl From<&str> for NeteaseNotice {
    fn from(message: &str) -> Self {
        Self::info(message)
    }
}

impl From<String> for NeteaseNotice {
    fn from(message: String) -> Self {
        Self::info(message)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AuthState {
    LoggedOut,
    LoggedIn,
}

pub(super) struct ManagedTask<T> {
    result: Receiver<T>,
    cancelled: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl<T: Send + 'static> ManagedTask<T> {
    pub(super) fn spawn(work: impl FnOnce(&AtomicBool) -> T + Send + 'static) -> Self {
        Self::spawn_stream(move |cancelled, sender| {
            let result = work(&cancelled);
            if !cancelled.load(Ordering::Acquire) {
                let _ = sender.send(result);
            }
        })
    }

    fn spawn_stream(work: impl FnOnce(Arc<AtomicBool>, mpsc::Sender<T>) + Send + 'static) -> Self {
        let cancelled = Arc::new(AtomicBool::new(false));
        let worker_cancelled = Arc::clone(&cancelled);
        let (sender, result) = mpsc::channel();
        let worker = thread::spawn(move || work(worker_cancelled, sender));
        Self {
            result,
            cancelled,
            worker: Some(worker),
        }
    }

    #[cfg(test)]
    pub(super) fn from_receiver(result: Receiver<T>) -> Self {
        Self {
            result,
            cancelled: Arc::new(AtomicBool::new(false)),
            worker: None,
        }
    }

    fn try_recv(&self) -> Result<T, TryRecvError> {
        self.result.try_recv()
    }

    fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }
}

impl<T> Drop for ManagedTask<T> {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum MusicSource {
    Local,
    Netease,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum NeteaseView {
    Liked,
    Search,
}

pub(super) enum NeteaseModal {
    Risk,
    Login(LoginJob),
    Search(String),
    ConfirmDownload,
}

pub(super) struct LoginJob {
    pub(super) qr_lines: Vec<String>,
    pub(super) status: String,
    task: ManagedTask<Result<LoginStatus, String>>,
    pub(super) started: Instant,
    pub(super) finished: bool,
}

pub(super) struct ChromeLoginJob {
    pub(super) task: ManagedTask<Result<(), String>>,
}

pub(super) enum CatalogResult {
    Liked(Vec<Song>),
    Search { query: String, page: SongPage },
}

pub(super) struct CatalogJob {
    pub(super) task: ManagedTask<Result<CatalogResult, NeteaseError>>,
}

pub(super) struct NeteaseDownloadJob {
    pub(super) song: Song,
    pub(super) task: ManagedTask<Result<DownloadOutcome, NeteaseError>>,
}

#[derive(Default)]
pub(super) struct DownloadSummary {
    pub(super) completed: usize,
    pub(super) skipped: usize,
    pub(super) failed: Vec<String>,
    pub(super) qualities: BTreeMap<Quality, usize>,
    pub(super) downgraded: usize,
}

pub(super) struct NeteasePanel {
    pub(super) client: NeteaseClient,
    pub(super) risk_store: RiskStore,
    pub(super) view: NeteaseView,
    pub(super) songs: Vec<Song>,
    pub(super) selected_row: usize,
    pub(super) selected_ids: BTreeSet<u64>,
    pub(super) page_offset: usize,
    pub(super) page_size: usize,
    pub(super) search_query: Option<String>,
    pub(super) modal: Option<NeteaseModal>,
    pub(super) chrome_login_job: Option<ChromeLoginJob>,
    pub(super) catalog_job: Option<CatalogJob>,
    pub(super) download_job: Option<NeteaseDownloadJob>,
    pub(super) download_queue: VecDeque<Song>,
    pub(super) download_summary: DownloadSummary,
    auth: AuthState,
}

impl NeteasePanel {
    pub(super) fn new(
        session_store: SessionStore,
        risk_store: RiskStore,
    ) -> Result<Self, NeteaseError> {
        let client = NeteaseClient::new(session_store);
        let auth = if client.session()?.is_some() {
            AuthState::LoggedIn
        } else {
            AuthState::LoggedOut
        };
        Ok(Self {
            client,
            risk_store,
            view: NeteaseView::Liked,
            songs: Vec::new(),
            selected_row: 0,
            selected_ids: BTreeSet::new(),
            page_offset: 0,
            page_size: 50,
            search_query: None,
            modal: None,
            chrome_login_job: None,
            catalog_job: None,
            download_job: None,
            download_queue: VecDeque::new(),
            download_summary: DownloadSummary::default(),
            auth,
        })
    }

    pub(super) fn is_logged_in(&self) -> bool {
        self.auth == AuthState::LoggedIn
    }

    pub(super) fn active_download(&self) -> bool {
        self.download_job.is_some() || !self.download_queue.is_empty()
    }

    pub(super) fn visible_range(&self) -> Range<usize> {
        let start = if self.view == NeteaseView::Liked {
            self.page_offset.min(self.songs.len())
        } else {
            0
        };
        start..(start + self.page_size).min(self.songs.len())
    }

    pub(super) fn visible_songs(&self) -> &[Song] {
        let range = self.visible_range();
        &self.songs[range]
    }

    pub(super) fn selected_song(&self) -> Option<&Song> {
        self.visible_songs().get(self.selected_row)
    }

    pub(super) fn start_login(&mut self) -> Result<(), NeteaseError> {
        if self.chrome_login_job.is_some() {
            return Ok(());
        }
        if let Some(NeteaseModal::Login(job)) = &self.modal {
            job.task.cancel();
        }
        let ticket = self.client.begin_login()?;
        let qr_lines = ticket.qr_lines()?;
        let client = self.client.clone();
        let task = ManagedTask::spawn_stream(move |cancelled, sender| {
            while !cancelled.load(Ordering::Acquire) {
                let status = client
                    .poll_login(&ticket)
                    .map_err(|error| error.to_string());
                let terminal = matches!(
                    status,
                    Ok(LoginStatus::LoggedIn | LoginStatus::Expired) | Err(_)
                );
                if sender.send(status).is_err() || terminal {
                    break;
                }
                thread::sleep(Duration::from_secs(1));
            }
        });
        self.modal = Some(NeteaseModal::Login(LoginJob {
            qr_lines,
            status: "Waiting for scan".into(),
            task,
            started: Instant::now(),
            finished: false,
        }));
        Ok(())
    }

    pub(super) fn start_chrome_login(&mut self) {
        if self.chrome_login_job.is_some() {
            return;
        }
        if let Some(NeteaseModal::Login(job)) = &self.modal {
            job.task.cancel();
        }
        self.modal = None;
        let client = self.client.clone();
        let task = ManagedTask::spawn(move |_| {
            client
                .import_chrome_session()
                .map(|_| ())
                .map_err(|error| error.to_string())
        });
        self.chrome_login_job = Some(ChromeLoginJob { task });
    }

    pub(super) fn resume_after_login(&mut self, music_root: &Path) {
        if self.download_queue.is_empty() {
            self.start_liked();
        } else {
            self.launch_next_download(music_root);
        }
    }

    pub(super) fn expire_session(&mut self) -> Result<(), NeteaseError> {
        self.client.logout()?;
        self.auth = AuthState::LoggedOut;
        if let Some(NeteaseModal::Login(job)) = &self.modal {
            job.task.cancel();
        }
        self.modal = None;
        Ok(())
    }

    pub(super) fn start_liked(&mut self) {
        if self.catalog_job.is_some() {
            return;
        }
        let client = self.client.clone();
        let task = ManagedTask::spawn(move |_| client.liked_songs().map(CatalogResult::Liked));
        self.catalog_job = Some(CatalogJob { task });
    }

    pub(super) fn start_search(&mut self, query: String, offset: usize) {
        if self.catalog_job.is_some() {
            return;
        }
        let client = self.client.clone();
        let task = ManagedTask::spawn(move |_| {
            client
                .search(&query, offset, 50)
                .map(|page| CatalogResult::Search { query, page })
        });
        self.catalog_job = Some(CatalogJob { task });
    }

    pub(super) fn toggle_current(&mut self) {
        if let Some(song) = self.selected_song() {
            let id = song.id;
            if !self.selected_ids.remove(&id) {
                self.selected_ids.insert(id);
            }
        }
    }

    pub(super) fn select_current_page(&mut self) {
        let ids = self
            .visible_songs()
            .iter()
            .map(|song| song.id)
            .collect::<Vec<_>>();
        self.selected_ids.extend(ids);
    }

    pub(super) fn select_all_liked(&mut self) {
        if self.view == NeteaseView::Liked {
            self.selected_ids
                .extend(self.songs.iter().map(|song| song.id));
        }
    }

    pub(super) fn queue_selected(&mut self, music_root: &Path) {
        self.download_queue = self
            .songs
            .iter()
            .filter(|song| self.selected_ids.contains(&song.id))
            .cloned()
            .collect();
        self.download_summary = DownloadSummary::default();
        self.launch_next_download(music_root);
    }

    pub(super) fn launch_next_download(&mut self, music_root: &Path) {
        if self.download_job.is_some() {
            return;
        }
        let Some(song) = self.download_queue.pop_front() else {
            return;
        };
        let worker_song = song.clone();
        let client = self.client.clone();
        let music_root = music_root.to_path_buf();
        let task = ManagedTask::spawn(move |cancelled| {
            client.download_song(&music_root, &worker_song, cancelled)
        });
        self.download_job = Some(NeteaseDownloadJob { song, task });
    }

    pub(super) fn cancel_all(&mut self) {
        self.download_queue.clear();
        self.modal = None;
        self.chrome_login_job = None;
        self.catalog_job = None;
        self.download_job = None;
    }
}

pub(super) fn poll_netease(library: &mut MediaLibrary) -> Result<(), Box<dyn Error>> {
    poll_netease_chrome_login(library)?;
    poll_netease_login(library)?;
    poll_netease_catalog(library)?;
    poll_netease_download(library)
}

pub(super) fn poll_netease_chrome_login(library: &mut MediaLibrary) -> Result<(), Box<dyn Error>> {
    let Some(panel) = &mut library.netease else {
        return Ok(());
    };
    let outcome = match panel
        .chrome_login_job
        .as_ref()
        .map(|job| job.task.try_recv())
    {
        Some(Ok(result)) => Some(result),
        Some(Err(TryRecvError::Disconnected)) => {
            Some(Err("Chrome login task exited unexpectedly".into()))
        }
        Some(Err(TryRecvError::Empty)) | None => None,
    };
    let Some(outcome) = outcome else {
        return Ok(());
    };
    panel.chrome_login_job = None;
    match outcome {
        Ok(()) => {
            let session = panel.client.session()?.ok_or_else(|| {
                NeteaseError::Protocol("Chrome login completed without a saved session".into())
            })?;
            panel.auth = AuthState::LoggedIn;
            library.netease_message = Some(NeteaseNotice::info(format!(
                "Logged in to NetEase as {} via Chrome",
                session.nickname()
            )));
            panel.resume_after_login(&library.config.music_root);
        }
        Err(error) => {
            library.netease_message = Some(NeteaseNotice::error(format!(
                "Chrome login failed: {error}"
            )));
        }
    }
    Ok(())
}

pub(super) fn poll_netease_login(library: &mut MediaLibrary) -> Result<(), Box<dyn Error>> {
    let Some(panel) = &mut library.netease else {
        return Ok(());
    };
    let login_event = if let Some(NeteaseModal::Login(job)) = &mut panel.modal
        && !job.finished
    {
        match job.task.try_recv() {
            Ok(status) => Some(status),
            Err(TryRecvError::Disconnected) => Some(Err("Login task exited unexpectedly".into())),
            Err(TryRecvError::Empty) => None,
        }
    } else {
        None
    };
    if let Some(event) = login_event {
        match event {
            Ok(LoginStatus::WaitingScan) => {
                if let Some(NeteaseModal::Login(job)) = &mut panel.modal {
                    job.status = "Waiting for scan".into();
                }
            }
            Ok(LoginStatus::WaitingConfirmation) => {
                if let Some(NeteaseModal::Login(job)) = &mut panel.modal {
                    job.status = "Scanned · confirm on your phone".into();
                }
            }
            Ok(LoginStatus::Expired) => {
                if let Some(NeteaseModal::Login(job)) = &mut panel.modal {
                    job.status = "QR code expired · press r to refresh".into();
                    job.finished = true;
                }
            }
            Ok(LoginStatus::LoggedIn) => {
                let session = panel.client.session()?.ok_or_else(|| {
                    NeteaseError::Protocol("QR login completed without a saved session".into())
                })?;
                panel.modal = None;
                panel.auth = AuthState::LoggedIn;
                library.netease_message = Some(NeteaseNotice::info(format!(
                    "Logged in to NetEase as {}",
                    session.nickname()
                )));
                panel.resume_after_login(&library.config.music_root);
            }
            Err(error) => {
                if let Some(NeteaseModal::Login(job)) = &mut panel.modal {
                    job.status = format!("Login failed: {error} · press c for Chrome · r to retry");
                    job.finished = true;
                }
            }
        }
    }
    Ok(())
}

pub(super) fn poll_netease_catalog(library: &mut MediaLibrary) -> Result<(), Box<dyn Error>> {
    let Some(panel) = &mut library.netease else {
        return Ok(());
    };
    let catalog_outcome = match panel.catalog_job.as_ref().map(|job| job.task.try_recv()) {
        Some(Ok(result)) => Some(result),
        Some(Err(TryRecvError::Disconnected)) => Some(Err(NeteaseError::Protocol(
            "catalog task exited unexpectedly".into(),
        ))),
        Some(Err(TryRecvError::Empty)) | None => None,
    };
    if let Some(outcome) = catalog_outcome {
        panel.catalog_job = None;
        match outcome {
            Ok(CatalogResult::Liked(songs)) => {
                let count = songs.len();
                panel.view = NeteaseView::Liked;
                panel.songs = songs;
                panel.selected_row = 0;
                panel.page_offset = 0;
                panel.search_query = None;
                panel.selected_ids.clear();
                library.netease_message =
                    Some(NeteaseNotice::info(format!("Loaded {count} liked songs")));
            }
            Ok(CatalogResult::Search { query, page }) => {
                let count = page.songs.len();
                let total = page.total;
                panel.view = NeteaseView::Search;
                panel.songs = page.songs;
                panel.selected_row = 0;
                panel.page_offset = page.offset;
                panel.search_query = Some(query);
                panel.selected_ids.clear();
                library.netease_message = Some(NeteaseNotice::info(format!(
                    "Search returned {count} of {total} songs"
                )));
            }
            Err(NeteaseError::LoginRequired) => {
                panel.expire_session()?;
                library.netease_message =
                    Some(NeteaseNotice::error(netease_session_expired_hint()));
            }
            Err(error) => {
                library.netease_message = Some(NeteaseNotice::error(format!(
                    "NetEase catalog failed: {error}"
                )));
            }
        }
    }
    Ok(())
}

pub(super) fn poll_netease_download(library: &mut MediaLibrary) -> Result<(), Box<dyn Error>> {
    let (downloaded_path, downloads_finished) = {
        let Some(panel) = &mut library.netease else {
            return Ok(());
        };
        let download_outcome = match panel.download_job.as_ref().map(|job| job.task.try_recv()) {
            Some(Ok(result)) => Some(result),
            Some(Err(TryRecvError::Disconnected)) => Some(Err(NeteaseError::Protocol(
                "download task exited unexpectedly".into(),
            ))),
            Some(Err(TryRecvError::Empty)) | None => None,
        };
        let Some(outcome) = download_outcome else {
            return Ok(());
        };
        let job = panel
            .download_job
            .take()
            .expect("download outcome has a job");
        let mut downloaded_path = None;
        match outcome {
            Ok(DownloadOutcome::Downloaded { path, quality }) => {
                panel.download_summary.completed += 1;
                *panel.download_summary.qualities.entry(quality).or_default() += 1;
                if quality < job.song.max_quality {
                    panel.download_summary.downgraded += 1;
                }
                library.netease_message = Some(NeteaseNotice::info(format!(
                    "Downloaded {} · {quality}: {} · project queued",
                    job.song.title,
                    path.display()
                )));
                downloaded_path = Some(path);
            }
            Ok(DownloadOutcome::Skipped { quality, .. }) => {
                panel.download_summary.skipped += 1;
                library.netease_message = Some(NeteaseNotice::info(format!(
                    "Skipped existing {} · {quality}",
                    job.song.title
                )));
            }
            Ok(DownloadOutcome::Unavailable { reason, .. }) => {
                panel
                    .download_summary
                    .failed
                    .push(format!("{}: {reason}", job.song.title));
                library.netease_message = Some(NeteaseNotice::error(format!(
                    "Skipped unavailable {}: {reason}",
                    job.song.title
                )));
            }
            Err(NeteaseError::LoginRequired) => {
                panel.download_queue.push_front(job.song);
                panel.expire_session()?;
                library.netease_message =
                    Some(NeteaseNotice::error(netease_session_expired_hint()));
                return Ok(());
            }
            Err(error) => {
                panel
                    .download_summary
                    .failed
                    .push(format!("{}: {error}", job.song.title));
                library.netease_message = Some(NeteaseNotice::error(format!(
                    "Download failed {}: {error}",
                    job.song.title
                )));
            }
        }
        (downloaded_path, panel.download_queue.is_empty())
    };

    let refreshed_after_download = if let Some(path) = downloaded_path {
        library.enqueue_downloaded_source(&path);
        library.refresh_after_netease_download();
        true
    } else {
        false
    };

    if library.job.is_none()
        && let Some(path) = library.start_next_queued()
    {
        library.message = Some(format!(
            "Creating project and separating: {}",
            display_name(&path)
        ));
    }

    if downloads_finished {
        let panel = library.netease.as_ref().expect("source is configured");
        library.netease_message = Some(format_netease_download_summary(&panel.download_summary));
        if !refreshed_after_download {
            library.refresh_after_netease_download();
        }
    } else if let Some(panel) = &mut library.netease {
        panel.launch_next_download(&library.config.music_root);
    }
    Ok(())
}

pub(super) fn format_netease_download_summary(summary: &DownloadSummary) -> NeteaseNotice {
    let failed = summary.failed.len();
    let qualities = summary
        .qualities
        .iter()
        .map(|(quality, count)| format!("{quality} {count}"))
        .collect::<Vec<_>>()
        .join(", ");
    let mut failure_details = summary
        .failed
        .iter()
        .take(3)
        .cloned()
        .collect::<Vec<_>>()
        .join(" | ");
    if failed > 3 {
        let _ = write!(failure_details, " | +{} more", failed - 3);
    }
    let message = format!(
        "NetEase downloads complete · {} downloaded · {} skipped · {failed} failed\n\
         Actual quality: {} · {} downgraded{}",
        summary.completed,
        summary.skipped,
        if qualities.is_empty() {
            "none"
        } else {
            &qualities
        },
        summary.downgraded,
        if failure_details.is_empty() {
            String::new()
        } else {
            format!("\nFailures: {failure_details}")
        }
    );
    if failed == 0 {
        NeteaseNotice::info(message)
    } else {
        NeteaseNotice::error(message)
    }
}

pub(super) fn toggle_music_source(library: &mut MediaLibrary) -> Result<(), NeteaseError> {
    library.music_source = match library.music_source {
        MusicSource::Local => MusicSource::Netease,
        MusicSource::Netease => MusicSource::Local,
    };
    if library.music_source == MusicSource::Netease {
        let panel = library.netease.as_mut().expect("source is configured");
        if !panel.risk_store.accepted()? {
            panel.modal = Some(NeteaseModal::Risk);
        } else if panel.is_logged_in() && panel.songs.is_empty() {
            panel.start_liked();
        }
    }
    Ok(())
}

pub(super) fn handle_netease_modal_key(
    library: &mut MediaLibrary,
    key: KeyCode,
) -> Result<(), Box<dyn Error>> {
    let Some(panel) = &mut library.netease else {
        return Ok(());
    };
    let Some(modal) = panel.modal.take() else {
        return Ok(());
    };
    match modal {
        NeteaseModal::Risk => match key {
            KeyCode::Char('y' | 'Y') => {
                panel.risk_store.accept()?;
                library.netease_message =
                    Some("NetEase experimental source enabled locally".into());
            }
            KeyCode::Char('n' | 'N') | KeyCode::Esc => {
                library.music_source = MusicSource::Local;
            }
            _ => panel.modal = Some(NeteaseModal::Risk),
        },
        NeteaseModal::Login(job) => match key {
            KeyCode::Esc => {
                job.task.cancel();
            }
            KeyCode::Char('c') => {
                job.task.cancel();
                panel.start_chrome_login();
                library.netease_message = Some("Importing NetEase login from Chrome...".into());
            }
            KeyCode::Char('r') => {
                job.task.cancel();
                panel.start_login()?;
            }
            _ => panel.modal = Some(NeteaseModal::Login(job)),
        },
        NeteaseModal::Search(mut query) => match key {
            KeyCode::Esc => {}
            KeyCode::Enter => {
                if query.trim().is_empty() {
                    library.netease_message = Some("Enter a search query".into());
                } else {
                    panel.start_search(query, 0);
                    library.netease_message = Some("Searching NetEase songs...".into());
                }
            }
            KeyCode::Backspace => {
                query.pop();
                panel.modal = Some(NeteaseModal::Search(query));
            }
            KeyCode::Char(character) if !character.is_control() => {
                query.push(character);
                panel.modal = Some(NeteaseModal::Search(query));
            }
            _ => panel.modal = Some(NeteaseModal::Search(query)),
        },
        NeteaseModal::ConfirmDownload => match key {
            KeyCode::Char('y' | 'Y') if panel.active_download() => {
                library.netease_message = Some(netease_download_busy_hint().into());
            }
            KeyCode::Char('y' | 'Y') => {
                panel.queue_selected(&library.config.music_root);
                library.netease_message = Some(NeteaseNotice::info(format!(
                    "Downloading {} selected NetEase songs...",
                    panel.selected_ids.len()
                )));
            }
            KeyCode::Char('n' | 'N') | KeyCode::Esc => {}
            _ => panel.modal = Some(NeteaseModal::ConfirmDownload),
        },
    }
    Ok(())
}

pub(super) fn handle_netease_source_key(
    library: &mut MediaLibrary,
    key: KeyCode,
) -> Result<(), Box<dyn Error>> {
    let Some(panel) = &mut library.netease else {
        return Ok(());
    };
    match key {
        KeyCode::Up => panel.selected_row = panel.selected_row.saturating_sub(1),
        KeyCode::Down => {
            panel.selected_row =
                (panel.selected_row + 1).min(panel.visible_songs().len().saturating_sub(1));
        }
        KeyCode::Left => {
            if panel.view == NeteaseView::Liked {
                panel.page_offset = panel.page_offset.saturating_sub(panel.page_size);
                panel.selected_row = 0;
            } else if let Some(query) = panel.search_query.clone() {
                let offset = panel.page_offset.saturating_sub(panel.page_size);
                panel.start_search(query, offset);
            }
        }
        KeyCode::Right => {
            if panel.view == NeteaseView::Liked {
                if panel.page_offset + panel.page_size < panel.songs.len() {
                    panel.page_offset += panel.page_size;
                    panel.selected_row = 0;
                }
            } else if panel.songs.len() == panel.page_size
                && let Some(query) = panel.search_query.clone()
            {
                panel.start_search(query, panel.page_offset + panel.page_size);
            }
        }
        KeyCode::Enter if panel.active_download() => {
            library.netease_message = Some(netease_download_busy_hint().into());
        }
        KeyCode::Char('c') if !panel.is_logged_in() => {
            panel.start_chrome_login();
            library.netease_message = Some("Importing NetEase login from Chrome...".into());
        }
        KeyCode::Char('i') if !panel.is_logged_in() && panel.chrome_login_job.is_none() => {
            panel.start_login()?;
        }
        KeyCode::Char('l') if panel.is_logged_in() => {
            panel.start_liked();
            library.netease_message = Some("Loading liked songs...".into());
        }
        KeyCode::Char('/') if panel.is_logged_in() => {
            panel.modal = Some(NeteaseModal::Search(String::new()));
        }
        KeyCode::Char('x') if panel.is_logged_in() && !panel.active_download() => {
            panel.client.logout()?;
            panel.auth = AuthState::LoggedOut;
            panel.catalog_job = None;
            panel.songs.clear();
            panel.selected_ids.clear();
            library.netease_message = Some("Logged out of NetEase".into());
        }
        KeyCode::Char('r') if panel.is_logged_in() => match panel.view {
            NeteaseView::Liked => panel.start_liked(),
            NeteaseView::Search => {
                if let Some(query) = panel.search_query.clone() {
                    panel.start_search(query, panel.page_offset);
                }
            }
        },
        KeyCode::Char(' ') if panel.is_logged_in() => panel.toggle_current(),
        KeyCode::Char('a') if panel.is_logged_in() => panel.select_current_page(),
        KeyCode::Char('A') if panel.is_logged_in() => {
            if panel.view == NeteaseView::Liked {
                panel.select_all_liked();
                library.netease_message = Some(NeteaseNotice::info(format!(
                    "Selected all {} liked songs · press Enter to review",
                    panel.selected_ids.len()
                )));
            } else {
                library.netease_message = Some("Press l before selecting all liked songs".into());
            }
        }
        KeyCode::Enter if panel.is_logged_in() && !panel.selected_ids.is_empty() => {
            panel.modal = Some(NeteaseModal::ConfirmDownload);
        }
        KeyCode::Enter if panel.is_logged_in() => {
            panel.toggle_current();
            if !panel.selected_ids.is_empty() {
                panel.modal = Some(NeteaseModal::ConfirmDownload);
            }
        }
        KeyCode::Char('c' | 'i' | 'l' | '/' | 'r' | ' ') | KeyCode::Enter => {
            library.netease_message = Some(netease_sign_in_hint().into());
        }
        _ => {}
    }
    Ok(())
}

mod view;

pub(super) use view::*;
