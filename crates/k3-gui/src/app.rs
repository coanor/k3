use std::{
    error::Error,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{Receiver, RecvTimeoutError},
    },
    thread,
    time::Duration,
};

use crate::{
    logging::DiagnosticLog,
    settings::{GuiSettings, SettingsWriter, SettingsWriterHandle},
    ui_text,
};
use k3_app::{
    LoadedProject, PlaybackCommand, PlaybackService, PlaybackSnapshot, PlaybackStatus,
    ProjectLibrary, ProjectRevision, ProjectSummary, RodioBackend, TrackKind, lyric_countdown,
};
use slint::winit_030::{EventResult, WinitWindowAccessor, winit};
use slint::{ComponentHandle, LogicalSize, ModelRc, SharedString, VecModel};
use uuid::Uuid;

slint::include_modules!();

struct AppData {
    settings: GuiSettings,
    settings_writable: bool,
    projects: Vec<ProjectSummary>,
    selected_id: Option<Uuid>,
    selected_document_revision: Option<ProjectRevision>,
    presented_project_generation: u64,
    scan_generation: u64,
    open_generation: u64,
}

pub fn launch() {
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
    ui.set_view_mode(if settings.projects_root.is_none() {
        ViewMode::Setup
    } else {
        ViewMode::Rehearsal
    });

    let data = Arc::new(Mutex::new(AppData {
        settings,
        settings_writable,
        projects: Vec::new(),
        selected_id: None,
        selected_document_revision: None,
        presented_project_generation: 0,
        scan_generation: 0,
        open_generation: 0,
    }));
    let (settings_writer, settings_writer_handle) = SettingsWriter::start()?;
    let playback = Arc::new(PlaybackService::start(RodioBackend::default()));
    let _ = playback.execute(PlaybackCommand::SetVolume(initial_volume));
    let snapshots = playback.subscribe()?;
    install_callbacks(&ui, &data, &playback, &settings_writer_handle);
    install_focus_refresh(&ui, &data, &settings_writer_handle);

    let pump_running = Arc::new(AtomicBool::new(true));
    let snapshot_listener = start_snapshot_listener(
        ui.as_weak(),
        Arc::clone(&data),
        settings_writer_handle.clone(),
        snapshots,
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
            ScanIntent::Startup,
        );
    }

    let ui_result = ui.run();
    pump_running.store(false, Ordering::Release);
    let _ = snapshot_listener.join();
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
                        ScanIntent::Refresh,
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

fn install_library_callbacks(
    ui: &K3Window,
    data: &Arc<Mutex<AppData>>,
    playback: &Arc<PlaybackService>,
    settings_writer: &SettingsWriterHandle,
) {
    install_choose_root_callback(ui);
    install_save_root_callback(ui, data, settings_writer);
    install_search_callback(ui, data);
    install_refresh_callback(ui, data, settings_writer);
    install_open_project_callback(ui, data, playback);
}

fn install_choose_root_callback(ui: &K3Window) {
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
}

fn install_save_root_callback(
    ui: &K3Window,
    data: &Arc<Mutex<AppData>>,
    settings_writer: &SettingsWriterHandle,
) {
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
                ui.set_error_kind(ErrorKind::None);
            }
            scan_library(
                ui.clone(),
                Arc::clone(&data),
                settings_writer.clone(),
                path,
                ScanIntent::AdoptRoot,
            );
        });
    }
}

fn install_search_callback(ui: &K3Window, data: &Arc<Mutex<AppData>>) {
    {
        let ui = ui.as_weak();
        let data = Arc::clone(data);
        ui.unwrap().on_search(move |query| {
            if let Some(ui) = ui.upgrade() {
                publish_projects(&ui, &data, query.as_str());
            }
        });
    }
}

fn install_refresh_callback(
    ui: &K3Window,
    data: &Arc<Mutex<AppData>>,
    settings_writer: &SettingsWriterHandle,
) {
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
                    ScanIntent::Refresh,
                );
            }
        });
    }
}

