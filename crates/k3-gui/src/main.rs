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
    ui_text,
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
        eprintln!("{}", ui_text::gui_start_failed(error.as_ref()));
        if let Some(log) = log {
            eprintln!("{}", ui_text::diagnostics(log.path()));
        }
        eprintln!("{}", ui_text::TUI_FALLBACK);
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    let (settings, settings_writable) = match GuiSettings::load() {
        Ok(settings) => (settings, true),
        Err(error) => {
            eprintln!("{}", ui_text::settings_load_failed(&error));
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
                    show_error(&ui, ui_text::unreadable_projects_folder(&path));
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
                ui.set_project_state(ui_text::LOADING_AUDIO.into());
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
            let mut transitions = PlaybackTransitionTracker::default();
            while running.load(Ordering::Acquire) {
                thread::sleep(Duration::from_millis(80));
                if !running.load(Ordering::Acquire) {
                    break;
                }
                let result = playback.execute(PlaybackCommand::Refresh);
                if let Ok(snapshot) = &result
                    && let Some(message) = transitions.observe(snapshot)
                    && let Ok(log) = DiagnosticLog::initialize()
                {
                    log.record(message);
                }
                let _ = ui.upgrade_in_event_loop(move |ui| match result {
                    Ok(snapshot) => apply_snapshot(&ui, &snapshot),
                    Err(error) => show_playback_error(&ui, error.to_string()),
                });
            }
        })
        .expect("failed to start GUI snapshot pump")
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct PlaybackTransition {
    status: PlaybackStatus,
    track: Option<TrackKind>,
}

#[derive(Default)]
struct PlaybackTransitionTracker {
    previous: Option<PlaybackTransition>,
}

impl PlaybackTransitionTracker {
    fn observe(&mut self, snapshot: &PlaybackSnapshot) -> Option<String> {
        let next = PlaybackTransition {
            status: snapshot.status,
            track: snapshot.track,
        };
        if self.previous == Some(next) {
            return None;
        }
        self.previous = Some(next);
        Some(format!(
            "playback state: status={} track={}",
            playback_status_log_label(next.status),
            next.track.map_or("none", TrackKind::log_label)
        ))
    }
}

const fn playback_status_log_label(status: PlaybackStatus) -> &'static str {
    match status {
        PlaybackStatus::Unavailable => "unavailable",
        PlaybackStatus::Loading => "loading",
        PlaybackStatus::Paused => "paused",
        PlaybackStatus::Playing => "playing",
        PlaybackStatus::Finished => "finished",
        PlaybackStatus::Error => "error",
    }
}

trait TrackLogLabel {
    fn log_label(self) -> &'static str;
}

