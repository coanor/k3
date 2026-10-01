use std::{
    collections::{HashSet, VecDeque},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread,
    time::Duration,
};

use k3::netease::{
    DownloadOutcome, LoginStatus, NeteaseClient, NeteaseError, RiskStore, Song, local_stores,
};
use qrcode::{Color, QrCode};
use slint::{ComponentHandle, Image, Model, ModelRc, Rgb8Pixel, SharedPixelBuffer, VecModel};

use super::{AppData, K3Window, NeteaseSongItem, separation};

struct NetEaseUi {
    client: NeteaseClient,
    risk: RiskStore,
    songs: Mutex<Vec<Song>>,
    selected_ids: Mutex<HashSet<u64>>,
    downloaded_ids: Mutex<HashSet<u64>>,
    downloads: Mutex<DownloadQueue>,
    generation: AtomicU64,
    cancelled: Arc<AtomicBool>,
}

#[derive(Clone)]
struct DownloadJob {
    song: Song,
    projects_root: PathBuf,
    profile_index: i32,
}

#[derive(Default)]
struct DownloadQueue {
    active: Option<DownloadJob>,
    queued: VecDeque<DownloadJob>,
    paused_for_login: bool,
    finished: usize,
    issues: Vec<String>,
}

impl DownloadQueue {
    fn enqueue(&mut self, job: DownloadJob) -> bool {
        if self
            .active
            .as_ref()
            .is_some_and(|active| active.song.id == job.song.id)
            || self
                .queued
                .iter()
                .any(|queued| queued.song.id == job.song.id)
        {
            return false;
        }
        self.queued.push_back(job);
        true
    }

    fn start_next(&mut self) -> Option<DownloadJob> {
        if self.active.is_some() || self.paused_for_login {
            return None;
        }
        let job = self.queued.pop_front()?;
        self.active = Some(job.clone());
        Some(job)
    }

    fn finish_active(&mut self) {
        self.active = None;
        self.finished += 1;
    }

    fn pause_active(&mut self) {
        if let Some(active) = self.active.take() {
            self.queued.push_front(active);
        }
        self.paused_for_login = true;
    }

    fn contains(&self, song_id: u64) -> bool {
        self.active
            .as_ref()
            .is_some_and(|job| job.song.id == song_id)
            || self.queued.iter().any(|job| job.song.id == song_id)
    }
}

pub(super) fn install(ui: &K3Window, data: &Arc<Mutex<AppData>>) -> Arc<AtomicBool> {
    let cancelled = Arc::new(AtomicBool::new(false));
    let (session_store, risk) = match local_stores() {
        Ok(stores) => stores,
        Err(error) => {
            ui.set_netease_message(format!("NetEase setup failed: {error}").into());
            return cancelled;
        }
    };
    let net = Arc::new(NetEaseUi {
        client: NeteaseClient::new(session_store),
        risk,
        songs: Mutex::new(Vec::new()),
        selected_ids: Mutex::new(HashSet::new()),
        downloaded_ids: Mutex::new(HashSet::new()),
        downloads: Mutex::new(DownloadQueue::default()),
        generation: AtomicU64::new(0),
        cancelled: Arc::clone(&cancelled),
    });
    match net.risk.accepted() {
        Ok(accepted) => ui.set_netease_accepted(accepted),
        Err(error) => ui.set_netease_message(format!("NetEase setup failed: {error}").into()),
    }
    match net.client.session() {
        Ok(Some(session)) => {
            ui.set_netease_logged_in(true);
            ui.set_netease_account(session.nickname().into());
        }
        Ok(None) => {}
        Err(error) => ui.set_netease_message(format!("NetEase session failed: {error}").into()),
    }

    install_risk_callback(ui, &net);
    install_chrome_login(ui, &net, data);
    install_qr_login(ui, &net, data);
    install_logout(ui, &net);
    install_catalog_callbacks(ui, &net, data);
    install_selection_callback(ui, &net);
    install_download_callback(ui, &net, data);
    cancelled
}