fn install_open_project_callback(
    ui: &K3Window,
    data: &Arc<Mutex<AppData>>,
    playback: &Arc<PlaybackService>,
) {
    {
        let ui = ui.as_weak();
        let data = Arc::clone(data);
        let playback = Arc::clone(playback);
        ui.unwrap().on_open_project(move |id| {
            let Ok(id) = Uuid::parse_str(id.as_str()) else {
                return;
            };
            let project = data.lock().ok().and_then(|mut data| {
                let project = data
                    .projects
                    .iter()
                    .find(|project| project.id == id)
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
                ui.set_error_kind(ErrorKind::None);
                ui.set_playback_state(PlaybackState::Loading);
            }
            open_project(
                ui.clone(),
                Arc::clone(&data),
                Arc::clone(&playback),
                generation,
                project,
            );
        });
    }
}

fn install_playback_callbacks(
    ui: &K3Window,
    data: &Arc<Mutex<AppData>>,
    playback: &Arc<PlaybackService>,
    settings_writer: &SettingsWriterHandle,
) {
    {
        let ui = ui.as_weak();
        let playback = Arc::clone(playback);
        ui.unwrap().on_playback_command(move |command| {
            let command = match command {
                PlaybackAction::Toggle => PlaybackCommand::Toggle,
                PlaybackAction::Back => PlaybackCommand::SeekBy(-5),
                PlaybackAction::Forward => PlaybackCommand::SeekBy(5),
                PlaybackAction::Restart => PlaybackCommand::Restart,
                PlaybackAction::Retry => PlaybackCommand::Retry,
            };
            dispatch_playback(&ui, &playback, command);
        });
    }

    {
        let ui = ui.as_weak();
        let playback = Arc::clone(playback);
        ui.unwrap().on_seek_to(move |seconds| {
            dispatch_playback(
                &ui,
                &playback,
                PlaybackCommand::SeekTo(Duration::from_secs_f32(seconds.max(0.0))),
            );
        });
    }

    {
        let ui = ui.as_weak();
        let playback = Arc::clone(playback);
        ui.unwrap().on_seek_lyric(move |seconds| {
            dispatch_playback(
                &ui,
                &playback,
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
            dispatch_playback(&ui, &playback, PlaybackCommand::SetVolume(volume));
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
        ui.unwrap().on_set_key(move |semitones| {
            let Ok(semitones) = i8::try_from(semitones) else {
                return;
            };
            dispatch_playback(&ui, &playback, PlaybackCommand::SetKeyShift(semitones));
        });
    }

    {
        let ui = ui.as_weak();
        let playback = Arc::clone(playback);
        ui.unwrap().on_switch_track(move |track| {
            let track = match track {
                TrackSelection::Original => TrackKind::Original,
                TrackSelection::Accompaniment => TrackKind::Accompaniment,
                TrackSelection::Vocals => TrackKind::Vocals,
            };
            dispatch_playback(&ui, &playback, PlaybackCommand::SwitchTrack(track));
        });
    }
}

#[derive(Clone, Copy)]
enum ScanIntent {
    Startup,
    Refresh,
    AdoptRoot,
}

impl ScanIntent {
    const fn reopens_last_project(self) -> bool {
        matches!(self, Self::Startup)
    }

    const fn adopts_root(self) -> bool {
        matches!(self, Self::AdoptRoot)
    }
}

fn scan_library(
    ui: slint::Weak<K3Window>,
    data: Arc<Mutex<AppData>>,
    settings_writer: SettingsWriterHandle,
    root: PathBuf,
    intent: ScanIntent,
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
                            if intent.adopts_root() {
                                data.settings.projects_root = Some(root.clone());
                                data.settings.last_project_id = None;
                                data.selected_id = None;
                                data.selected_document_revision = None;
                            }
                            let last_project = if intent.reopens_last_project() {
                                data.settings.last_project_id.clone()
                            } else {
                                None
                            };
                            let project_changed = data.selected_id.as_ref().is_some_and(|id| {
                                projects
                                    .iter()
                                    .find(|project| project.id == *id)
                                    .is_none_or(|project| {
                                        project.document_revision != data.selected_document_revision
                                    })
                            });
                            let settings = (intent.adopts_root() && data.settings_writable)
                                .then(|| data.settings.clone());
                            data.projects = projects;
                            Some((last_project, project_changed, settings))
                        })
                    else {
                        return;
                    };
                    ui.set_loading(false);
                    if intent.adopts_root() {
                        ui.set_projects_root(path_text(&root));
                    }
                    ui.set_view_mode(ViewMode::Rehearsal);
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
                    if intent.adopts_root() {
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
            match playback.dispatch(PlaybackCommand::Load(loaded)) {
                Ok(()) => {}
                Err(error) => {
                    ui.set_loading(false);
                    show_playback_error(&ui, error.to_string());
                }
            }
        });
    });
}

