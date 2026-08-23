use std::{
    error::Error,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

use k3_app::{
    LoadedProject, PlaybackCommand, PlaybackService, PlaybackSnapshot, PlaybackStatus,
    ProjectLibrary, ProjectSummary, RodioBackend, TrackKind, lyric_countdown,
};
use k3_gui::{
    logging::DiagnosticLog,
    settings::{GuiSettings, SettingsWriter, SettingsWriterHandle},
};
use slint::winit_030::{EventResult, WinitWindowAccessor, winit};
use slint::{ComponentHandle, LogicalSize, ModelRc, SharedString, VecModel};

slint::include_modules!();

struct AppData {
    settings: GuiSettings,
    settings_writable: bool,
    projects: Vec<ProjectSummary>,
    selected_id: Option<String>,
    selected_document_revision: Option<Arc<[u8]>>,
    scan_generation: u64,
    open_generation: u64,
}

fn main() {
    let log = DiagnosticLog::initialize().ok();
    if let Some(log) = log {
        log.record(format!(
            "GUI starting; session={}",
            std::env::var("XDG_SESSION_TYPE").unwrap_or_else(|_| "unknown".into())
        ));
    }
    if let Err(error) = run() {
        if let Some(log) = log {
            log.record(format!("GUI stopped with error: {error}"));
        }
        eprintln!("K3 GUI could not start: {error}");
        if let Some(log) = log {
            eprintln!("Diagnostics: {}", log.path().display());
        }
        eprintln!("Run `k3 tui` to use the terminal interface instead.");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    let (settings, settings_writable) = match GuiSettings::load() {
        Ok(settings) => (settings, true),
        Err(error) => {
            eprintln!("K3 GUI settings could not be loaded and will be preserved: {error}");
            (GuiSettings::default(), false)
        }
    };
    let initial_volume = settings.volume;
    let ui = K3Window::new()?;
    if let Ok(log) = DiagnosticLog::initialize() {
        ui.set_log_path(path_text(log.path()));
    }
    ui.window().set_size(LogicalSize::new(
        logical_dimension(settings.window.width),
        logical_dimension(settings.window.height),
    ));
    ui.set_volume(settings.volume);
    ui.set_projects_root(
        settings
            .projects_root
            .as_deref()
            .map_or_else(SharedString::default, path_text),
    );
    ui.set_setup_mode(settings.projects_root.is_none());

    let data = Arc::new(Mutex::new(AppData {
        settings,
        settings_writable,
        projects: Vec::new(),
        selected_id: None,
        selected_document_revision: None,
        scan_generation: 0,
        open_generation: 0,
    }));
    let (settings_writer, settings_writer_handle) = SettingsWriter::start()?;
    let playback = Arc::new(PlaybackService::start(RodioBackend::default()));
    let _ = playback.execute(PlaybackCommand::SetVolume(initial_volume));
    install_callbacks(&ui, &data, &playback, &settings_writer_handle);
    install_focus_refresh(&ui, &data, &settings_writer_handle);

    let pump_running = Arc::new(AtomicBool::new(true));
    let snapshot_pump = start_snapshot_pump(
        ui.as_weak(),
        Arc::clone(&playback),
        Arc::clone(&pump_running),
    );

    if let Some(root) = data
        .lock()
        .ok()
        .and_then(|data| data.settings.projects_root.clone())
    {
        scan_library(
            ui.as_weak(),
            Arc::clone(&data),
            settings_writer_handle.clone(),
            root,
            true,
            false,
        );
    }

    let ui_result = ui.run();
    pump_running.store(false, Ordering::Release);
    let _ = snapshot_pump.join();
    ui_result?;
    let physical = ui.window().size();
    let logical = physical.to_logical(ui.window().scale_factor());
    if let Ok(mut data) = data.lock() {
        data.settings.window.width = saved_dimension(logical.width, 640.0);
        data.settings.window.height = saved_dimension(logical.height, 480.0);
        if data.settings_writable {
            settings_writer_handle.persist(data.settings.clone());
        }
    }
    settings_writer.finish();
    Ok(())
}

fn install_focus_refresh(
    ui: &K3Window,
    data: &Arc<Mutex<AppData>>,
    settings_writer: &SettingsWriterHandle,
) {
    let ui = ui.as_weak();
    let data = Arc::clone(data);
    let settings_writer = settings_writer.clone();
    let mut was_focused = true;
    ui.unwrap().window().on_winit_window_event(move |_, event| {
        if let winit::event::WindowEvent::Focused(focused) = event {
            if *focused && !was_focused {
                let root = data
                    .lock()
                    .ok()
                    .and_then(|data| data.settings.projects_root.clone());
                if let Some(root) = root {
                    scan_library(
                        ui.clone(),
                        Arc::clone(&data),
                        settings_writer.clone(),
                        root,
                        false,
                        false,
                    );
                }
            }
            was_focused = *focused;
        }
        EventResult::Propagate
    });
}

fn install_callbacks(
    ui: &K3Window,
    data: &Arc<Mutex<AppData>>,
    playback: &Arc<PlaybackService>,
    settings_writer: &SettingsWriterHandle,
) {
    install_library_callbacks(ui, data, playback, settings_writer);
    install_playback_callbacks(ui, data, playback, settings_writer);
}

// Keeping callback registration flat makes ownership and generation captures auditable.
#[allow(clippy::too_many_lines)]
fn install_library_callbacks(
    ui: &K3Window,
    data: &Arc<Mutex<AppData>>,
    playback: &Arc<PlaybackService>,
    settings_writer: &SettingsWriterHandle,
) {
    {
        let ui = ui.as_weak();
        ui.unwrap().on_choose_projects_root(move || {
            let ui = ui.clone();
            thread::spawn(move || {
                let Some(path) = rfd::FileDialog::new().pick_folder() else {
                    return;
                };
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(ui) = ui.upgrade() {
                        ui.set_projects_root(path_text(&path));
                    }
                });
            });
        });
    }

    {
        let ui = ui.as_weak();
        let data = Arc::clone(data);
        let settings_writer = settings_writer.clone();
        ui.unwrap().on_save_projects_root(move |root| {
            let path = PathBuf::from(root.as_str());
            if !path.is_dir() {
                if let Some(ui) = ui.upgrade() {
                    show_error(
                        &ui,
                        format!("Projects folder is not readable: {}", path.display()),
                    );
                }
                return;
            }
            if let Some(ui) = ui.upgrade() {
                ui.set_loading(true);
                ui.set_has_error(false);
                ui.set_playback_error_visible(false);
            }
            scan_library(
                ui.clone(),
                Arc::clone(&data),
                settings_writer.clone(),
                path,
                false,
                true,
            );
        });
    }

    {
        let ui = ui.as_weak();
        let data = Arc::clone(data);
        ui.unwrap().on_search(move |query| {
            if let Some(ui) = ui.upgrade() {
                publish_projects(&ui, &data, query.as_str());
            }
        });
    }

    {
        let ui = ui.as_weak();
        let data = Arc::clone(data);
        let settings_writer = settings_writer.clone();
        ui.unwrap().on_refresh_library(move || {
            let root = data
                .lock()
                .ok()
                .and_then(|data| data.settings.projects_root.clone());
            if let Some(root) = root {
                if let Some(ui) = ui.upgrade() {
                    ui.set_loading(true);
                }
                scan_library(
                    ui.clone(),
                    Arc::clone(&data),
                    settings_writer.clone(),
                    root,
                    false,
                    false,
                );
            }
        });
    }

    {
        let ui = ui.as_weak();
        let data = Arc::clone(data);
        let playback = Arc::clone(playback);
        let settings_writer = settings_writer.clone();
        ui.unwrap().on_open_project(move |id| {
            let project = data.lock().ok().and_then(|mut data| {
                let project = data
                    .projects
                    .iter()
                    .find(|project| project.id.to_string() == id.as_str())
                    .cloned();
                if project.is_some() {
                    data.open_generation = data.open_generation.wrapping_add(1);
                }
                project.map(|project| (project, data.open_generation))
            });
            let Some((project, generation)) = project else {
                return;
            };
            if let Some(ui) = ui.upgrade() {
                ui.set_loading(true);
                ui.set_has_error(false);
                ui.set_playback_error_visible(false);
                ui.set_project_state("Loading audio…".into());
            }
            open_project(
                ui.clone(),
                Arc::clone(&data),
                Arc::clone(&playback),
                settings_writer.clone(),
                generation,
                project,
            );
        });
    }
}

