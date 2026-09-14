use std::{
    collections::VecDeque,
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
    separation::{self, Profile, SeparationRequest},
    settings::{GuiSettings, SettingsWriter, SettingsWriterHandle},
    ui_text,
};
use k3_app::{
    GuiRecordingController, LoadedProject, LyricsChoice, LyricsSearch, PlaybackCommand,
    PlaybackService, PlaybackSnapshot, PlaybackStatus, ProjectLibrary, ProjectRevision,
    ProjectSummary, RodioBackend, TrackKind, default_project_lyrics_query, find_project_lyrics,
    lyric_countdown, save_project_lyrics,
};
use slint::winit_030::{EventResult, WinitWindowAccessor, winit};
use slint::{ComponentHandle, LogicalSize, ModelRc, SharedString, VecModel};
use uuid::Uuid;

mod netease_gui;

slint::include_modules!();

struct AppData {
    settings: GuiSettings,
    settings_writable: bool,
    projects: Vec<ProjectSummary>,
    selected_id: Option<Uuid>,
    pending_open_id: Option<Uuid>,
    selected_document_revision: Option<ProjectRevision>,
    presented_project_generation: u64,
    scan_generation: u64,
    open_generation: u64,
    lyrics_generation: u64,
    lyrics_context: Option<LyricsContext>,
    lyrics_choices: Vec<LyricsChoice>,
    separation_running: bool,
    queued_netease_separations: VecDeque<QueuedSeparation>,
}

struct QueuedSeparation {
    source: PathBuf,
    profile_index: i32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct LyricsContext {
    generation: u64,
    project_id: Uuid,
    project_root: PathBuf,
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
        pending_open_id: None,
        selected_document_revision: None,
        presented_project_generation: 0,
        scan_generation: 0,
        open_generation: 0,
        lyrics_generation: 0,
        lyrics_context: None,
        lyrics_choices: Vec::new(),
        separation_running: false,
        queued_netease_separations: VecDeque::new(),
    }));
    let (settings_writer, settings_writer_handle) = SettingsWriter::start()?;
    let playback = Arc::new(PlaybackService::start(RodioBackend::default()));
    let recording = Arc::new(Mutex::new(GuiRecordingController::default()));
    let _ = playback.execute(PlaybackCommand::SetVolume(initial_volume));
    let snapshots = playback.subscribe()?;
    install_callbacks(&ui, &data, &playback, &recording, &settings_writer_handle);
    let netease_cancelled = netease_gui::install(&ui, &data);
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
    netease_cancelled.store(true, Ordering::Release);
    if let Ok(mut recording) = recording.lock() {
        recording.abort();
    }
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
    recording: &Arc<Mutex<GuiRecordingController>>,
    settings_writer: &SettingsWriterHandle,
) {
    install_library_callbacks(ui, data, playback, recording, settings_writer);
    install_playback_callbacks(ui, data, playback, recording, settings_writer);
    install_recording_callbacks(ui, data, playback, recording);
    install_lyrics_callbacks(ui, data, recording);
    install_separation_callbacks(ui, data, settings_writer);
}

fn install_lyrics_callbacks(
    ui: &K3Window,
    data: &Arc<Mutex<AppData>>,
    recording: &Arc<Mutex<GuiRecordingController>>,
) {
    install_begin_lyrics_callback(ui, data, recording);
    install_close_lyrics_callback(ui, data);
    install_search_lyrics_callback(ui, data, recording);
    install_save_lyrics_callback(ui, data, recording);
}

fn install_begin_lyrics_callback(
    ui: &K3Window,
    data: &Arc<Mutex<AppData>>,
    recording: &Arc<Mutex<GuiRecordingController>>,
) {
    let ui = ui.as_weak();
    let data = Arc::clone(data);
    let recording = Arc::clone(recording);
    ui.unwrap().on_begin_lyrics_search(move || {
        if recording
            .lock()
            .is_ok_and(|recording| recording.is_recording())
        {
            return;
        }
        let context = data
            .lock()
            .ok()
            .and_then(|mut data| begin_lyrics_context(&mut data));
        let Some(context) = context else {
            return;
        };
        let fallback = ui
            .upgrade()
            .map_or_else(String::new, |ui| ui.get_song_title().to_string());
        if let Some(ui) = ui.upgrade() {
            ui.set_lyrics_query(fallback.clone().into());
            ui.set_lyrics_candidates(ModelRc::new(VecModel::default()));
            ui.set_selected_lyrics_candidate(-1);
            ui.set_lyrics_panel_state(LyricsPanelState::Preparing);
            ui.set_lyrics_panel_message("Reading project and audio metadata…".into());
            ui.set_overlay(Overlay::LyricsSearch);
        }
        let ui = ui.clone();
        let data = Arc::clone(&data);
        thread::spawn(move || {
            let query = default_project_lyrics_query(&context.project_root).unwrap_or(fallback);
            let _ = slint::invoke_from_event_loop(move || {
                let Some(ui) = ui.upgrade() else {
                    return;
                };
                if !lyrics_context_matches(&data, &context) {
                    return;
                }
                ui.set_lyrics_query(query.into());
                ui.set_lyrics_panel_state(LyricsPanelState::Idle);
                ui.set_lyrics_panel_message(SharedString::default());
            });
        });
    });
}