fn install_risk_callback(ui: &K3Window, net: &Arc<NetEaseUi>) {
    {
        let weak = ui.as_weak();
        let net = Arc::clone(net);
        ui.on_accept_netease_risk(move || {
            if let Some(ui) = weak.upgrade() {
                match net.risk.accept() {
                    Ok(()) => {
                        ui.set_netease_accepted(true);
                        ui.set_netease_message("Experimental NetEase source enabled".into());
                    }
                    Err(error) => {
                        ui.set_netease_message(format!("Cannot enable NetEase: {error}").into());
                    }
                }
            }
        });
    }
}

fn install_chrome_login(ui: &K3Window, net: &Arc<NetEaseUi>, data: &Arc<Mutex<AppData>>) {
    {
        let weak = ui.as_weak();
        let net = Arc::clone(net);
        let data = Arc::clone(data);
        ui.on_netease_login_chrome(move || {
            let Some(ui) = weak.upgrade() else { return };
            if ui.get_netease_busy() || !ui.get_netease_accepted() {
                return;
            }
            let generation = net.next_generation();
            ui.set_netease_busy(true);
            ui.set_netease_message("Importing NetEase login from Chrome…".into());
            let net = Arc::clone(&net);
            let weak = weak.clone();
            let data = Arc::clone(&data);
            thread::spawn(move || {
                let result = net
                    .client
                    .import_chrome_session()
                    .map(|session| session.nickname().to_owned());
                let _ = slint::invoke_from_event_loop(move || {
                    if !net.current(generation) {
                        return;
                    }
                    let Some(ui) = weak.upgrade() else { return };
                    ui.set_netease_busy(false);
                    match result {
                        Ok(name) => {
                            signed_in(&ui, &name);
                            resume_downloads(&ui, &weak, &net, &data);
                        }
                        Err(error) => {
                            ui.set_netease_message(format!("Chrome login failed: {error}").into());
                        }
                    }
                });
            });
        });
    }
}

fn install_qr_login(ui: &K3Window, net: &Arc<NetEaseUi>, data: &Arc<Mutex<AppData>>) {
    {
        let weak = ui.as_weak();
        let net = Arc::clone(net);
        let data = Arc::clone(data);
        ui.on_netease_login_qr(move || {
            let Some(ui) = weak.upgrade() else { return };
            if ui.get_netease_busy() || !ui.get_netease_accepted() {
                return;
            }
            let generation = net.next_generation();
            ui.set_netease_busy(true);
            ui.set_netease_qr_visible(false);
            ui.set_netease_message("Requesting NetEase login QR code…".into());
            let net = Arc::clone(&net);
            let weak = weak.clone();
            let data = Arc::clone(&data);
            thread::spawn(move || {
                let ticket = match net.client.begin_login() {
                    Ok(ticket) => ticket,
                    Err(error) => {
                        finish_error(net, weak, generation, format!("QR login failed: {error}"));
                        return;
                    }
                };
                let pixels = match qr_pixels(ticket.qr_url()) {
                    Ok(pixels) => pixels,
                    Err(error) => {
                        finish_error(net, weak, generation, error);
                        return;
                    }
                };
                show_qr_code(&net, &weak, generation, pixels);
                while net.current(generation) {
                    match net.client.poll_login(&ticket) {
                        Ok(LoginStatus::WaitingScan) => {}
                        Ok(LoginStatus::WaitingConfirmation) => {
                            let qr_net = Arc::clone(&net);
                            let qr_weak = weak.clone();
                            let _ = slint::invoke_from_event_loop(move || {
                                if qr_net.current(generation)
                                    && let Some(ui) = qr_weak.upgrade()
                                {
                                    ui.set_netease_message(
                                        "Confirm login in the NetEase app".into(),
                                    );
                                }
                            });
                        }
                        Ok(LoginStatus::Expired) => {
                            finish_error(
                                net,
                                weak,
                                generation,
                                "QR code expired. Try again.".into(),
                            );
                            return;
                        }
                        Ok(LoginStatus::LoggedIn) => {
                            let result = net
                                .client
                                .session()
                                .map(|session| session.map(|value| value.nickname().to_owned()));
                            let _ = slint::invoke_from_event_loop(move || {
                                if !net.current(generation) {
                                    return;
                                }
                                let Some(ui) = weak.upgrade() else { return };
                                ui.set_netease_busy(false);
                                match result {
                                    Ok(Some(name)) => {
                                        signed_in(&ui, &name);
                                        resume_downloads(&ui, &weak, &net, &data);
                                    }
                                    Ok(None) => ui.set_netease_message(
                                        "Login completed without a saved session".into(),
                                    ),
                                    Err(error) => ui.set_netease_message(
                                        format!("Login failed: {error}").into(),
                                    ),
                                }
                            });
                            return;
                        }
                        Err(error) => {
                            finish_error(
                                net,
                                weak,
                                generation,
                                format!("QR login failed: {error}"),
                            );
                            return;
                        }
                    }
                    thread::sleep(Duration::from_secs(2));
                }
            });
        });
    }
}