// Keeping callback registration flat makes each event's worker handoff explicit.
#[allow(clippy::too_many_lines)]
fn install_playback_callbacks(
    ui: &K3Window,
    data: &Arc<Mutex<AppData>>,
    playback: &Arc<PlaybackService>,
    settings_writer: &SettingsWriterHandle,
) {
    {
        let ui = ui.as_weak();
        let playback = Arc::clone(playback);
        let data = Arc::clone(data);
        ui.unwrap().on_playback_command(move |command| {
            let command = match command.as_str() {
                "toggle" => PlaybackCommand::Toggle,
                "back" => PlaybackCommand::SeekBy(-5),
                "forward" => PlaybackCommand::SeekBy(5),
                "restart" => PlaybackCommand::Restart,
                "retry" => PlaybackCommand::Retry,
                _ => return,
            };
            dispatch_playback(ui.clone(), &playback, &data, command);
        });
    }

    {
        let ui = ui.as_weak();
        let playback = Arc::clone(playback);
        let data = Arc::clone(data);
        ui.unwrap().on_seek_to(move |seconds| {
            dispatch_playback(
                ui.clone(),
                &playback,
                &data,
                PlaybackCommand::SeekTo(Duration::from_secs_f32(seconds.max(0.0))),
            );
        });
    }

    {
        let ui = ui.as_weak();
        let playback = Arc::clone(playback);
        let data = Arc::clone(data);
        ui.unwrap().on_seek_lyric(move |seconds| {
            dispatch_playback(
                ui.clone(),
                &playback,
                &data,
                PlaybackCommand::SeekTo(Duration::from_secs_f32(seconds.max(0.0))),
            );
        });
    }

    {
        let ui = ui.as_weak();
        let playback = Arc::clone(playback);
        let data = Arc::clone(data);
        ui.unwrap().on_set_volume(move |volume| {
            if let Some(ui) = ui.upgrade() {
                ui.set_volume(volume.clamp(0.0, 1.0));
            }
            if let Ok(mut data) = data.lock() {
                data.settings.volume = volume.clamp(0.0, 1.0);
            }
            dispatch_playback(
                ui.clone(),
                &playback,
                &data,
                PlaybackCommand::SetVolume(volume),
            );
        });
    }

    {
        let data = Arc::clone(data);
        let settings_writer = settings_writer.clone();
        ui.on_save_volume(move |volume| {
            if let Ok(mut data) = data.lock() {
                data.settings.volume = volume.clamp(0.0, 1.0);
                if data.settings_writable {
                    settings_writer.persist(data.settings.clone());
                }
            }
        });
    }

    {
        let ui = ui.as_weak();
        let playback = Arc::clone(playback);
        let data = Arc::clone(data);
        ui.unwrap().on_set_key(move |semitones| {
            let Ok(semitones) = i8::try_from(semitones) else {
                return;
            };
            dispatch_playback(
                ui.clone(),
                &playback,
                &data,
                PlaybackCommand::SetKeyShift(semitones),
            );
        });
    }

    {
        let ui = ui.as_weak();
        let playback = Arc::clone(playback);
        let data = Arc::clone(data);
        ui.unwrap().on_switch_track(move |track| {
            let track = match track {
                0 => TrackKind::Original,
                1 => TrackKind::Accompaniment,
                2 => TrackKind::Vocals,
                _ => return,
            };
            dispatch_playback(
                ui.clone(),
                &playback,
                &data,
                PlaybackCommand::SwitchTrack(track),
            );
        });
    }
}