fn is_current_open(data: &Mutex<AppData>, generation: u64) -> bool {
    data.lock()
        .is_ok_and(|data| data.open_generation == generation)
}

fn dispatch_playback(
    ui: &slint::Weak<K3Window>,
    playback: &PlaybackService,
    command: PlaybackCommand,
) {
    if let Err(error) = playback.dispatch(command)
        && let Some(ui) = ui.upgrade()
    {
        show_playback_error(&ui, error.to_string());
    }
}

fn start_snapshot_listener(
    ui: slint::Weak<K3Window>,
    data: Arc<Mutex<AppData>>,
    settings_writer: SettingsWriterHandle,
    snapshots: Receiver<PlaybackSnapshot>,
    running: Arc<AtomicBool>,
) -> thread::JoinHandle<()> {
    thread::Builder::new()
        .name("k3-gui-snapshots".into())
        .spawn(move || {
            let mut transitions = PlaybackTransitionTracker::default();
            while running.load(Ordering::Acquire) {
                let snapshot = match snapshots.recv_timeout(Duration::from_millis(100)) {
                    Ok(snapshot) => snapshot,
                    Err(RecvTimeoutError::Timeout) => continue,
                    Err(RecvTimeoutError::Disconnected) => break,
                };
                if let Some(message) = transitions.observe(&snapshot)
                    && let Ok(log) = DiagnosticLog::initialize()
                {
                    log.record(message);
                }
                let data = Arc::clone(&data);
                let settings_writer = settings_writer.clone();
                let _ = ui.upgrade_in_event_loop(move |ui| {
                    apply_published_snapshot(&ui, &data, &settings_writer, &snapshot);
                });
            }
        })
        .expect("failed to start GUI snapshot pump")
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct PlaybackTransition {
    project_id: Option<Uuid>,
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
            project_id: snapshot.project_id,
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
            next.track.map_or("none", track_log_label)
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

const fn track_log_label(track: TrackKind) -> &'static str {
    match track {
        TrackKind::Original => "original",
        TrackKind::Accompaniment => "accompaniment",
        TrackKind::Vocals => "vocals",
        TrackKind::Take => "take",
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
                ProjectState::Ready
            } else {
                ProjectState::RepairNeeded
            },
            selected: data.selected_id == Some(project.id),
        })
        .collect::<Vec<_>>();
    ui.set_projects(ModelRc::new(VecModel::from(items)));
}