fn install_close_lyrics_callback(ui: &K3Window, data: &Arc<Mutex<AppData>>) {
    let data = Arc::clone(data);
    ui.on_close_lyrics_search(move || {
        if let Ok(mut data) = data.lock() {
            invalidate_lyrics_context(&mut data);
        }
    });
}

fn install_search_lyrics_callback(
    ui: &K3Window,
    data: &Arc<Mutex<AppData>>,
    recording: &Arc<Mutex<GuiRecordingController>>,
) {
    let ui = ui.as_weak();
    let data = Arc::clone(data);
    let recording = Arc::clone(recording);
    ui.unwrap().on_search_online_lyrics(move |query| {
        if recording
            .lock()
            .is_ok_and(|recording| recording.is_recording())
        {
            return;
        }
        let query = query.trim().to_owned();
        if query.is_empty() {
            return;
        }
        let context = data
            .lock()
            .ok()
            .and_then(|mut data| begin_lyrics_context(&mut data));
        let Some(context) = context else {
            return;
        };
        if let Some(ui) = ui.upgrade() {
            ui.set_lyrics_panel_state(LyricsPanelState::Searching);
            ui.set_lyrics_panel_message(format!("Searching online lyrics: {query}").into());
            ui.set_lyrics_candidates(ModelRc::new(VecModel::default()));
            ui.set_selected_lyrics_candidate(-1);
        }
        let ui = ui.clone();
        let data = Arc::clone(&data);
        thread::spawn(move || {
            let progress_ui = ui.clone();
            let progress_data = Arc::clone(&data);
            let progress_context = context.clone();
            let result =
                find_project_lyrics(&context.project_root, &query, true, &mut |progress| {
                    let ui = progress_ui.clone();
                    let data = Arc::clone(&progress_data);
                    let context = progress_context.clone();
                    let message = progress.to_string();
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(ui) = ui.upgrade()
                            && lyrics_context_matches(&data, &context)
                            && ui.get_overlay() == Overlay::LyricsSearch
                            && ui.get_lyrics_panel_state() == LyricsPanelState::Searching
                        {
                            ui.set_lyrics_panel_message(message.into());
                        }
                    });
                })
                .map_err(|error| error.to_string());
            let _ = slint::invoke_from_event_loop(move || {
                apply_lyrics_search_result(&ui, &data, &context, result);
            });
        });
    });
}

fn apply_lyrics_search_result(
    ui: &slint::Weak<K3Window>,
    data: &Mutex<AppData>,
    context: &LyricsContext,
    result: Result<LyricsSearch, String>,
) {
    let Some(ui) = ui.upgrade() else {
        return;
    };
    let Ok(mut data) = data.lock() else {
        return;
    };
    if !lyrics_context_is_current(&data, context) || ui.get_overlay() != Overlay::LyricsSearch {
        return;
    }
    match result {
        Ok(LyricsSearch::Candidates(choices)) => {
            let items = choices
                .iter()
                .map(|choice| LyricsCandidateItem {
                    label: choice.label().into(),
                    origin: choice.origin_label().into(),
                    preview: choice.preview_lines(3).join("\n").into(),
                })
                .collect::<Vec<_>>();
            let count = items.len();
            data.lyrics_choices = choices;
            ui.set_lyrics_candidates(ModelRc::new(VecModel::from(items)));
            ui.set_selected_lyrics_candidate(0);
            ui.set_lyrics_panel_state(LyricsPanelState::Results);
            ui.set_lyrics_panel_message(format!("Found {count} synced lyric versions").into());
        }
        Ok(LyricsSearch::NotFound) => {
            ui.set_lyrics_panel_state(LyricsPanelState::Idle);
            ui.set_lyrics_panel_message("No duration-matched synced lyrics found".into());
        }
        Ok(LyricsSearch::AlreadyPresent) => {
            ui.set_lyrics_panel_state(LyricsPanelState::Idle);
            ui.set_lyrics_panel_message("Current lyrics are already available".into());
        }
        Err(error) => {
            ui.set_lyrics_panel_state(LyricsPanelState::Idle);
            ui.set_lyrics_panel_message(format!("Lyrics search failed: {error}").into());
        }
    }
}