fn scan_library(
    ui: slint::Weak<K3Window>,
    data: Arc<Mutex<AppData>>,
    settings_writer: SettingsWriterHandle,
    root: PathBuf,
    reopen_last: bool,
    commit_root: bool,
) {
    let generation = match data.lock() {
        Ok(mut data) => {
            data.scan_generation = data.scan_generation.wrapping_add(1);
            data.scan_generation
        }
        Err(_) => return,
    };
    thread::spawn(move || {
        let result = ProjectLibrary::scan(&root);
        let _ = slint::invoke_from_event_loop(move || {
            let Some(ui) = ui.upgrade() else {
                return;
            };
            match result {
                Ok(projects) => {
                    let Some((last_project, project_changed, settings)) =
                        data.lock().ok().and_then(|mut data| {
                            if data.scan_generation != generation {
                                return None;
                            }
                            if commit_root {
                                data.settings.projects_root = Some(root.clone());
                                data.settings.last_project_id = None;
                                data.selected_id = None;
                                data.selected_document_revision = None;
                            }
                            let last_project = if reopen_last {
                                data.settings.last_project_id.clone()
                            } else {
                                None
                            };
                            let project_changed = data.selected_id.as_ref().is_some_and(|id| {
                                projects
                                    .iter()
                                    .find(|project| project.id.to_string() == *id)
                                    .is_none_or(|project| {
                                        project.document_revision != data.selected_document_revision
                                    })
                            });
                            let settings = (commit_root && data.settings_writable)
                                .then(|| data.settings.clone());
                            data.projects = projects;
                            Some((last_project, project_changed, settings))
                        })
                    else {
                        return;
                    };
                    ui.set_loading(false);
                    if commit_root {
                        ui.set_projects_root(path_text(&root));
                    }
                    ui.set_setup_mode(false);
                    ui.set_project_changed(project_changed);
                    clear_general_error(&ui);
                    publish_projects(&ui, &data, ui.get_search_query().as_str());
                    if let Some(settings) = settings {
                        settings_writer.persist(settings);
                    }
                    if let Some(id) = last_project {
                        ui.invoke_open_project(id.into());
                    }
                }
                Err(error) => {
                    let current_root = data.lock().ok().and_then(|data| {
                        (data.scan_generation == generation)
                            .then(|| data.settings.projects_root.clone())
                            .flatten()
                    });
                    if data
                        .lock()
                        .ok()
                        .is_none_or(|data| data.scan_generation != generation)
                    {
                        return;
                    }
                    ui.set_loading(false);
                    if commit_root {
                        ui.set_projects_root(
                            current_root
                                .as_deref()
                                .map_or_else(SharedString::default, path_text),
                        );
                    }
                    show_error(&ui, error.to_string());
                }
            }
        });
    });
}

