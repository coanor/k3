use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread,
    time::Duration,
};

use k3::netease::{DownloadOutcome, LoginStatus, NeteaseClient, RiskStore, Song, local_stores};
use qrcode::{Color, QrCode};
use slint::{ComponentHandle, Image, ModelRc, Rgb8Pixel, SharedPixelBuffer, VecModel};

use super::{
    AppData, K3Window, NeteaseSongItem, RecordingState, SeparationState, path_text, separation,
};

struct NetEaseUi {
    client: NeteaseClient,
    risk: RiskStore,
    songs: Mutex<Vec<Song>>,
    generation: AtomicU64,
    running: Arc<AtomicBool>,
}

pub(super) fn install(ui: &K3Window, data: &Arc<Mutex<AppData>>) -> Arc<AtomicBool> {
    let running = Arc::new(AtomicBool::new(true));
    let (session_store, risk) = match local_stores() {
        Ok(stores) => stores,
        Err(error) => {
            ui.set_netease_message(format!("NetEase setup failed: {error}").into());
            return running;
        }
    };
    let net = Arc::new(NetEaseUi {
        client: NeteaseClient::new(session_store),
        risk,
        songs: Mutex::new(Vec::new()),
        generation: AtomicU64::new(0),
        running: Arc::clone(&running),
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
    install_chrome_login(ui, &net);
    install_qr_login(ui, &net);
    install_logout(ui, &net);
    install_catalog_callbacks(ui, &net);
    install_download_callback(ui, &net, data);
    running
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

fn install_chrome_login(ui: &K3Window, net: &Arc<NetEaseUi>) {
    {
        let weak = ui.as_weak();
        let net = Arc::clone(net);
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
                        Ok(name) => signed_in(&ui, &name),
                        Err(error) => {
                            ui.set_netease_message(format!("Chrome login failed: {error}").into());
                        }
                    }
                });
            });
        });
    }
}

fn install_qr_login(ui: &K3Window, net: &Arc<NetEaseUi>) {
    {
        let weak = ui.as_weak();
        let net = Arc::clone(net);
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
                                    Ok(Some(name)) => signed_in(&ui, &name),
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
            if ui.get_netease_busy() {
                return;
            }
            net.next_generation();
            match net.client.logout() {
                Ok(()) => {
                    net.songs.lock().unwrap().clear();
                    ui.set_netease_logged_in(false);
                    ui.set_netease_account("".into());
                    ui.set_netease_qr_visible(false);
                    ui.set_netease_songs(ModelRc::new(VecModel::default()));
                    ui.set_netease_selected(-1);
                    ui.set_netease_message("Logged out of NetEase".into());
                }
                Err(error) => ui.set_netease_message(format!("Log out failed: {error}").into()),
            }
        });
    }
}

fn install_catalog_callbacks(ui: &K3Window, net: &Arc<NetEaseUi>) {
    {
        let weak = ui.as_weak();
        let net = Arc::clone(net);
        ui.on_netease_search(move |query| {
            let query = query.trim().to_owned();
            if query.is_empty() {
                if let Some(ui) = weak.upgrade() {
                    ui.set_netease_message("Enter a song or artist".into());
                }
                return;
            }
            load_songs(&weak, &net, Some(query));
        });
    }
    {
        let weak = ui.as_weak();
        let net = Arc::clone(net);
        ui.on_netease_liked(move || load_songs(&weak, &net, None));
    }
}

fn install_download_callback(ui: &K3Window, net: &Arc<NetEaseUi>, data: &Arc<Mutex<AppData>>) {
    {
        let weak = ui.as_weak();
        let net = Arc::clone(net);
        let data = Arc::clone(data);
        ui.on_netease_download(move |index| {
            let Some(ui) = weak.upgrade() else { return };
            if ui.get_netease_busy() || ui.get_separation_state() == SeparationState::Running { return; }
            let Some(song) = usize::try_from(index).ok().and_then(|index| net.songs.lock().ok()?.get(index).cloned()) else { return };
            if !song.available {
                ui.set_netease_message("This song is unavailable for this account or region".into());
                return;
            }
            let Some(root) = data.lock().ok().and_then(|data| data.settings.projects_root.clone()) else {
                ui.set_netease_message("Choose a projects folder first".into());
                return;
            };
            let generation = net.next_generation();
            ui.set_netease_busy(true);
            ui.set_netease_message(format!("Downloading {}…", song.title).into());
            let weak = weak.clone();
            let net = Arc::clone(&net);
            thread::spawn(move || {
                let music_root = root.join(".netease-audio");
                let result = net
                    .client
                    .download_song(&music_root, &song, &net.running)
                    .and_then(|outcome| match outcome {
                        DownloadOutcome::Downloaded { path, .. } => Ok(path),
                        DownloadOutcome::Skipped { song_id, .. } => net
                            .client
                            .downloaded_song_path(&music_root, song_id)?
                            .ok_or_else(|| {
                                k3::netease::NeteaseError::Protocol("cached audio is missing".into())
                            }),
                        DownloadOutcome::Unavailable { reason, .. } => {
                            Err(k3::netease::NeteaseError::Protocol(reason))
                        }
                    })
                    .map_err(|error| error.to_string())
                    .and_then(|path| {
                        separation::destination(&path, &root).map(|(_, exists)| (path, exists))
                    });
                let _ = slint::invoke_from_event_loop(move || {
                    if !net.current(generation) {
                        return;
                    }
                    let Some(ui) = weak.upgrade() else { return };
                    ui.set_netease_busy(false);
                    match result {
                        Ok((path, exists)) => {
                            ui.set_separation_source(path_text(&path));
                            ui.set_separation_existing(exists);
                            ui.set_separation_state(SeparationState::Idle);
                            if exists {
                                ui.set_netease_message(
                                    "A same-name project exists. Replace stems uses its saved source, not this download; move the existing project before importing this audio.".into(),
                                );
                            } else if ui.get_recording_state() != RecordingState::Idle {
                                ui.set_netease_message(
                                    "Audio is ready. Stop recording, then select Create and separate.".into(),
                                );
                            } else {
                                ui.set_netease_message(
                                    "Audio downloaded. Starting separation…".into(),
                                );
                                ui.invoke_start_separation(
                                    path_text(&path),
                                    ui.get_separation_profile(),
                                    false,
                                );
                            }
                        }
                        Err(error) => ui.set_netease_message(
                            format!("NetEase download failed: {error}").into(),
                        ),
                    }
                });
            });
        });
    }
}

impl NetEaseUi {
    fn next_generation(&self) -> u64 {
        self.generation.fetch_add(1, Ordering::AcqRel) + 1
    }

    fn current(&self, generation: u64) -> bool {
        self.running.load(Ordering::Acquire)
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

fn load_songs(weak: &slint::Weak<K3Window>, net: &Arc<NetEaseUi>, query: Option<String>) {
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
                        })
                        .collect::<Vec<_>>();
                    let count = items.len();
                    *net.songs.lock().unwrap() = songs;
                    ui.set_netease_songs(ModelRc::new(VecModel::from(items)));
                    ui.set_netease_selected(-1);
                    ui.set_netease_message(
                        format!("Found {count} songs; select one to download and separate").into(),
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