fn apply_published_snapshot(
    ui: &K3Window,
    data: &Mutex<AppData>,
    settings_writer: &SettingsWriterHandle,
    snapshot: &PlaybackSnapshot,
) {
    let Some(project_id) = snapshot.project_id else {
        return;
    };
    let mut selected_changed = false;
    let mut presentation_changed = false;
    let mut settings = None;
    if let Ok(mut data) = data.lock() {
        selected_changed = data.selected_id != Some(project_id);
        presentation_changed = should_apply_project_snapshot(
            data.selected_id,
            data.presented_project_generation,
            snapshot,
        );
        data.presented_project_generation = snapshot.project_generation;
        if selected_changed {
            data.selected_id = Some(project_id);
            data.settings.last_project_id = Some(project_id.to_string());
            settings = data.settings_writable.then(|| data.settings.clone());
        }
        if !snapshot.reload_required {
            data.selected_document_revision
                .clone_from(&snapshot.document_revision);
        }
    }
    ui.set_loading(false);
    if presentation_changed {
        ui.set_current_project_id(project_id.to_string().into());
        ui.set_project_changed(false);
        apply_project_snapshot(ui, snapshot);
    }
    if selected_changed {
        publish_projects(ui, data, ui.get_search_query().as_str());
        if let Some(settings) = settings {
            settings_writer.persist(settings);
        }
    }
    apply_snapshot(ui, snapshot);
}

fn should_apply_project_snapshot(
    selected_id: Option<Uuid>,
    presented_generation: u64,
    snapshot: &PlaybackSnapshot,
) -> bool {
    selected_id != snapshot.project_id || presented_generation != snapshot.project_generation
}

fn apply_project_snapshot(ui: &K3Window, snapshot: &PlaybackSnapshot) {
    ui.set_song_title(snapshot.title.as_deref().unwrap_or_default().into());
    ui.set_original_available(track_available(snapshot, TrackKind::Original));
    ui.set_accompaniment_available(track_available(snapshot, TrackKind::Accompaniment));
    ui.set_vocals_available(track_available(snapshot, TrackKind::Vocals));
    let lyrics = snapshot
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
    ui.set_playback_state(playback_state(snapshot.status));
    if let Some(track) = snapshot.track {
        ui.set_active_track(match track {
            TrackKind::Original | TrackKind::Take => TrackSelection::Original,
            TrackKind::Accompaniment => TrackSelection::Accompaniment,
            TrackKind::Vocals => TrackSelection::Vocals,
        });
    }
    if let Some(error) = &snapshot.error {
        show_playback_error(ui, error.clone());
    } else if ui.get_error_kind() == ErrorKind::Playback {
        ui.set_error_kind(ErrorKind::None);
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

fn track_available(snapshot: &PlaybackSnapshot, kind: TrackKind) -> bool {
    snapshot
        .tracks
        .iter()
        .find(|track| track.kind == kind)
        .is_some_and(k3_app::ProjectTrack::available)
}

const fn playback_state(status: PlaybackStatus) -> PlaybackState {
    match status {
        PlaybackStatus::Unavailable => PlaybackState::Unavailable,
        PlaybackStatus::Loading => PlaybackState::Loading,
        PlaybackStatus::Paused => PlaybackState::Paused,
        PlaybackStatus::Playing => PlaybackState::Playing,
        PlaybackStatus::Finished => PlaybackState::Finished,
        PlaybackStatus::Error => PlaybackState::Error,
    }
}

fn show_error(ui: &K3Window, message: impl Into<SharedString>) {
    let message = message.into();
    if let Ok(log) = DiagnosticLog::initialize() {
        log.record(format!("error: {message}"));
    }
    ui.set_error_kind(ErrorKind::General);
    ui.set_error_message(message);
}

fn clear_general_error(ui: &K3Window) {
    if ui.get_error_kind() != ErrorKind::Playback {
        ui.set_error_kind(ErrorKind::None);
        ui.set_error_message(SharedString::default());
    }
}

fn show_playback_error(ui: &K3Window, message: impl Into<SharedString>) {
    let message = message.into();
    if let Ok(log) = DiagnosticLog::initialize() {
        log.record(format!("playback error: {message}"));
    }
    ui.set_error_kind(ErrorKind::Playback);
    ui.set_error_message(message);
    ui.set_playback_state(PlaybackState::Error);
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
#[path = "interaction_tests.rs"]
mod interaction_tests;