fn install_logout(ui: &K3Window, net: &Arc<NetEaseUi>) {
    {
        let weak = ui.as_weak();
        let net = Arc::clone(net);
        ui.on_netease_logout(move || {
            let Some(ui) = weak.upgrade() else { return };
            if ui.get_netease_busy() || ui.get_netease_download_running() {
                return;
            }
            net.next_generation();
            match net.client.logout() {
                Ok(()) => {
                    net.songs.lock().unwrap().clear();
                    net.selected_ids.lock().unwrap().clear();
                    net.downloaded_ids.lock().unwrap().clear();
                    *net.downloads.lock().unwrap() = DownloadQueue::default();
                    ui.set_netease_logged_in(false);
                    ui.set_netease_account("".into());
                    ui.set_netease_qr_visible(false);
                    ui.set_netease_songs(ModelRc::new(VecModel::default()));
                    ui.set_netease_selected_count(0);
                    ui.set_netease_queued_count(0);
                    ui.set_netease_message("Logged out of NetEase".into());
                }
                Err(error) => ui.set_netease_message(format!("Log out failed: {error}").into()),
            }
        });
    }
}

fn install_catalog_callbacks(ui: &K3Window, net: &Arc<NetEaseUi>, data: &Arc<Mutex<AppData>>) {
    {
        let weak = ui.as_weak();
        let net = Arc::clone(net);
        let data = Arc::clone(data);
        ui.on_netease_search(move |query| {
            let query = query.trim().to_owned();
            if query.is_empty() {
                if let Some(ui) = weak.upgrade() {
                    ui.set_netease_message("Enter a song or artist".into());
                }
                return;
            }
            load_songs(&weak, &net, &data, Some(query));
        });
    }
    {
        let weak = ui.as_weak();
        let net = Arc::clone(net);
        let data = Arc::clone(data);
        ui.on_netease_liked(move || load_songs(&weak, &net, &data, None));
    }
}

fn install_selection_callback(ui: &K3Window, net: &Arc<NetEaseUi>) {
    let weak = ui.as_weak();
    let net = Arc::clone(net);
    ui.on_netease_toggle_selection(move |index| {
        let Some(ui) = weak.upgrade() else { return };
        let Some(song) = usize::try_from(index)
            .ok()
            .and_then(|index| net.songs.lock().ok()?.get(index).cloned())
        else {
            return;
        };
        if !song.available {
            ui.set_netease_message("This song is unavailable for this account or region".into());
            return;
        }
        if net.downloads.lock().unwrap().contains(song.id) {
            ui.set_netease_message(format!("Already queued: {}", song.title).into());
            return;
        }
        let mut selected = net.selected_ids.lock().unwrap();
        if !selected.insert(song.id) {
            selected.remove(&song.id);
        }
        drop(selected);
        net.refresh_song_items(&ui);
    });
}