impl TrackLogLabel for TrackKind {
    fn log_label(self) -> &'static str {
        match self {
            Self::Original => "original",
            Self::Accompaniment => "accompaniment",
            Self::Vocals => "vocals",
        }
    }
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
                ui_text::PROJECT_READY.into()
            } else {
                ui_text::PROJECT_REPAIR_NEEDED.into()
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
            PlaybackStatus::Unavailable => ui_text::PLAYBACK_UNAVAILABLE,
            PlaybackStatus::Loading => ui_text::LOADING_AUDIO,
            PlaybackStatus::Paused => ui_text::PLAYBACK_PAUSED,
            PlaybackStatus::Playing => ui_text::PLAYBACK_PLAYING,
            PlaybackStatus::Finished => ui_text::PLAYBACK_FINISHED,
            PlaybackStatus::Error => ui_text::PLAYBACK_ERROR,
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

#[cfg(test)]
mod interaction_tests {
    use std::{
        cell::Cell,
        cell::RefCell,
        rc::Rc,
        time::{Duration, Instant},
    };

    use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
    use slint::platform::{
        Key, Platform, PlatformError, PointerEventButton, WindowAdapter, WindowEvent,
    };
    use slint::{ComponentHandle, LogicalPosition, ModelRc, PhysicalSize, SharedString, VecModel};

    use super::{K3Window, LyricItem, PlaybackTransitionTracker, ProjectItem};
    use k3_app::{PlaybackSnapshot, PlaybackStatus, TrackKind};

    thread_local! {
        static WINDOW: Rc<MinimalSoftwareWindow> =
            MinimalSoftwareWindow::new(RepaintBufferType::ReusedBuffer);
    }

    struct TestPlatform;

    impl Platform for TestPlatform {
        fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, PlatformError> {
            Ok(WINDOW.with(Clone::clone))
        }
    }

    fn setup_window() -> Rc<MinimalSoftwareWindow> {
        let _ = slint::platform::set_platform(Box::new(TestPlatform));
        let window = WINDOW.with(Clone::clone);
        window.set_size(PhysicalSize::new(1280, 800));
        window
    }

    fn click(ui: &K3Window, position: LogicalPosition) {
        ui.window().dispatch_event(WindowEvent::PointerPressed {
            position,
            button: PointerEventButton::Left,
        });
        ui.window().dispatch_event(WindowEvent::PointerReleased {
            position,
            button: PointerEventButton::Left,
        });
    }

    fn press_key(ui: &K3Window, text: impl Into<SharedString>) {
        ui.window()
            .dispatch_event(WindowEvent::KeyPressed { text: text.into() });
    }

    fn release_key(ui: &K3Window, text: impl Into<SharedString>) {
        ui.window()
            .dispatch_event(WindowEvent::KeyReleased { text: text.into() });
    }

    #[test]
    fn custom_buttons_activate_on_first_click() {
        let window = setup_window();
        let ui = K3Window::new().expect("test UI should construct");
        ui.set_setup_mode(false);
        ui.set_playback_ready(true);
        ui.set_accompaniment_available(true);
        ui.set_projects(ModelRc::new(VecModel::from(vec![ProjectItem {
            id: SharedString::from("project-1"),
            title: SharedString::from("Project one"),
            path: SharedString::from("/tmp/project-1"),
            status: SharedString::from("Ready"),
            selected: false,
        }])));

        let refresh_clicks = Rc::new(Cell::new(0));
        let observed_refresh_clicks = Rc::clone(&refresh_clicks);
        ui.on_refresh_library(move || {
            observed_refresh_clicks.set(observed_refresh_clicks.get() + 1);
        });

        let playback_clicks = Rc::new(Cell::new(0));
        let observed_playback_clicks = Rc::clone(&playback_clicks);
        ui.on_playback_command(move |command| {
            assert_eq!(command.as_str(), "toggle");
            observed_playback_clicks.set(observed_playback_clicks.get() + 1);
        });

        let track_clicks = Rc::new(Cell::new(0));
        let observed_track_clicks = Rc::clone(&track_clicks);
        ui.on_switch_track(move |track| {
            assert_eq!(track, 1);
            observed_track_clicks.set(observed_track_clicks.get() + 1);
        });

        let project_clicks = Rc::new(Cell::new(0));
        let observed_project_clicks = Rc::clone(&project_clicks);
        ui.on_open_project(move |project_id| {
            assert_eq!(project_id.as_str(), "project-1");
            observed_project_clicks.set(observed_project_clicks.get() + 1);
        });

        ui.show().expect("test UI should show");
        window.draw_if_needed(|_| {});
        click(&ui, LogicalPosition::new(268.0, 142.0));
        click(&ui, LogicalPosition::new(608.0, 759.0));
        click(&ui, LogicalPosition::new(162.0, 759.0));
        click(&ui, LogicalPosition::new(150.0, 200.0));

        assert_eq!(
            (
                refresh_clicks.get(),
                playback_clicks.get(),
                track_clicks.get(),
                project_clicks.get()
            ),
            (1, 1, 1, 1),
            "the first pointer click must invoke each custom control"
        );
    }

    #[test]
    fn global_shortcuts_emit_playback_volume_track_and_refresh_intents() {
        let window = setup_window();
        let ui = K3Window::new().expect("test UI should construct");
        ui.set_setup_mode(false);
        ui.set_playback_ready(true);
        ui.set_original_available(true);
        ui.set_accompaniment_available(true);
        ui.set_vocals_available(true);

        let playback = Rc::new(RefCell::new(Vec::<String>::new()));
        let observed_playback = Rc::clone(&playback);
        ui.on_playback_command(move |command| {
            observed_playback.borrow_mut().push(command.to_string());
        });
        let tracks = Rc::new(RefCell::new(Vec::<i32>::new()));
        let observed_tracks = Rc::clone(&tracks);
        ui.on_switch_track(move |track| observed_tracks.borrow_mut().push(track));
        let volumes = Rc::new(RefCell::new(Vec::<f32>::new()));
        let observed_volumes = Rc::clone(&volumes);
        ui.on_set_volume(move |volume| observed_volumes.borrow_mut().push(volume));
        let refreshes = Rc::new(Cell::new(0));
        let observed_refreshes = Rc::clone(&refreshes);
        ui.on_refresh_library(move || observed_refreshes.set(observed_refreshes.get() + 1));

        ui.show().expect("test UI should show");
        window.draw_if_needed(|_| {});
        press_key(&ui, " ");
        press_key(&ui, Key::LeftArrow);
        press_key(&ui, Key::RightArrow);
        press_key(&ui, Key::UpArrow);
        press_key(&ui, "2");
        press_key(&ui, Key::Control);
        press_key(&ui, "r");
        release_key(&ui, "r");
        release_key(&ui, Key::Control);

        assert_eq!(playback.borrow().as_slice(), ["toggle", "back", "forward"]);
        assert_eq!(tracks.borrow().as_slice(), [1]);
        assert_eq!(volumes.borrow().as_slice(), [1.0]);
        assert_eq!(refreshes.get(), 1);
    }

    #[test]
    fn search_focus_keeps_typing_from_triggering_playback_shortcuts() {
        let window = setup_window();
        let ui = K3Window::new().expect("test UI should construct");
        ui.set_setup_mode(false);
        ui.set_playback_ready(true);
        let playback = Rc::new(Cell::new(0));
        let observed_playback = Rc::clone(&playback);
        ui.on_playback_command(move |_| observed_playback.set(observed_playback.get() + 1));

        ui.show().expect("test UI should show");
        window.draw_if_needed(|_| {});
        press_key(&ui, Key::Control);
        press_key(&ui, "f");
        release_key(&ui, "f");
        release_key(&ui, Key::Control);
        press_key(&ui, " ");

        assert_eq!(playback.get(), 0);
        assert_eq!(ui.get_search_query().as_str(), " ");
    }

    #[test]
    fn disabled_track_does_not_emit_a_switch_intent() {
        let window = setup_window();
        let ui = K3Window::new().expect("test UI should construct");
        ui.set_setup_mode(false);
        ui.set_playback_ready(true);
        ui.set_accompaniment_available(false);
        let switches = Rc::new(Cell::new(0));
        let observed_switches = Rc::clone(&switches);
        ui.on_switch_track(move |_| observed_switches.set(observed_switches.get() + 1));

        ui.show().expect("test UI should show");
        window.draw_if_needed(|_| {});
        click(&ui, LogicalPosition::new(162.0, 759.0));

        assert_eq!(switches.get(), 0);
    }

    #[test]
    fn escape_closes_only_the_topmost_transient_layer() {
        let window = setup_window();
        let ui = K3Window::new().expect("test UI should construct");
        ui.set_setup_mode(true);
        ui.set_projects_root(SharedString::from("/tmp/projects"));
        ui.set_confirm_root_change(true);
        ui.set_show_about(true);

        ui.show().expect("test UI should show");
        window.draw_if_needed(|_| {});
        press_key(&ui, Key::Escape);
        assert!(!ui.get_show_about());
        assert!(ui.get_confirm_root_change());
        assert!(ui.get_setup_mode());

        press_key(&ui, Key::Escape);
        assert!(!ui.get_confirm_root_change());
        assert!(ui.get_setup_mode());

        press_key(&ui, Key::Escape);
        assert!(!ui.get_setup_mode());
    }

    #[test]
    fn major_playback_states_render_without_losing_the_project() {
        let window = setup_window();
        let ui = K3Window::new().expect("test UI should construct");
        ui.set_setup_mode(false);
        ui.set_playback_ready(true);
        ui.set_song_title(SharedString::from("State fixture"));
        ui.set_lyrics(ModelRc::new(VecModel::from(vec![LyricItem {
            text: SharedString::from("Current lyric"),
            at_seconds: 1.0,
        }])));

        ui.show().expect("test UI should show");
        for (playing, has_error, changed) in [
            (false, false, false),
            (true, false, false),
            (false, true, false),
            (false, false, true),
        ] {
            ui.set_playing(playing);
            ui.set_has_error(has_error);
            ui.set_project_changed(changed);
            window.request_redraw();
            window.draw_if_needed(|_| {});
            assert_eq!(ui.get_song_title().as_str(), "State fixture");
        }
    }

    #[test]
    fn playback_transitions_are_logged_once_per_state_change() {
        let mut tracker = PlaybackTransitionTracker::default();
        let mut snapshot = PlaybackSnapshot {
            status: PlaybackStatus::Paused,
            track: Some(TrackKind::Accompaniment),
            ..PlaybackSnapshot::default()
        };

        assert_eq!(
            tracker.observe(&snapshot).as_deref(),
            Some("playback state: status=paused track=accompaniment")
        );
        assert_eq!(tracker.observe(&snapshot), None);

        snapshot.status = PlaybackStatus::Playing;
        assert_eq!(
            tracker.observe(&snapshot).as_deref(),
            Some("playback state: status=playing track=accompaniment")
        );
    }

    #[test]
    fn window_and_thousand_project_first_frame_stay_within_budget() {
        let window = setup_window();
        let started = Instant::now();
        let ui = K3Window::new().expect("test UI should construct");
        ui.set_setup_mode(false);
        ui.set_projects(ModelRc::new(VecModel::from(
            (0..1_000)
                .map(|index| ProjectItem {
                    id: format!("project-{index:04}").into(),
                    title: format!("Project {index:04}").into(),
                    path: format!("/tmp/project-{index:04}").into(),
                    status: SharedString::from("Ready"),
                    selected: index == 0,
                })
                .collect::<Vec<_>>(),
        )));
        ui.show().expect("test UI should show");
        window.draw_if_needed(|_| {});

        assert!(
            started.elapsed() < Duration::from_secs(1),
            "1000-project first frame took {:?}",
            started.elapsed()
        );
    }
}