fn open_project(
    ui: slint::Weak<K3Window>,
    data: Arc<Mutex<AppData>>,
    playback: Arc<PlaybackService>,
    settings_writer: SettingsWriterHandle,
    generation: u64,
    project: ProjectSummary,
) {
    thread::spawn(move || {
        let loaded = LoadedProject::open(&project.path);
        let _ = slint::invoke_from_event_loop(move || {
            let Some(ui) = ui.upgrade() else {
                return;
            };
            if !is_current_open(&data, generation) {
                return;
            }
            let loaded = match loaded {
                Ok(loaded) => loaded,
                Err(error) => {
                    ui.set_loading(false);
                    show_error(&ui, error.to_string());
                    return;
                }
            };
            let presentation = loaded.clone();
            let response = match playback.submit(PlaybackCommand::Load(loaded)) {
                Ok(response) => response,
                Err(error) => {
                    ui.set_loading(false);
                    show_playback_error(&ui, error.to_string());
                    return;
                }
            };
            let ui = ui.as_weak();
            thread::spawn(move || {
                let result = response.recv();
                let _ = slint::invoke_from_event_loop(move || {
                    let Some(ui) = ui.upgrade() else {
                        return;
                    };
                    if !is_current_open(&data, generation) {
                        return;
                    }
                    ui.set_loading(false);
                    match result {
                        Ok(snapshot) => {
                            apply_loaded_project(&ui, &presentation);
                            if let Ok(mut data) = data.lock() {
                                let id = presentation.id.to_string();
                                data.selected_id = Some(id.clone());
                                data.selected_document_revision
                                    .clone_from(&presentation.document_revision);
                                data.settings.last_project_id = Some(id.clone());
                                ui.set_current_project_id(id.into());
                                ui.set_project_changed(false);
                                if data.settings_writable {
                                    settings_writer.persist(data.settings.clone());
                                }
                            }
                            publish_projects(&ui, &data, ui.get_search_query().as_str());
                            apply_snapshot(&ui, &snapshot);
                        }
                        Err(error) => show_playback_error(&ui, error.to_string()),
                    }
                });
            });
        });
    });
}