fn install_download_callback(ui: &K3Window, net: &Arc<NetEaseUi>, data: &Arc<Mutex<AppData>>) {
    let weak = ui.as_weak();
    let net = Arc::clone(net);
    let data = Arc::clone(data);
    ui.on_netease_queue_selected(move || {
        let Some(ui) = weak.upgrade() else { return };
        if ui.get_netease_busy() || !ui.get_netease_logged_in() {
            return;
        }
        let Some(root) = data
            .lock()
            .ok()
            .and_then(|state| state.settings.projects_root.clone())
        else {
            ui.set_netease_message("Choose a projects folder first".into());
            return;
        };
        let selected = net.selected_ids.lock().unwrap().clone();
        let songs = net.songs.lock().unwrap();
        let mut downloads = net.downloads.lock().unwrap();
        if downloads.active.is_none() && downloads.queued.is_empty() {
            downloads.finished = 0;
            downloads.issues.clear();
        }
        let mut added = 0;
        for song in songs.iter().filter(|song| selected.contains(&song.id)) {
            if downloads.enqueue(DownloadJob {
                song: song.clone(),
                projects_root: root.clone(),
                profile_index: ui.get_separation_profile(),
            }) {
                added += 1;
            }
        }
        drop(downloads);
        drop(songs);
        net.selected_ids.lock().unwrap().clear();
        net.refresh_song_items(&ui);
        ui.set_netease_message(format!("Queued {added} songs for download and separation").into());
        launch_next_download(&ui, &weak, &net, &data);
    });
}

fn launch_next_download(
    ui: &K3Window,
    weak: &slint::Weak<K3Window>,
    net: &Arc<NetEaseUi>,
    data: &Arc<Mutex<AppData>>,
) {
    let mut downloads = net.downloads.lock().unwrap();
    if downloads.active.is_some() {
        return;
    }
    let job = downloads.start_next();
    let waiting = downloads.queued.len();
    let summary = if job.is_none() && !downloads.paused_for_login && downloads.finished > 0 {
        Some(format!(
            "Downloads finished: {} processed, {} issues{}",
            downloads.finished,
            downloads.issues.len(),
            downloads
                .issues
                .last()
                .map_or_else(String::new, |error| format!(". Last error: {error}"))
        ))
    } else {
        None
    };
    drop(downloads);
    ui.set_netease_queued_count(visible_count(waiting));
    let Some(job) = job else {
        ui.set_netease_download_running(false);
        if let Some(summary) = summary {
            ui.set_netease_message(summary.into());
        }
        net.refresh_song_items(ui);
        return;
    };
    ui.set_netease_download_running(true);
    ui.set_netease_message(format!("Downloading {} · {waiting} waiting…", job.song.title).into());
    net.refresh_song_items(ui);
    let net = Arc::clone(net);
    let weak = weak.clone();
    let data = Arc::clone(data);
    thread::spawn(move || {
        let music_root = job.projects_root.join(".netease-audio");
        let outcome = net
            .client
            .download_song(&music_root, &job.song, &net.cancelled)
            .and_then(|outcome| match outcome {
                DownloadOutcome::Downloaded { path, .. } => Ok(path),
                DownloadOutcome::Skipped { song_id, .. } => net
                    .client
                    .downloaded_song_path(&music_root, song_id)?
                    .ok_or_else(|| NeteaseError::Protocol("cached audio is missing".into())),
                DownloadOutcome::Unavailable { reason, .. } => Err(NeteaseError::Protocol(reason)),
            });
        let retryable = outcome.as_ref().ok().is_some_and(|path| {
            separation::retryable_unprepared_destination(path, &job.projects_root)
        });
        let _ = slint::invoke_from_event_loop(move || {
            if net.cancelled.load(Ordering::Acquire) {
                return;
            }
            let Some(ui) = weak.upgrade() else { return };
            finish_download(&ui, &weak, &net, &data, &job, outcome, retryable);
        });
    });
}