fn install_save_lyrics_callback(
    ui: &K3Window,
    data: &Arc<Mutex<AppData>>,
    recording: &Arc<Mutex<GuiRecordingController>>,
) {
    let ui = ui.as_weak();
    let data = Arc::clone(data);
    let recording = Arc::clone(recording);
    ui.unwrap().on_save_lyrics_candidate(move |index| {
        if recording
            .lock()
            .is_ok_and(|recording| recording.is_recording())
        {
            return;
        }
        let Some(index) = usize::try_from(index).ok() else {
            return;
        };
        let selection = data.lock().ok().and_then(|data| {
            let context = data.lyrics_context.clone()?;
            if !lyrics_context_is_current(&data, &context) {
                return None;
            }
            data.lyrics_choices
                .get(index)
                .cloned()
                .map(|choice| (context, choice))
        });
        let Some((context, choice)) = selection else {
            return;
        };
        if let Some(ui) = ui.upgrade() {
            ui.set_lyrics_panel_state(LyricsPanelState::Saving);
            ui.set_lyrics_panel_message("Saving synchronized lyrics…".into());
        }
        let ui = ui.clone();
        let data = Arc::clone(&data);
        thread::spawn(move || {
            let result = save_project_lyrics(&context.project_root, choice, &mut |_| {})
                .map_err(|error| error.to_string());
            let _ = slint::invoke_from_event_loop(move || {
                let Some(ui) = ui.upgrade() else {
                    return;
                };
                if !lyrics_context_matches(&data, &context) {
                    return;
                }
                match result {
                    Ok(saved) => {
                        if let Ok(mut data) = data.lock() {
                            invalidate_lyrics_context(&mut data);
                        }
                        ui.set_overlay(Overlay::None);
                        ui.set_lyrics_panel_state(LyricsPanelState::Idle);
                        ui.set_recording_message(
                            format!(
                                "Saved lyrics: {} - {} · {}",
                                saved.artist, saved.track, saved.origin
                            )
                            .into(),
                        );
                        ui.invoke_open_project(context.project_id.to_string().into());
                    }
                    Err(error) => {
                        ui.set_lyrics_panel_state(LyricsPanelState::Results);
                        ui.set_lyrics_panel_message(
                            format!("Cannot save synchronized lyrics: {error}").into(),
                        );
                    }
                }
            });
        });
    });
}

fn begin_lyrics_context(data: &mut AppData) -> Option<LyricsContext> {
    let project_id = data.selected_id?;
    let project_root = data
        .projects
        .iter()
        .find(|project| project.id == project_id)?
        .path
        .clone();
    data.lyrics_generation = data.lyrics_generation.wrapping_add(1);
    data.lyrics_choices.clear();
    let context = LyricsContext {
        generation: data.lyrics_generation,
        project_id,
        project_root,
    };
    data.lyrics_context = Some(context.clone());
    Some(context)
}

fn invalidate_lyrics_context(data: &mut AppData) {
    data.lyrics_generation = data.lyrics_generation.wrapping_add(1);
    data.lyrics_context = None;
    data.lyrics_choices.clear();
}

fn lyrics_context_is_current(data: &AppData, context: &LyricsContext) -> bool {
    data.lyrics_context.as_ref() == Some(context)
        && data.selected_id == Some(context.project_id)
        && data
            .projects
            .iter()
            .any(|project| project.id == context.project_id && project.path == context.project_root)
}

fn lyrics_context_matches(data: &Mutex<AppData>, context: &LyricsContext) -> bool {
    data.lock()
        .is_ok_and(|data| lyrics_context_is_current(&data, context))
}

fn install_separation_callbacks(
    ui: &K3Window,
    data: &Arc<Mutex<AppData>>,
    settings_writer: &SettingsWriterHandle,
) {
    install_choose_separation_source(ui, data);
    install_start_separation(ui, data, settings_writer);
}