fn is_current_open(data: &Mutex<AppData>, generation: u64) -> bool {
    data.lock()
        .is_ok_and(|data| data.open_generation == generation)
}

fn dispatch_playback(
    ui: slint::Weak<K3Window>,
    playback: &PlaybackService,
    data: &Arc<Mutex<AppData>>,
    command: PlaybackCommand,
) {
    let data = Arc::clone(data);
    let switching_track = matches!(command, PlaybackCommand::SwitchTrack(_));
    let updates_document = matches!(command, PlaybackCommand::SetKeyShift(_));
    if switching_track && let Some(ui) = ui.upgrade() {
        ui.set_switching_track(true);
    }
    let response = match playback.submit(command) {
        Ok(response) => response,
        Err(error) => {
            if let Some(ui) = ui.upgrade() {
                if switching_track {
                    ui.set_switching_track(false);
                }
                show_playback_error(&ui, error.to_string());
            }
            return;
        }
    };
    thread::spawn(move || {
        let result = response.recv();
        let _ = slint::invoke_from_event_loop(move || {
            let Some(ui) = ui.upgrade() else {
                return;
            };
            if switching_track {
                ui.set_switching_track(false);
            }
            match result {
                Ok(snapshot) => {
                    if updates_document
                        && !snapshot.reload_required
                        && !ui.get_project_changed()
                        && let Ok(mut data) = data.lock()
                        && data.selected_id.as_deref()
                            == snapshot.project_id.map(|id| id.to_string()).as_deref()
                    {
                        data.selected_document_revision
                            .clone_from(&snapshot.document_revision);
                    }
                    apply_snapshot(&ui, &snapshot);
                }
                Err(error) => show_playback_error(&ui, error.to_string()),
            }
        });
    });
}

fn start_snapshot_pump(
    ui: slint::Weak<K3Window>,
    playback: Arc<PlaybackService>,
    running: Arc<AtomicBool>,
) -> thread::JoinHandle<()> {
    thread::Builder::new()
        .name("k3-gui-snapshots".into())
        .spawn(move || {
            while running.load(Ordering::Acquire) {
                thread::sleep(Duration::from_millis(80));
                if !running.load(Ordering::Acquire) {
                    break;
                }
                let result = playback.execute(PlaybackCommand::Refresh);
                let _ = ui.upgrade_in_event_loop(move |ui| match result {
                    Ok(snapshot) => apply_snapshot(&ui, &snapshot),
                    Err(error) => show_playback_error(&ui, error.to_string()),
                });
            }
        })
        .expect("failed to start GUI snapshot pump")
}

fn publish_projects(ui: &K3Window, data: &Mutex<AppData>, query: &str) {
    let Ok(data) = data.lock() else {
        return;
    };
    let items = ProjectLibrary::filter(&data.projects, query)
        .into_iter()
        .map(|project| ProjectItem {
            id: project.id.to_string().into(),
            title: project.title.as_str().into(),
            path: path_text(&project.path),
            status: if project.source_available {
                "Ready".into()
            } else {
                "Repair needed".into()
            },
            selected: data
                .selected_id
                .as_ref()
                .is_some_and(|selected| selected == &project.id.to_string()),
        })
        .collect::<Vec<_>>();
    ui.set_projects(ModelRc::new(VecModel::from(items)));
}