fn finish_download(
    ui: &K3Window,
    weak: &slint::Weak<K3Window>,
    net: &Arc<NetEaseUi>,
    data: &Arc<Mutex<AppData>>,
    job: &DownloadJob,
    outcome: Result<PathBuf, NeteaseError>,
    retryable: bool,
) {
    if matches!(outcome, Err(NeteaseError::LoginRequired)) {
        net.downloads.lock().unwrap().pause_active();
        ui.set_netease_logged_in(false);
        ui.set_netease_account("".into());
        ui.set_netease_message(
            "NetEase session expired. Log in again to resume queued downloads.".into(),
        );
        ui.set_netease_download_running(false);
        net.refresh_song_items(ui);
        return;
    }
    let mut downloads = net.downloads.lock().unwrap();
    downloads.finish_active();
    if let Err(error) = &outcome {
        downloads
            .issues
            .push(format!("{}: {error}", job.song.title));
    }
    drop(downloads);
    if outcome.is_ok() {
        net.downloaded_ids.lock().unwrap().insert(job.song.id);
    }
    match outcome {
        Ok(path) => match separation::destination(&path, &job.projects_root) {
            Ok((_, true)) if retryable => {
                let waiting = super::queue_netease_separation(
                    ui,
                    data,
                    path,
                    &job.projects_root,
                    job.profile_index,
                    true,
                );
                ui.set_netease_message(
                    format!(
                        "Retrying separation for {} · {waiting} waiting",
                        job.song.title
                    )
                    .into(),
                );
            }
            Ok((_, true)) => {
                let issue = format!(
                    "{}: a same-name project exists; move it before importing this audio",
                    job.song.title
                );
                net.downloads.lock().unwrap().issues.push(issue.clone());
                ui.set_netease_message(format!("Downloaded, but not separated: {issue}").into());
            }
            Ok((_, false)) => {
                let waiting = super::queue_netease_separation(
                    ui,
                    data,
                    path,
                    &job.projects_root,
                    job.profile_index,
                    false,
                );
                ui.set_netease_message(
                    format!(
                        "Downloaded {} · {waiting} waiting to separate",
                        job.song.title
                    )
                    .into(),
                );
            }
            Err(error) => {
                net.downloads
                    .lock()
                    .unwrap()
                    .issues
                    .push(format!("{}: {error}", job.song.title));
                ui.set_netease_message(
                    format!(
                        "Downloaded {} but cannot prepare project: {error}",
                        job.song.title
                    )
                    .into(),
                );
            }
        },
        Err(error) => ui.set_netease_message(
            format!("NetEase download failed for {}: {error}", job.song.title).into(),
        ),
    }
    net.refresh_song_items(ui);
    launch_next_download(ui, weak, net, data);
}

fn visible_count(count: usize) -> i32 {
    i32::try_from(count).unwrap_or(i32::MAX)
}

fn resume_downloads(
    ui: &K3Window,
    weak: &slint::Weak<K3Window>,
    net: &Arc<NetEaseUi>,
    data: &Arc<Mutex<AppData>>,
) {
    net.downloads.lock().unwrap().paused_for_login = false;
    launch_next_download(ui, weak, net, data);
}

impl NetEaseUi {
    fn refresh_song_items(&self, ui: &K3Window) {
        let songs = self.songs.lock().unwrap();
        let selected = self.selected_ids.lock().unwrap();
        let downloaded = self.downloaded_ids.lock().unwrap();
        let downloads = self.downloads.lock().unwrap();
        let items = songs
            .iter()
            .map(|song| NeteaseSongItem {
                title: song.title.clone().into(),
                detail: format!(
                    "{} · {} · {}{}",
                    song.artists.join(" / "),
                    song.album,
                    song.max_quality,
                    if song.available {
                        ""
                    } else {
                        " · unavailable"
                    }
                )
                .into(),
                available: song.available,
                selected: selected.contains(&song.id),
                queued: downloads.contains(song.id),
                downloaded: downloaded.contains(&song.id),
            })
            .collect::<Vec<_>>();
        ui.set_netease_selected_count(visible_count(selected.len()));
        ui.set_netease_queued_count(visible_count(downloads.queued.len()));
        let model = ui.get_netease_songs();
        if model.row_count() == items.len() && model.as_any().is::<VecModel<NeteaseSongItem>>() {
            for (index, item) in items.into_iter().enumerate() {
                model.set_row_data(index, item);
            }
        } else {
            ui.set_netease_songs(ModelRc::new(VecModel::from(items)));
        }
    }

    fn next_generation(&self) -> u64 {
        self.generation.fetch_add(1, Ordering::AcqRel) + 1
    }

    fn current(&self, generation: u64) -> bool {
        !self.cancelled.load(Ordering::Acquire)
            && self.generation.load(Ordering::Acquire) == generation
    }
}

fn signed_in(ui: &K3Window, name: &str) {
    ui.set_netease_logged_in(true);
    ui.set_netease_account(name.into());
    ui.set_netease_qr_visible(false);
    ui.set_netease_message(format!("Logged in to NetEase as {name}").into());
}