fn install_choose_separation_source(ui: &K3Window, data: &Arc<Mutex<AppData>>) {
    let weak = ui.as_weak();
    let picker_data = Arc::clone(data);
    ui.on_choose_separation_source(move || {
        let weak = weak.clone();
        let picker_data = Arc::clone(&picker_data);
        thread::spawn(move || {
            let Some(source) = rfd::FileDialog::new()
                .add_filter(
                    "Audio",
                    &["flac", "mp3", "wav", "m4a", "ogg", "opus", "aac"],
                )
                .pick_file()
            else {
                return;
            };
            let root = picker_data
                .lock()
                .ok()
                .and_then(|data| data.settings.projects_root.clone());
            let result = root
                .as_deref()
                .ok_or_else(|| "Choose a projects folder first".to_string())
                .and_then(|root| separation::destination(&source, root));
            let _ = slint::invoke_from_event_loop(move || {
                let Some(ui) = weak.upgrade() else {
                    return;
                };
                if ui.get_separation_state() == SeparationState::Running {
                    return;
                }
                match result {
                    Ok((_, exists)) => {
                        ui.set_separation_source(path_text(&source));
                        ui.set_separation_existing(exists);
                        ui.set_separation_state(SeparationState::Idle);
                        ui.set_separation_message(SharedString::default());
                    }
                    Err(error) => {
                        ui.set_separation_state(SeparationState::Error);
                        ui.set_separation_message(error.into());
                    }
                }
            });
        });
    });
}

fn install_start_separation(
    ui: &K3Window,
    data: &Arc<Mutex<AppData>>,
    settings_writer: &SettingsWriterHandle,
) {
    let weak = ui.as_weak();
    let data = Arc::clone(data);
    let settings_writer = settings_writer.clone();
    ui.on_start_separation(move |source, profile_index, allow_replace| {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        let root = data
            .lock()
            .ok()
            .and_then(|data| data.settings.projects_root.clone());
        let Some(root) = root else {
            ui.set_separation_state(SeparationState::Error);
            ui.set_separation_message("Choose a projects folder first".into());
            return;
        };
        let request = SeparationRequest {
            source: PathBuf::from(source.as_str()),
            projects_root: root.clone(),
            profile: Profile::from_index(profile_index),
            allow_replace,
        };
        let (project, exists) = match separation::destination(&request.source, &root) {
            Ok(destination) => destination,
            Err(error) => {
                ui.set_separation_state(SeparationState::Error);
                ui.set_separation_message(error.into());
                return;
            }
        };
        if exists && !allow_replace {
            ui.set_separation_existing(true);
            ui.set_separation_state(SeparationState::Error);
            ui.set_separation_message(
                "This project already exists. Review the replacement warning and try again.".into(),
            );
            return;
        }
        let script = match separation::bundled_script() {
            Ok(script) => script,
            Err(error) => {
                ui.set_separation_state(SeparationState::Error);
                ui.set_separation_message(error.into());
                return;
            }
        };
        let Ok(mut state) = data.lock() else {
            ui.set_separation_state(SeparationState::Error);
            ui.set_separation_message("Could not access the project library".into());
            return;
        };
        if state.separation_running {
            return;
        }
        if exists
            && state.projects.iter().any(|item| {
                (Some(item.id) == state.selected_id || Some(item.id) == state.pending_open_id)
                    && item.path == project
            })
        {
            ui.set_separation_state(SeparationState::Error);
            ui.set_separation_message(
                "Switch to another project before replacing the loaded song's stems.".into(),
            );
            return;
        }
        state.separation_running = true;
        drop(state);
        ui.set_separation_state(SeparationState::Running);
        ui.set_separation_message("Separating audio. This may take several minutes…".into());
        run_separation_task(
            weak.clone(),
            Arc::clone(&data),
            settings_writer.clone(),
            request,
            script,
        );
    });
}

fn run_separation_task(
    weak: slint::Weak<K3Window>,
    data: Arc<Mutex<AppData>>,
    settings_writer: SettingsWriterHandle,
    request: SeparationRequest,
    script: PathBuf,
) {
    thread::spawn(move || {
        let root = request.projects_root.clone();
        let result = separation::run(&request, &script);
        let _ = slint::invoke_from_event_loop(move || {
            if let Ok(mut state) = data.lock() {
                state.separation_running = false;
            }
            let Some(ui) = weak.upgrade() else {
                return;
            };
            match result {
                Ok(_) => {
                    ui.set_separation_state(SeparationState::Success);
                    ui.set_separation_message(
                        "Separation complete. Select the song in the project list.".into(),
                    );
                }
                Err(error) => {
                    ui.set_separation_state(SeparationState::Error);
                    ui.set_separation_message(error.into());
                }
            }
            scan_library(
                weak,
                Arc::clone(&data),
                settings_writer,
                root,
                ScanIntent::Refresh,
            );
            start_next_queued_separation(&ui, &data);
        });
    });
}