fn apply_loaded_project(ui: &K3Window, project: &LoadedProject) {
    ui.set_song_title(project.title.as_str().into());
    ui.set_key_shift(i32::from(project.key_shift_semitones));
    ui.set_original_available(track_available(project, TrackKind::Original));
    ui.set_accompaniment_available(track_available(project, TrackKind::Accompaniment));
    ui.set_vocals_available(track_available(project, TrackKind::Vocals));
    ui.set_active_track(match project.default_track() {
        Some(TrackKind::Original) | None => 0,
        Some(TrackKind::Accompaniment) => 1,
        Some(TrackKind::Vocals) => 2,
    });
    let lyrics = project
        .lyrics
        .as_ref()
        .map(|timeline| {
            timeline
                .lines()
                .iter()
                .map(|line| LyricItem {
                    text: line.text.as_str().into(),
                    at_seconds: line.at.as_secs_f32(),
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    ui.set_lyrics(ModelRc::new(VecModel::from(lyrics)));
    ui.set_active_lyric(-1);
    ui.set_lyric_countdown_index(-1);
    ui.set_lyric_countdown_dots(0);
    ui.set_lyric_scroll_y(0.0);
    ui.set_follow_lyrics(true);
}

fn apply_snapshot(ui: &K3Window, snapshot: &PlaybackSnapshot) {
    if snapshot.project_id.is_none() {
        return;
    }
    if snapshot.reload_required {
        ui.set_project_changed(true);
    }
    ui.set_position_seconds(snapshot.position.as_secs_f32());
    ui.set_duration_seconds(snapshot.duration.map_or(0.0, |value| value.as_secs_f32()));
    ui.set_volume(snapshot.volume);
    ui.set_key_shift(i32::from(snapshot.key_shift_semitones));
    ui.set_playing(snapshot.status == PlaybackStatus::Playing);
    ui.set_playback_ready(matches!(
        snapshot.status,
        PlaybackStatus::Paused | PlaybackStatus::Playing | PlaybackStatus::Finished
    ));
    if let Some(track) = snapshot.track {
        ui.set_active_track(match track {
            TrackKind::Original => 0,
            TrackKind::Accompaniment => 1,
            TrackKind::Vocals => 2,
        });
    }
    ui.set_project_state(
        match snapshot.status {
            PlaybackStatus::Unavailable => "Playback unavailable",
            PlaybackStatus::Loading => "Loading audio…",
            PlaybackStatus::Paused => "Paused",
            PlaybackStatus::Playing => "Playing",
            PlaybackStatus::Finished => "Finished",
            PlaybackStatus::Error => "Playback error",
        }
        .into(),
    );
    if let Some(error) = &snapshot.error {
        show_playback_error(ui, error.clone());
    } else if ui.get_playback_error_visible() {
        ui.set_playback_error_visible(false);
        ui.set_has_error(false);
        ui.set_error_message(SharedString::default());
    }
    let active = snapshot
        .lyrics
        .as_ref()
        .and_then(|lyrics| lyrics.active_index(snapshot.position, 0))
        .and_then(|index| i32::try_from(index).ok())
        .unwrap_or(-1);
    let countdown = snapshot
        .lyrics
        .as_ref()
        .and_then(|lyrics| lyric_countdown(lyrics, snapshot.position));
    ui.set_lyric_countdown_index(
        countdown
            .and_then(|countdown| i32::try_from(countdown.index).ok())
            .unwrap_or(-1),
    );
    ui.set_lyric_countdown_dots(countdown.map_or(0, |countdown| i32::from(countdown.seconds)));
    ui.set_active_lyric(active);
    if ui.get_follow_lyrics() && active >= 0 {
        let active = f32::from(i16::try_from(active).unwrap_or(i16::MAX));
        ui.set_lyric_scroll_y((180.0 - active * 64.0).min(0.0));
    }
}

fn track_available(project: &LoadedProject, kind: TrackKind) -> bool {
    project
        .track(kind)
        .is_some_and(k3_app::ProjectTrack::available)
}

fn show_error(ui: &K3Window, message: impl Into<SharedString>) {
    let message = message.into();
    if let Ok(log) = DiagnosticLog::initialize() {
        log.record(format!("error: {message}"));
    }
    ui.set_has_error(true);
    ui.set_playback_error_visible(false);
    ui.set_error_message(message);
}

fn clear_general_error(ui: &K3Window) {
    if !ui.get_playback_error_visible() {
        ui.set_has_error(false);
        ui.set_error_message(SharedString::default());
    }
}

fn show_playback_error(ui: &K3Window, message: impl Into<SharedString>) {
    let message = message.into();
    if let Ok(log) = DiagnosticLog::initialize() {
        log.record(format!("playback error: {message}"));
    }
    ui.set_has_error(true);
    ui.set_playback_error_visible(true);
    ui.set_error_message(message);
    ui.set_playback_ready(false);
}

fn path_text(path: &Path) -> SharedString {
    path.to_string_lossy().into_owned().into()
}

fn logical_dimension(value: u32) -> f32 {
    f32::from(u16::try_from(value).unwrap_or(u16::MAX))
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn saved_dimension(value: f32, minimum: f32) -> u32 {
    value.round().clamp(minimum, f32::from(u16::MAX)) as u32
}