fn show_qr_code(
    net: &Arc<NetEaseUi>,
    weak: &slint::Weak<K3Window>,
    generation: u64,
    pixels: SharedPixelBuffer<Rgb8Pixel>,
) {
    let net = Arc::clone(net);
    let weak = weak.clone();
    let _ = slint::invoke_from_event_loop(move || {
        if !net.current(generation) {
            return;
        }
        if let Some(ui) = weak.upgrade() {
            ui.set_netease_qr(Image::from_rgb8(pixels));
            ui.set_netease_qr_visible(true);
            ui.set_netease_message("Scan with the NetEase Cloud Music app".into());
        }
    });
}

fn finish_error(
    net: Arc<NetEaseUi>,
    weak: slint::Weak<K3Window>,
    generation: u64,
    message: String,
) {
    let _ = slint::invoke_from_event_loop(move || {
        if !net.current(generation) {
            return;
        }
        if let Some(ui) = weak.upgrade() {
            ui.set_netease_busy(false);
            ui.set_netease_qr_visible(false);
            ui.set_netease_message(message.into());
        }
    });
}

fn load_songs(
    weak: &slint::Weak<K3Window>,
    net: &Arc<NetEaseUi>,
    data: &Arc<Mutex<AppData>>,
    query: Option<String>,
) {
    let Some(ui) = weak.upgrade() else { return };
    if ui.get_netease_busy() || !ui.get_netease_logged_in() {
        return;
    }
    let generation = net.next_generation();
    ui.set_netease_busy(true);
    ui.set_netease_message(
        if query.is_some() {
            "Searching NetEase songs…"
        } else {
            "Loading liked songs…"
        }
        .into(),
    );
    let weak = weak.clone();
    let net = Arc::clone(net);
    let root = data
        .lock()
        .ok()
        .and_then(|state| state.settings.projects_root.clone());
    thread::spawn(move || {
        let result = match query {
            Some(query) => net.client.search(&query, 0, 50).map(|page| page.songs),
            None => net.client.liked_songs(),
        };
        let _ = slint::invoke_from_event_loop(move || {
            if !net.current(generation) {
                return;
            }
            let Some(ui) = weak.upgrade() else { return };
            ui.set_netease_busy(false);
            match result {
                Ok(songs) => {
                    let count = songs.len();
                    let cached = root.as_ref().map_or(Ok(HashSet::new()), |root| {
                        let ids = songs.iter().map(|song| song.id).collect::<Vec<_>>();
                        net.client
                            .downloaded_song_ids(&root.join(".netease-audio"), &ids)
                    });
                    *net.songs.lock().unwrap() = songs;
                    net.selected_ids.lock().unwrap().clear();
                    let cache_warning = match cached {
                        Ok(ids) => {
                            *net.downloaded_ids.lock().unwrap() = ids;
                            String::new()
                        }
                        Err(error) => {
                            net.downloaded_ids.lock().unwrap().clear();
                            format!("; downloaded status unavailable: {error}")
                        }
                    };
                    net.refresh_song_items(&ui);
                    ui.set_netease_message(
                        format!("Found {count} songs; select any number to queue{cache_warning}")
                            .into(),
                    );
                }
                Err(error) => {
                    ui.set_netease_message(format!("NetEase catalog failed: {error}").into());
                }
            }
        });
    });
}

fn qr_pixels(url: &str) -> Result<SharedPixelBuffer<Rgb8Pixel>, String> {
    let code = QrCode::new(url.as_bytes())
        .map_err(|error| format!("Cannot render login QR code: {error}"))?;
    let modules = code.width();
    let width = (modules + 8) * 4;
    let size = u32::try_from(width).map_err(|_| "Login QR code is too large")?;
    let mut pixels = SharedPixelBuffer::<Rgb8Pixel>::new(size, size);
    for (position, pixel) in pixels.make_mut_bytes().chunks_exact_mut(3).enumerate() {
        let x = position % width / 4;
        let y = position / width / 4;
        let dark = x >= 4
            && y >= 4
            && x < modules + 4
            && y < modules + 4
            && code[(x - 4, y - 4)] == Color::Dark;
        let value = if dark { 0 } else { 255 };
        pixel.copy_from_slice(&[value, value, value]);
    }
    Ok(pixels)
}

#[cfg(test)]
mod tests {
    use super::*;
    use k3::netease::{NeteaseError, Quality, SessionStore};
    use slint::Model;