fn queue_netease_separation(
    ui: &K3Window,
    data: &Arc<Mutex<AppData>>,
    source: PathBuf,
    profile_index: i32,
) -> usize {
    let Ok(mut state) = data.lock() else {
        ui.set_separation_message("Could not access the separation queue".into());
        return 0;
    };
    state
        .queued_netease_separations
        .push_back(QueuedSeparation {
            source,
            profile_index,
        });
    drop(state);
    start_next_queued_separation(ui, data);
    update_separation_queue_count(ui, data)
}

fn start_next_queued_separation(ui: &K3Window, data: &Arc<Mutex<AppData>>) {
    if ui.get_recording_state() != RecordingState::Idle {
        update_separation_queue_count(ui, data);
        return;
    }
    loop {
        let next = {
            let Ok(mut state) = data.lock() else { return };
            if state.separation_running {
                let waiting = state.queued_netease_separations.len();
                drop(state);
                ui.set_netease_separation_queued_count(i32::try_from(waiting).unwrap_or(i32::MAX));
                return;
            }
            state.queued_netease_separations.pop_front()
        };
        let Some(next) = next else {
            ui.set_netease_separation_queued_count(0);
            return;
        };
        update_separation_queue_count(ui, data);
        ui.set_separation_source(path_text(&next.source));
        ui.set_separation_existing(false);
        ui.invoke_start_separation(path_text(&next.source), next.profile_index, false);
        if data.lock().is_ok_and(|state| state.separation_running) {
            return;
        }
    }
}

fn update_separation_queue_count(ui: &K3Window, data: &Arc<Mutex<AppData>>) -> usize {
    let count = data
        .lock()
        .map_or(0, |state| state.queued_netease_separations.len());
    ui.set_netease_separation_queued_count(i32::try_from(count).unwrap_or(i32::MAX));
    count
}