    fn job(id: u64) -> DownloadJob {
        DownloadJob {
            song: Song {
                id,
                title: format!("Song {id}"),
                artists: vec!["Artist".into()],
                album: String::new(),
                cover_url: None,
                max_quality: Quality::Standard,
                available: true,
            },
            projects_root: PathBuf::from("/projects"),
            profile_index: 1,
        }
    }

    #[test]
    fn gui_download_is_active_until_the_window_closes() {
        let sandbox = tempfile::tempdir().unwrap();
        let net = NetEaseUi {
            client: NeteaseClient::new(SessionStore::at(sandbox.path().join("session.json"))),
            risk: RiskStore::at(sandbox.path().join("risk.json")),
            songs: Mutex::new(Vec::new()),
            selected_ids: Mutex::new(HashSet::new()),
            downloaded_ids: Mutex::new(HashSet::new()),
            downloads: Mutex::new(DownloadQueue::default()),
            generation: AtomicU64::new(0),
            cancelled: Arc::new(AtomicBool::new(false)),
        };
        let unavailable = Song {
            id: 1,
            title: "Test song".into(),
            artists: vec!["Test artist".into()],
            album: String::new(),
            cover_url: None,
            max_quality: Quality::Standard,
            available: false,
        };

        assert!(matches!(
            net.client
                .download_song(sandbox.path(), &unavailable, &net.cancelled),
            Ok(DownloadOutcome::Unavailable { .. })
        ));
        net.cancelled.store(true, Ordering::Release);
        assert!(matches!(
            net.client
                .download_song(sandbox.path(), &unavailable, &net.cancelled),
            Err(NeteaseError::Cancelled)
        ));
    }

    #[test]
    fn more_songs_can_queue_while_a_download_is_active() {
        let mut queue = DownloadQueue::default();
        assert!(queue.enqueue(job(1)));
        assert_eq!(queue.start_next().unwrap().song.id, 1);
        assert!(queue.enqueue(job(2)));
        assert!(!queue.enqueue(job(1)));
        assert_eq!(queue.queued.len(), 1);
        queue.finish_active();
        assert_eq!(queue.start_next().unwrap().song.id, 2);
    }

    #[test]
    fn gui_keeps_multiple_songs_selected_during_download() {
        let _window = super::super::interaction_tests::setup_window();
        let ui = K3Window::new().unwrap();
        let sandbox = tempfile::tempdir().unwrap();
        let net = Arc::new(NetEaseUi {
            client: NeteaseClient::new(SessionStore::at(sandbox.path().join("session.json"))),
            risk: RiskStore::at(sandbox.path().join("risk.json")),
            songs: Mutex::new(vec![job(1).song, job(2).song]),
            selected_ids: Mutex::new(HashSet::new()),
            downloaded_ids: Mutex::new(HashSet::new()),
            downloads: Mutex::new(DownloadQueue::default()),
            generation: AtomicU64::new(0),
            cancelled: Arc::new(AtomicBool::new(false)),
        });
        install_selection_callback(&ui, &net);
        net.refresh_song_items(&ui);
        let original_model = ui.get_netease_songs();
        ui.invoke_netease_toggle_selection(0);
        ui.set_netease_download_running(true);
        ui.invoke_netease_toggle_selection(1);

        assert_eq!(ui.get_netease_selected_count(), 2);
        let songs = ui.get_netease_songs();
        assert!(std::ptr::eq(original_model.as_any(), songs.as_any()));
        assert!(songs.row_data(0).unwrap().selected);
        assert!(songs.row_data(1).unwrap().selected);
    }

    #[test]
    fn expired_login_keeps_queued_downloads_in_order() {
        let mut queue = DownloadQueue::default();
        assert!(queue.enqueue(job(1)));
        assert!(queue.enqueue(job(2)));
        assert_eq!(queue.start_next().unwrap().song.id, 1);
        queue.pause_active();
        assert!(queue.start_next().is_none());
        assert_eq!(queue.queued.len(), 2);
        queue.paused_for_login = false;
        assert_eq!(queue.start_next().unwrap().song.id, 1);
        queue.finish_active();
        assert_eq!(queue.start_next().unwrap().song.id, 2);
    }
}