fn install_library_callbacks(
    ui: &K3Window,
    data: &Arc<Mutex<AppData>>,
    playback: &Arc<PlaybackService>,
    recording: &Arc<Mutex<GuiRecordingController>>,
    settings_writer: &SettingsWriterHandle,
) {
    install_choose_root_callback(ui);
    install_save_root_callback(ui, data, settings_writer);
    install_search_callback(ui, data);
    install_refresh_callback(ui, data, settings_writer);
    install_open_project_callback(ui, data, playback, recording);
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
            if ui.upgrade().is_some_and(|ui| {
                ui.get_netease_download_running() || ui.get_netease_queued_count() > 0
            }) {
                if let Some(ui) = ui.upgrade() {
                    show_error(
                        &ui,
                        "Wait for NetEase downloads to finish before changing folders",
                    );
                }
                return;
            }
            if data.lock().is_ok_and(|data| {
                data.separation_running || !data.queued_netease_separations.is_empty()
            }) {
                if let Some(ui) = ui.upgrade() {
                    show_error(
                        &ui,
                        "Wait for separation queue to finish before changing folders",
                    );
                }
                return;
            }
            let path = PathBuf::from(root.as_str());
            if !path.is_dir() {
                if let Some(ui) = ui.upgrade() {
                    show_error(&ui, ui_text::unreadable_projects_folder(&path));
                }
                return;
            }
            if let Ok(mut data) = data.lock() {
                invalidate_lyrics_context(&mut data);
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
    recording: &Arc<Mutex<GuiRecordingController>>,
) {
    {
        let ui = ui.as_weak();
        let data = Arc::clone(data);
        let playback = Arc::clone(playback);
        let recording = Arc::clone(recording);
        ui.unwrap().on_open_project(move |id| {
            if data.lock().is_ok_and(|data| data.separation_running) {
                return;
            }
            if recording
                .lock()
                .is_ok_and(|recording| recording.is_recording())
            {
                if let Some(ui) = ui.upgrade() {
                    show_error(&ui, "Stop the recording before switching projects");
                }
                return;
            }
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
                    data.pending_open_id = Some(id);
                    invalidate_lyrics_context(&mut data);
                }
                project.map(|project| (project, data.open_generation))
            });
            let Some((project, generation)) = project else {
                return;
            };
            if let Some(ui) = ui.upgrade() {
                begin_project_load(&ui);
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

fn begin_project_load(ui: &K3Window) {
    ui.set_loading(true);
    ui.set_error_kind(ErrorKind::None);
    ui.set_playback_state(PlaybackState::Loading);
    ui.set_position_seconds(0.0);
    ui.set_duration_seconds(0.0);
}

fn install_recording_callbacks(
    ui: &K3Window,
    data: &Arc<Mutex<AppData>>,
    playback: &Arc<PlaybackService>,
    recording: &Arc<Mutex<GuiRecordingController>>,
) {
    {
        let ui = ui.as_weak();
        let data = Arc::clone(data);
        let playback = Arc::clone(playback);
        let recording = Arc::clone(recording);
        ui.unwrap().on_recording_command(move |start| {
            if start {
                start_gui_recording(ui.clone(), &data, &playback, Arc::clone(&recording));
            } else {
                stop_gui_recording(ui.clone(), Arc::clone(&data), Arc::clone(&recording));
            }
        });
    }

    {
        let recording = Arc::clone(recording);
        ui.on_set_monitoring(move |enabled| {
            if let Ok(recording) = recording.lock() {
                recording.set_monitoring(enabled);
            }
        });
    }
}

fn start_gui_recording(
    ui: slint::Weak<K3Window>,
    data: &Arc<Mutex<AppData>>,
    playback: &PlaybackService,
    recording: Arc<Mutex<GuiRecordingController>>,
) {
    let project_root = data.lock().ok().and_then(|data| {
        let selected = data.selected_id?;
        data.projects
            .iter()
            .find(|project| project.id == selected)
            .map(|project| project.path.clone())
    });
    let Some(project_root) = project_root else {
        if let Some(ui) = ui.upgrade() {
            show_error(&ui, "Choose a project before recording");
        }
        return;
    };
    let monitoring = ui.upgrade().is_some_and(|ui| {
        ui.set_monitoring(true);
        true
    });
    let volume = ui.upgrade().map_or(1.0, |ui| ui.get_volume());
    let data = Arc::clone(data);
    if let Some(ui) = ui.upgrade() {
        ui.set_recording_state(RecordingState::Starting);
        ui.set_recording_message("Opening microphone…".into());
        if ui.get_playback_state() == PlaybackState::Playing {
            let _ = playback.dispatch(PlaybackCommand::Toggle);
        }
    }
    thread::spawn(move || {
        let result = recording
            .lock()
            .map_err(|_| "recording state is unavailable".to_owned())
            .and_then(|mut recording| {
                recording
                    .start(&project_root, monitoring, volume)
                    .map_err(|error| error.to_string())
            });
        let started = result.as_ref().ok().cloned();
        let event_ui = ui.clone();
        let _ = slint::invoke_from_event_loop(move || {
            let Some(ui) = event_ui.upgrade() else {
                return;
            };
            match result {
                Ok(started) => {
                    ui.set_recording_state(RecordingState::Recording);
                    ui.set_recording_message(format!("Recording · {}", started.device).into());
                    clear_general_error(&ui);
                }
                Err(error) => {
                    ui.set_recording_state(RecordingState::Idle);
                    ui.set_monitoring(false);
                    ui.set_recording_message(SharedString::default());
                    show_error(&ui, format!("Cannot start recording: {error}"));
                    start_next_queued_separation(&ui, &data);
                }
            }
        });
        if started.is_some() {
            pump_recording_snapshots(&ui, &recording);
        }
    });
}

fn pump_recording_snapshots(ui: &slint::Weak<K3Window>, recording: &Mutex<GuiRecordingController>) {
    loop {
        thread::sleep(Duration::from_millis(80));
        let snapshot = recording
            .lock()
            .ok()
            .and_then(|mut recording| recording.snapshot());
        let Some(snapshot) = snapshot else {
            break;
        };
        let ui = ui.clone();
        if slint::invoke_from_event_loop(move || {
            if let Some(ui) = ui.upgrade()
                && ui.get_recording_state() == RecordingState::Recording
            {
                apply_transport_snapshot(&ui, &snapshot);
            }
        })
        .is_err()
        {
            break;
        }
    }
}

fn stop_gui_recording(
    ui: slint::Weak<K3Window>,
    data: Arc<Mutex<AppData>>,
    recording: Arc<Mutex<GuiRecordingController>>,
) {
    if let Some(ui) = ui.upgrade() {
        ui.set_recording_state(RecordingState::Stopping);
        ui.set_monitoring(false);
        ui.set_recording_message("Saving recording…".into());
    }
    thread::spawn(move || {
        let result = recording
            .lock()
            .map_err(|_| "recording state is unavailable".to_owned())
            .and_then(|mut recording| recording.stop().map_err(|error| error.to_string()));
        let _ = slint::invoke_from_event_loop(move || {
            let Some(ui) = ui.upgrade() else {
                return;
            };
            ui.set_recording_state(RecordingState::Idle);
            match result {
                Ok(saved) => {
                    let warning = saved
                        .warning
                        .map_or_else(String::new, |warning| format!(" · Warning: {warning}"));
                    ui.set_recording_message(
                        format!(
                            "Saved {:.1}s take from {}{}",
                            saved.duration.as_secs_f32(),
                            saved.device,
                            warning
                        )
                        .into(),
                    );
                    clear_general_error(&ui);
                    let project_id = ui.get_current_project_id();
                    if !project_id.is_empty() {
                        ui.invoke_open_project(project_id);
                    }
                }
                Err(error) => {
                    ui.set_recording_message(SharedString::default());
                    show_error(&ui, format!("Cannot save recording: {error}"));
                }
            }
            start_next_queued_separation(&ui, &data);
        });
    });
}

fn install_playback_callbacks(
    ui: &K3Window,
    data: &Arc<Mutex<AppData>>,
    playback: &Arc<PlaybackService>,
    recording: &Arc<Mutex<GuiRecordingController>>,
    settings_writer: &SettingsWriterHandle,
) {
    install_volume_callbacks(ui, data, playback, recording, settings_writer);

    {
        let ui = ui.as_weak();
        let playback = Arc::clone(playback);
        let recording = Arc::clone(recording);
        ui.unwrap().on_playback_command(move |command| {
            if recording_controls_active(&ui) {
                let command = match command {
                    PlaybackAction::Toggle => PlaybackCommand::Toggle,
                    PlaybackAction::Back => PlaybackCommand::SeekBy(-5),
                    PlaybackAction::Forward => PlaybackCommand::SeekBy(5),
                    PlaybackAction::Restart => PlaybackCommand::Restart,
                    PlaybackAction::Retry => PlaybackCommand::Retry,
                };
                dispatch_recording(&ui, &recording, command);
                return;
            }
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
        let recording = Arc::clone(recording);
        ui.unwrap().on_seek_to(move |seconds| {
            let command = PlaybackCommand::SeekTo(Duration::from_secs_f32(seconds.max(0.0)));
            if recording_controls_active(&ui) {
                dispatch_recording(&ui, &recording, command);
                return;
            }
            dispatch_playback(&ui, &playback, command);
        });
    }

    {
        let ui = ui.as_weak();
        let playback = Arc::clone(playback);
        let recording = Arc::clone(recording);
        ui.unwrap().on_seek_lyric(move |seconds| {
            let command = PlaybackCommand::SeekTo(Duration::from_secs_f32(seconds.max(0.0)));
            if recording_controls_active(&ui) {
                dispatch_recording(&ui, &recording, command);
                return;
            }
            dispatch_playback(&ui, &playback, command);
        });
    }

    {
        let ui = ui.as_weak();
        let playback = Arc::clone(playback);
        let recording = Arc::clone(recording);
        ui.unwrap().on_set_key(move |semitones| {
            let Ok(semitones) = i8::try_from(semitones) else {
                return;
            };
            if recording_controls_active(&ui) {
                dispatch_recording(&ui, &recording, PlaybackCommand::SetKeyShift(semitones));
                return;
            }
            dispatch_playback(&ui, &playback, PlaybackCommand::SetKeyShift(semitones));
        });
    }

    {
        let ui = ui.as_weak();
        let playback = Arc::clone(playback);
        let recording = Arc::clone(recording);
        ui.unwrap().on_switch_track(move |track| {
            let track = match track {
                TrackSelection::Original => TrackKind::Original,
                TrackSelection::Accompaniment => TrackKind::Accompaniment,
                TrackSelection::Vocals => TrackKind::Vocals,
                TrackSelection::Take => TrackKind::Take,
            };
            if recording_controls_active(&ui) {
                dispatch_recording(&ui, &recording, PlaybackCommand::SwitchTrack(track));
            }
            dispatch_playback(&ui, &playback, PlaybackCommand::SwitchTrack(track));
        });
    }
}

fn install_volume_callbacks(
    ui: &K3Window,
    data: &Arc<Mutex<AppData>>,
    playback: &Arc<PlaybackService>,
    recording: &Arc<Mutex<GuiRecordingController>>,
    settings_writer: &SettingsWriterHandle,
) {
    {
        let ui = ui.as_weak();
        let playback = Arc::clone(playback);
        let recording = Arc::clone(recording);
        let data = Arc::clone(data);
        ui.unwrap().on_set_volume(move |volume| {
            if let Some(ui) = ui.upgrade() {
                ui.set_volume(volume.clamp(0.0, 1.0));
            }
            if let Ok(mut data) = data.lock() {
                data.settings.volume = volume.clamp(0.0, 1.0);
            }
            if let Ok(mut recording) = recording.lock() {
                let _ = recording.execute(PlaybackCommand::SetVolume(volume));
            }
            dispatch_playback(&ui, &playback, PlaybackCommand::SetVolume(volume));
        });
    }

    {
        let ui = ui.as_weak();
        let data = Arc::clone(data);
        let settings_writer = settings_writer.clone();
        ui.unwrap().on_save_volume(move |volume| {
            if let Ok(mut data) = data.lock() {
                data.settings.volume = volume.clamp(0.0, 1.0);
                if data.settings_writable {
                    settings_writer.persist(data.settings.clone());
                }
            }
        });
    }
}

fn dispatch_recording(
    ui: &slint::Weak<K3Window>,
    recording: &Mutex<GuiRecordingController>,
    command: PlaybackCommand,
) {
    let snapshot = recording
        .lock()
        .ok()
        .and_then(|mut recording| recording.execute(command));
    if let (Some(ui), Some(snapshot)) = (ui.upgrade(), snapshot) {
        apply_transport_snapshot(&ui, &snapshot);
    }
}

fn recording_controls_active(ui: &slint::Weak<K3Window>) -> bool {
    ui.upgrade()
        .is_some_and(|ui| ui.get_recording_state() == RecordingState::Recording)
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
                                invalidate_lyrics_context(&mut data);
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
                        ui.set_separation_source(SharedString::default());
                        ui.set_separation_existing(false);
                        ui.set_separation_state(SeparationState::Idle);
                        ui.set_separation_message(SharedString::default());
                        ui.set_separation_panel_open(false);
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
                    clear_pending_open(&data, generation);
                    ui.set_loading(false);
                    show_error(&ui, error.to_string());
                    return;
                }
            };
            match playback.dispatch(PlaybackCommand::Load(loaded)) {
                Ok(()) => {}
                Err(error) => {
                    clear_pending_open(&data, generation);
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

fn clear_pending_open(data: &Mutex<AppData>, generation: u64) {
    if let Ok(mut data) = data.lock()
        && data.open_generation == generation
    {
        data.pending_open_id = None;
    }
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
        if !snapshot_matches_pending_open(data.pending_open_id, project_id) {
            return;
        }
        data.pending_open_id = None;
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

fn snapshot_matches_pending_open(pending_open_id: Option<Uuid>, snapshot_id: Uuid) -> bool {
    pending_open_id.is_none_or(|pending_open_id| pending_open_id == snapshot_id)
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
    ui.set_take_available(track_available(snapshot, TrackKind::Take));
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
    if ui.get_recording_state() != RecordingState::Idle {
        return;
    }
    apply_transport_snapshot(ui, snapshot);
}

fn apply_transport_snapshot(ui: &K3Window, snapshot: &PlaybackSnapshot) {
    ui.set_position_seconds(snapshot.position.as_secs_f32());
    ui.set_duration_seconds(snapshot.duration.map_or(0.0, |value| value.as_secs_f32()));
    ui.set_volume(snapshot.volume);
    ui.set_key_shift(i32::from(snapshot.key_shift_semitones));
    ui.set_playback_state(playback_state(snapshot.status));
    if let Some(track) = snapshot.track {
        ui.set_active_track(match track {
            TrackKind::Original => TrackSelection::Original,
            TrackKind::Accompaniment => TrackSelection::Accompaniment,
            TrackKind::Vocals => TrackSelection::Vocals,
            TrackKind::Take => TrackSelection::Take,
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
