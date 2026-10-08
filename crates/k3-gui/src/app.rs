use std::{
    collections::{HashSet, VecDeque},
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
    i18n,
    logging::DiagnosticLog,
    separation::{self, Profile, SeparationRequest},
    settings::{
        GuiLanguage, GuiQualityModel, GuiSeparationDevice, GuiSeparationProfile, GuiSettings,
        SettingsWriter, SettingsWriterHandle,
    },
    ui_text,
};
use k3_app::{
    GuiRecordingController, LoadedProject, LyricsChoice, LyricsSearch, PlaybackCommand,
    PlaybackService, PlaybackSnapshot, PlaybackStatus, ProjectLibrary, ProjectRevision,
    ProjectSummary, RodioBackend, TrackKind, default_project_lyrics_query, ensure_take_render,
    find_project_lyrics, lyric_countdown, save_project_lyrics,
};
use k3_core::{FileProjectRepository, ProjectRepository, VocalEffectPreset};
use slint::winit_030::{EventResult, WinitWindowAccessor, winit};
use slint::{ComponentHandle, LogicalSize, ModelRc, SharedString, VecModel};
use uuid::Uuid;

mod netease_gui;
mod updates_gui;

slint::include_modules!();

struct AppData {
    settings: GuiSettings,
    settings_writable: bool,
    projects: Vec<ProjectSummary>,
    selected_id: Option<Uuid>,
    pending_open_id: Option<Uuid>,
    selected_document_revision: Option<ProjectRevision>,
    takes: Vec<TakeChoice>,
    selected_take_id: Option<String>,
    take_operation_running: bool,
    presented_project_generation: u64,
    discard_snapshots_through_generation: Option<u64>,
    scan_generation: u64,
    open_generation: u64,
    lyrics_generation: u64,
    lyrics_context: Option<LyricsContext>,
    lyrics_choices: Vec<LyricsChoice>,
    separation_running: bool,
    active_separation_project: Option<PathBuf>,
    reopen_after_separation: Option<Uuid>,
    selected_separation_sources: Vec<SelectedSeparation>,
    queued_separations: VecDeque<QueuedSeparation>,
    separation_completed: usize,
    separation_errors: Vec<String>,
}

impl AppData {
    fn project_is_separating(&self, id: Uuid) -> bool {
        self.active_separation_project.as_ref().is_some_and(|path| {
            self.projects
                .iter()
                .any(|project| project.id == id && project.path == *path)
        })
    }

    fn project_is_open(&self, path: &Path) -> bool {
        self.open_project_id(path).is_some()
    }

    fn open_project_id(&self, path: &Path) -> Option<Uuid> {
        self.projects
            .iter()
            .find(|project| {
                project.path == path
                    && (Some(project.id) == self.selected_id
                        || Some(project.id) == self.pending_open_id)
            })
            .map(|project| project.id)
    }
}

struct QueuedSeparation {
    source: PathBuf,
    project: PathBuf,
    profile_index: i32,
    quality_model_index: i32,
    allow_replace: bool,
}

struct SelectedSeparation {
    source: PathBuf,
    project: PathBuf,
    allow_replace: bool,
}

#[derive(Clone)]
struct TakeChoice {
    id: String,
}

struct PendingTakeDeletion {
    project_id: Uuid,
    project_root: PathBuf,
    take_id: String,
    revision: Option<ProjectRevision>,
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
        #[cfg(target_os = "windows")]
        {
            let mut message = ui_text::gui_start_failed(error.as_ref());
            if let Some(log) = log {
                message.push_str(&format!("\n{}", ui_text::diagnostics(log.path())));
            }
            rfd::MessageDialog::new()
                .set_title("K3")
                .set_level(rfd::MessageLevel::Error)
                .set_description(message)
                .show();
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
    configure_window(&ui, &settings)?;

    let data = Arc::new(Mutex::new(AppData {
        settings,
        settings_writable,
        projects: Vec::new(),
        selected_id: None,
        pending_open_id: None,
        selected_document_revision: None,
        takes: Vec::new(),
        selected_take_id: None,
        take_operation_running: false,
        presented_project_generation: 0,
        discard_snapshots_through_generation: None,
        scan_generation: 0,
        open_generation: 0,
        lyrics_generation: 0,
        lyrics_context: None,
        lyrics_choices: Vec::new(),
        separation_running: false,
        active_separation_project: None,
        reopen_after_separation: None,
        selected_separation_sources: Vec::new(),
        queued_separations: VecDeque::new(),
        separation_completed: 0,
        separation_errors: Vec::new(),
    }));
    let (settings_writer, settings_writer_handle) = SettingsWriter::start()?;
    let playback = Arc::new(PlaybackService::start(RodioBackend::default()));
    let recording = Arc::new(Mutex::new(GuiRecordingController::default()));
    let _ = playback.execute(PlaybackCommand::SetVolume(initial_volume));
    let snapshots = playback.subscribe()?;
    install_callbacks(&ui, &data, &playback, &recording, &settings_writer_handle);
    install_preference_callbacks(&ui, &data, &settings_writer_handle);
    let netease_cancelled = netease_gui::install(&ui, &data);
    install_window_events(&ui, &data, &settings_writer_handle);

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
    let recording_result = recording
        .lock()
        .map_err(|error| format!("Cannot finish recording on close: {error}"))
        .and_then(|mut recording| {
            if recording.is_recording() {
                recording
                    .stop()
                    .map(|_| ())
                    .map_err(|error| format!("Cannot save recording on close: {error}"))
            } else {
                Ok(())
            }
        });
    if let Err(error) = &recording_result
        && let Ok(log) = DiagnosticLog::initialize()
    {
        log.record(error);
    }
    pump_running.store(false, Ordering::Release);
    let _ = snapshot_listener.join();
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
    ui_result?;
    recording_result?;
    Ok(())
}

fn configure_window(ui: &K3Window, settings: &GuiSettings) -> Result<(), Box<dyn Error>> {
    slint::select_bundled_translation(settings.language.locale())?;
    ui.set_language_index(settings.language.index());
    ui.set_separation_profile(settings.separation_profile.index());
    ui.set_quality_model_index(settings.separation_quality_model.index());
    ui.set_separation_device_index(settings.separation_device.index());
    if let Ok(path) = GuiSettings::path() {
        ui.set_settings_path(path_text(&path));
    }
    if let Ok(log) = DiagnosticLog::initialize() {
        ui.set_log_path(path_text(log.path()));
    }
    ui.window().set_size(LogicalSize::new(
        logical_dimension(settings.window.width),
        logical_dimension(settings.window.height),
    ));
    ui.set_volume(settings.volume);
    ui.set_effect_index(effect_index(settings.recording.default_effect));
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

    Ok(())
}

fn install_window_events(
    ui: &K3Window,
    data: &Arc<Mutex<AppData>>,
    settings_writer: &SettingsWriterHandle,
) {
    let ui = ui.as_weak();
    let data = Arc::clone(data);
    let settings_writer = settings_writer.clone();
    let mut was_focused = true;
    ui.unwrap().window().on_winit_window_event(move |_, event| {
        if let winit::event::WindowEvent::KeyboardInput { event, .. } = event
            && event.state == winit::event::ElementState::Pressed
            && !event.repeat
            && event.logical_key == winit::keyboard::Key::Named(winit::keyboard::NamedKey::Escape)
            && ui.upgrade().is_some_and(|ui| ui.invoke_close_top_layer())
        {
            return EventResult::PreventDefault;
        }
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

fn install_preference_callbacks(
    ui: &K3Window,
    data: &Arc<Mutex<AppData>>,
    settings_writer: &SettingsWriterHandle,
) {
    ui.on_localize_message(move |source, index| {
        let language = GuiLanguage::from_index(index).unwrap_or_default();
        i18n::message(source.as_str(), language).into()
    });
    updates_gui::install(ui);
    {
        let weak = ui.as_weak();
        let data = Arc::clone(data);
        let settings_writer = settings_writer.clone();
        ui.on_select_language(move |index| {
            let Some(language) = GuiLanguage::from_index(index) else {
                return;
            };
            if slint::select_bundled_translation(language.locale()).is_ok()
                && let Ok(mut data) = data.lock()
            {
                data.settings.language = language;
                if let Some(ui) = weak.upgrade() {
                    ui.set_take_options(take_options(data.takes.len(), language));
                }
                if data.settings_writable {
                    settings_writer.persist(data.settings.clone());
                }
            }
        });
    }
    {
        let data = Arc::clone(data);
        let settings_writer = settings_writer.clone();
        ui.on_select_separation_profile(move |index| {
            let Some(profile) = GuiSeparationProfile::from_index(index) else {
                return;
            };
            if let Ok(mut data) = data.lock() {
                data.settings.separation_profile = profile;
                if data.settings_writable {
                    settings_writer.persist(data.settings.clone());
                }
            }
        });
    }
    {
        let data = Arc::clone(data);
        let settings_writer = settings_writer.clone();
        ui.on_select_separation_device(move |index| {
            let Some(device) = GuiSeparationDevice::from_index(index) else {
                return;
            };
            if let Ok(mut data) = data.lock() {
                data.settings.separation_device = device;
                if data.settings_writable {
                    settings_writer.persist(data.settings.clone());
                }
            }
        });
    }
    {
        let data = Arc::clone(data);
        let settings_writer = settings_writer.clone();
        ui.on_select_quality_model(move |index| {
            let Some(model) = GuiQualityModel::from_index(index) else {
                return;
            };
            if let Ok(mut data) = data.lock() {
                data.settings.separation_quality_model = model;
                if data.settings_writable {
                    settings_writer.persist(data.settings.clone());
                }
            }
        });
    }
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
    install_recording_callbacks(ui, data, playback, recording, settings_writer);
    install_take_callbacks(ui, data, playback, recording);
    install_lyrics_callbacks(ui, data, recording);
    install_separation_callbacks(ui, data, playback, settings_writer);
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
    playback: &Arc<PlaybackService>,
    settings_writer: &SettingsWriterHandle,
) {
    install_choose_separation_source(ui, data);
    install_queue_selected_sources(ui, data);
    install_start_separation(ui, data, playback, settings_writer);
}

fn select_separation_sources(
    sources: Vec<PathBuf>,
    root: &Path,
) -> (Vec<SelectedSeparation>, Vec<String>) {
    let mut projects = HashSet::new();
    let mut selected = Vec::new();
    let mut skipped = Vec::new();
    for source in sources {
        let name = source.file_name().map_or_else(
            || source.display().to_string(),
            |name| name.to_string_lossy().into(),
        );
        match separation::destination(&source, root) {
            Ok((project, _)) if !projects.insert(project.to_string_lossy().to_lowercase()) => {
                skipped.push(format!(
                    "{name}: another selected file has the same project name"
                ));
            }
            Ok((project, exists)) => selected.push(SelectedSeparation {
                source,
                project,
                allow_replace: exists,
            }),
            Err(error) => skipped.push(format!("{name}: {error}")),
        }
    }
    (selected, skipped)
}

fn install_choose_separation_source(ui: &K3Window, data: &Arc<Mutex<AppData>>) {
    let weak = ui.as_weak();
    let picker_data = Arc::clone(data);
    ui.on_choose_separation_source(move || {
        let weak = weak.clone();
        let picker_data = Arc::clone(&picker_data);
        thread::spawn(move || {
            let language = picker_data
                .lock()
                .map_or(GuiLanguage::English, |data| data.settings.language);
            let audio_label = i18n::message("Audio", language);
            let Some(sources) = rfd::FileDialog::new()
                .add_filter(
                    &audio_label,
                    &["flac", "mp3", "wav", "m4a", "ogg", "opus", "aac"],
                )
                .pick_files()
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
                .map(|root| select_separation_sources(sources, root));
            let _ = slint::invoke_from_event_loop(move || {
                let Some(ui) = weak.upgrade() else {
                    return;
                };
                if picker_data
                    .lock()
                    .ok()
                    .and_then(|data| data.settings.projects_root.clone())
                    != root
                {
                    return;
                }
                match result {
                    Ok((selected, skipped)) => {
                        let count = selected.len();
                        let existing = selected.iter().any(|item| item.allow_replace);
                        let display = if count == 1 {
                            selected[0].source.display().to_string()
                        } else {
                            let names = selected
                                .iter()
                                .filter_map(|item| item.source.file_name())
                                .map(|name| name.to_string_lossy())
                                .collect::<Vec<_>>()
                                .join(", ");
                            format!("{count} files: {names}")
                        };
                        if let Ok(mut data) = picker_data.lock() {
                            data.selected_separation_sources = selected;
                        }
                        ui.set_separation_source(display.into());
                        ui.set_separation_selected_count(i32::try_from(count).unwrap_or(i32::MAX));
                        ui.set_separation_existing(existing);
                        ui.set_separation_selection_message(skipped.join("\n").into());
                    }
                    Err(error) => {
                        ui.set_separation_selection_message(error.into());
                    }
                }
            });
        });
    });
}

fn install_queue_selected_sources(ui: &K3Window, data: &Arc<Mutex<AppData>>) {
    let weak = ui.as_weak();
    let data = Arc::clone(data);
    ui.on_queue_selected_sources(move |profile_index| {
        let Some(ui) = weak.upgrade() else { return };
        if ui.get_recording_state() != RecordingState::Idle {
            return;
        }
        let Ok(mut state) = data.lock() else { return };
        let Some(root) = state.settings.projects_root.clone() else {
            ui.set_separation_selection_message("Choose a projects folder first".into());
            return;
        };
        if !state.separation_running && state.queued_separations.is_empty() {
            state.separation_completed = 0;
            state.separation_errors.clear();
        }
        let mut added = 0;
        let mut skipped = Vec::new();
        for selected in std::mem::take(&mut state.selected_separation_sources) {
            let name = selected.source.file_name().map_or_else(
                || selected.source.display().to_string(),
                |name| name.to_string_lossy().into(),
            );
            let (project, exists) = match separation::destination(&selected.source, &root) {
                Ok(destination) => destination,
                Err(error) => {
                    skipped.push(format!("{name}: {error}"));
                    continue;
                }
            };
            if project != selected.project || (exists && !selected.allow_replace) {
                skipped.push(format!(
                    "{name}: destination changed; select the file again"
                ));
                continue;
            }
            if state.active_separation_project.as_ref() == Some(&project)
                || state
                    .queued_separations
                    .iter()
                    .any(|job| job.project == project)
            {
                skipped.push(format!("{name}: already queued or separating"));
                continue;
            }
            let quality_model_index = state.settings.separation_quality_model.index();
            state.queued_separations.push_back(QueuedSeparation {
                source: selected.source,
                project,
                profile_index,
                quality_model_index,
                allow_replace: selected.allow_replace,
            });
            added += 1;
        }
        drop(state);
        ui.set_separation_source(SharedString::default());
        ui.set_separation_selected_count(0);
        ui.set_separation_existing(false);
        let summary = format!("Queued {added} audio file(s)");
        ui.set_separation_selection_message(if skipped.is_empty() {
            summary.into()
        } else {
            format!("{summary}\n{}", skipped.join("\n")).into()
        });
        start_next_queued_separation(&ui, &data);
    });
}

fn separation_request(
    ui: &K3Window,
    source: &str,
    projects_root: PathBuf,
    profile_index: i32,
    model_index: i32,
    allow_replace: bool,
) -> SeparationRequest {
    let profile = Profile::from_index(profile_index);
    let quality_model = GuiQualityModel::from_index(model_index).unwrap_or_default();
    SeparationRequest {
        device: GuiSeparationDevice::from_index(ui.get_separation_device_index())
            .unwrap_or_default(),
        source: PathBuf::from(source),
        projects_root,
        profile,
        model_id: (profile == Profile::Quality)
            .then(|| quality_model.model_id())
            .flatten(),
        allow_replace,
    }
}

fn install_start_separation(
    ui: &K3Window,
    data: &Arc<Mutex<AppData>>,
    playback: &Arc<PlaybackService>,
    settings_writer: &SettingsWriterHandle,
) {
    let weak = ui.as_weak();
    let data = Arc::clone(data);
    let playback = Arc::clone(playback);
    let settings_writer = settings_writer.clone();
    ui.on_start_separation(move |source, profile_index, model_index, allow_replace| {
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
        let request = separation_request(
            &ui,
            source.as_str(),
            root.clone(),
            profile_index,
            model_index,
            allow_replace,
        );
        let (project, exists) = match separation::destination(&request.source, &root) {
            Ok(destination) => destination,
            Err(error) => {
                ui.set_separation_state(SeparationState::Error);
                ui.set_separation_message(error.into());
                return;
            }
        };
        if exists && !allow_replace {
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
        if state.take_operation_running && state.project_is_open(&project) {
            ui.set_separation_state(SeparationState::Error);
            ui.set_separation_message("Wait for take rendering to finish".into());
            return;
        }
        if ui.get_recording_state() != RecordingState::Idle && state.project_is_open(&project) {
            ui.set_separation_state(SeparationState::Error);
            ui.set_separation_message(
                "Stop the recording before replacing this project's stems".into(),
            );
            return;
        }
        let reload_id = exists.then(|| state.open_project_id(&project)).flatten();
        state.separation_running = true;
        state.active_separation_project = Some(project);
        drop(state);
        if let Some(id) = reload_id
            && let Err(message) = unload_for_separation(&playback, &data, id)
        {
            if let Ok(mut state) = data.lock() {
                state.separation_running = false;
                state.active_separation_project = None;
            }
            ui.set_separation_state(SeparationState::Error);
            ui.set_separation_message(message.into());
            return;
        }
        if reload_id.is_some() {
            ui.set_playback_state(PlaybackState::Loading);
        }
        show_separation_started(&ui, &request);
        publish_projects(&ui, &data, ui.get_search_query().as_str());
        run_separation_task(
            weak.clone(),
            Arc::clone(&data),
            settings_writer.clone(),
            request,
            script,
        );
    });
}

fn unload_for_separation(
    playback: &PlaybackService,
    data: &Mutex<AppData>,
    id: Uuid,
) -> Result<(), String> {
    let snapshot = playback
        .execute(PlaybackCommand::Unload)
        .map_err(|error| error.to_string())?;
    if snapshot.status == PlaybackStatus::Error {
        return Err(snapshot
            .error
            .unwrap_or_else(|| "Could not unload audio".into()));
    }
    if let Ok(mut state) = data.lock() {
        state.open_generation = state.open_generation.wrapping_add(1);
        state.pending_open_id = None;
        state.reopen_after_separation = Some(id);
        state.discard_snapshots_through_generation = Some(snapshot.project_generation);
    }
    Ok(())
}

fn show_separation_started(ui: &K3Window, request: &SeparationRequest) {
    ui.set_separation_state(SeparationState::Running);
    ui.set_separation_progress(-1.0);
    ui.set_separation_stage(separation::ProgressPhase::Preparing.label().into());
    ui.set_separation_elapsed_seconds(0);
    let name = request.source.file_name().map_or_else(
        || request.source.display().to_string(),
        |name| name.to_string_lossy().into(),
    );
    ui.set_separation_current_song(name.clone().into());
    ui.set_separation_message(format!("Separating {name}. This may take several minutes…").into());
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
        let progress_ui = weak.clone();
        let result = separation::run(&request, &script, move |progress| {
            let weak = progress_ui.clone();
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(ui) = weak.upgrade() {
                    ui.set_separation_stage(progress.phase.label().into());
                    ui.set_separation_progress(progress.fraction.unwrap_or(-1.0));
                    ui.set_separation_elapsed_seconds(
                        i32::try_from(progress.elapsed.as_secs()).unwrap_or(i32::MAX),
                    );
                }
            });
        });
        let _ = slint::invoke_from_event_loop(move || {
            let mut reopen_id = None;
            if let Ok(mut state) = data.lock() {
                state.separation_running = false;
                state.active_separation_project = None;
                reopen_id = state.reopen_after_separation.take();
                match &result {
                    Ok(_) => state.separation_completed += 1,
                    Err(error) => state
                        .separation_errors
                        .push(format!("{}: {error}", request.source.display())),
                }
            }
            let Some(ui) = weak.upgrade() else {
                return;
            };
            publish_projects(&ui, &data, ui.get_search_query().as_str());
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
            if let Some(id) = reopen_id {
                ui.invoke_open_project(id.to_string().into());
            }
            start_next_queued_separation(&ui, &data);
        });
    });
}

fn queue_netease_separation(
    ui: &K3Window,
    data: &Arc<Mutex<AppData>>,
    source: PathBuf,
    projects_root: &Path,
    profile_index: i32,
    quality_model_index: i32,
    allow_replace: bool,
) -> usize {
    // The download handler already validated the file and destination.
    let project = source
        .file_stem()
        .map_or_else(|| source.clone(), |name| projects_root.join(name));
    let Ok(mut state) = data.lock() else {
        ui.set_separation_message("Could not access the separation queue".into());
        return 0;
    };
    if !state.separation_running && state.queued_separations.is_empty() {
        state.separation_completed = 0;
        state.separation_errors.clear();
    }
    if state.active_separation_project.as_ref() != Some(&project)
        && !state
            .queued_separations
            .iter()
            .any(|job| job.project == project)
    {
        state.queued_separations.push_back(QueuedSeparation {
            source,
            project,
            profile_index,
            quality_model_index,
            allow_replace,
        });
    }
    drop(state);
    start_next_queued_separation(ui, data);
    update_separation_queue_count(ui, data)
}

fn start_next_queued_separation(ui: &K3Window, data: &Arc<Mutex<AppData>>) {
    loop {
        let next = {
            let Ok(mut state) = data.lock() else { return };
            if state.separation_running {
                let waiting = state.queued_separations.len();
                drop(state);
                ui.set_separation_queued_count(i32::try_from(waiting).unwrap_or(i32::MAX));
                return;
            }
            // Keep FIFO order, but wait only when the next job would replace
            // the project currently being recorded or rendered.
            if (ui.get_recording_state() != RecordingState::Idle || state.take_operation_running)
                && state
                    .queued_separations
                    .front()
                    .is_some_and(|job| state.project_is_open(&job.project))
            {
                let waiting = state.queued_separations.len();
                drop(state);
                ui.set_separation_queued_count(i32::try_from(waiting).unwrap_or(i32::MAX));
                return;
            }
            state.queued_separations.pop_front()
        };
        let Some(next) = next else {
            ui.set_separation_queued_count(0);
            if let Ok(state) = data.lock()
                && (state.separation_completed > 0 || !state.separation_errors.is_empty())
            {
                let summary = format!(
                    "Separated {} song(s); {} failed{}",
                    state.separation_completed,
                    state.separation_errors.len(),
                    state
                        .separation_errors
                        .last()
                        .map_or_else(String::new, |error| format!(". Last error: {error}"))
                );
                ui.set_separation_state(if state.separation_errors.is_empty() {
                    SeparationState::Success
                } else {
                    SeparationState::Error
                });
                ui.set_separation_message(summary.into());
            }
            return;
        };
        update_separation_queue_count(ui, data);
        ui.invoke_start_separation(
            path_text(&next.source),
            next.profile_index,
            next.quality_model_index,
            next.allow_replace,
        );
        if data.lock().is_ok_and(|state| state.separation_running) {
            return;
        }
        if let Ok(mut state) = data.lock() {
            state.separation_errors.push(format!(
                "{}: {}",
                next.source.display(),
                ui.get_separation_message()
            ));
        }
    }
}

fn update_separation_queue_count(ui: &K3Window, data: &Arc<Mutex<AppData>>) -> usize {
    let count = data
        .lock()
        .map_or(0, |state| state.queued_separations.len());
    ui.set_separation_queued_count(i32::try_from(count).unwrap_or(i32::MAX));
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
                data.separation_running
                    || data.take_operation_running
                    || !data.queued_separations.is_empty()
            }) {
                if let Some(ui) = ui.upgrade() {
                    show_error(
                        &ui,
                        "Wait for audio processing to finish before changing folders",
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
            let Ok(id) = Uuid::parse_str(id.as_str()) else {
                return;
            };
            request_project_open(&ui, &data, &playback, &recording, id, None, false);
        });
    }
}

fn request_project_open(
    ui: &slint::Weak<K3Window>,
    data: &Arc<Mutex<AppData>>,
    playback: &Arc<PlaybackService>,
    recording: &Arc<Mutex<GuiRecordingController>>,
    id: Uuid,
    take_id: Option<String>,
    play_take: bool,
) {
    if data
        .lock()
        .is_ok_and(|data| data.project_is_separating(id) || data.take_operation_running)
    {
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
    let project = data.lock().ok().and_then(|mut data| {
        let project = data
            .projects
            .iter()
            .find(|project| project.id == id)
            .cloned()?;
        let preferred_take = take_id.or_else(|| {
            (data.selected_id == Some(id))
                .then(|| data.selected_take_id.clone())
                .flatten()
        });
        data.open_generation = data.open_generation.wrapping_add(1);
        data.pending_open_id = Some(id);
        data.take_operation_running = play_take;
        // An explicit project choice supersedes the automatic reopen of the
        // project that was unloaded for separation.
        data.reopen_after_separation = None;
        invalidate_lyrics_context(&mut data);
        Some((project, data.open_generation, preferred_take))
    });
    let Some((project, generation, preferred_take)) = project else {
        return;
    };
    if let Some(ui) = ui.upgrade() {
        if play_take {
            ui.set_take_busy(true);
        }
        begin_project_load(&ui);
    }
    open_project(
        ui.clone(),
        Arc::clone(data),
        Arc::clone(playback),
        generation,
        project,
        preferred_take,
        play_take,
    );
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
    settings_writer: &SettingsWriterHandle,
) {
    {
        let weak = ui.as_weak();
        let data = Arc::clone(data);
        let playback = Arc::clone(playback);
        let recording = Arc::clone(recording);
        let settings_writer = settings_writer.clone();
        ui.on_select_effect(move |index| {
            let Some(window) = weak.upgrade() else { return };
            if window.get_recording_state() != RecordingState::Idle
                || window.get_take_busy()
                || window.get_loading()
                || window.get_current_project_separating()
            {
                return;
            }
            let Some(effect) = effect_for_index(index) else {
                return;
            };
            window.set_take_effect_message(SharedString::default());
            let selected = data.lock().ok().and_then(|mut state| {
                state.settings.recording.default_effect = effect;
                if state.settings_writable {
                    settings_writer.persist(state.settings.clone());
                }
                let project_id = state.selected_id?;
                let take_id = state.selected_take_id.as_ref()?;
                let take = state.takes.iter().find(|take| take.id == *take_id)?;
                Some((project_id, take.id.clone()))
            });
            if let Some((project_id, take_id)) = selected {
                request_project_open(
                    &weak,
                    &data,
                    &playback,
                    &recording,
                    project_id,
                    Some(take_id),
                    true,
                );
            }
        });
    }

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

fn install_take_callbacks(
    ui: &K3Window,
    data: &Arc<Mutex<AppData>>,
    playback: &Arc<PlaybackService>,
    recording: &Arc<Mutex<GuiRecordingController>>,
) {
    install_select_take_callback(ui, data, playback, recording);
    install_delete_take_callbacks(ui, data, playback, recording);
}

fn install_delete_take_callbacks(
    ui: &K3Window,
    data: &Arc<Mutex<AppData>>,
    playback: &Arc<PlaybackService>,
    recording: &Arc<Mutex<GuiRecordingController>>,
) {
    let pending = Arc::new(Mutex::new(None::<PendingTakeDeletion>));
    {
        let weak = ui.as_weak();
        let data = Arc::clone(data);
        let pending = Arc::clone(&pending);
        ui.on_begin_delete_take(move || {
            let Some(window) = weak.upgrade() else { return };
            if window.get_recording_state() != RecordingState::Idle
                || window.get_loading()
                || window.get_take_busy()
                || window.get_current_project_separating()
            {
                return;
            }
            let selection = data.lock().ok().and_then(|state| {
                let project_id = state.selected_id?;
                let take_id = state.selected_take_id.clone()?;
                state.takes.iter().find(|take| take.id == take_id)?;
                let project_root = state
                    .projects
                    .iter()
                    .find(|p| p.id == project_id)?
                    .path
                    .clone();
                Some(PendingTakeDeletion {
                    project_id,
                    project_root,
                    take_id,
                    revision: state.selected_document_revision.clone(),
                })
            });
            if let Some(selection) = selection {
                window.set_delete_take_name(selection.take_id.clone().into());
                if let Ok(mut pending) = pending.lock() {
                    *pending = Some(selection);
                    window.set_overlay(Overlay::ConfirmTakeDelete);
                }
            }
        });
    }
    {
        let pending = Arc::clone(&pending);
        ui.on_cancel_delete_take(move || {
            if let Ok(mut pending) = pending.lock() {
                *pending = None;
            }
        });
    }
    let weak = ui.as_weak();
    let data = Arc::clone(data);
    let playback = Arc::clone(playback);
    let recording = Arc::clone(recording);
    ui.on_confirm_delete_take(move || {
        let Some(window) = weak.upgrade() else { return };
        let Some(selection) = pending.lock().ok().and_then(|mut pending| pending.take()) else {
            return;
        };
        window.set_overlay(Overlay::None);
        let valid = data.lock().is_ok_and(|mut state| {
            if state.take_operation_running
                || state.project_is_separating(selection.project_id)
                || state.selected_id != Some(selection.project_id)
                || state.selected_take_id.as_deref() != Some(selection.take_id.as_str())
                || window.get_recording_state() != RecordingState::Idle
            {
                return false;
            }
            state.take_operation_running = true;
            state.open_generation = state.open_generation.wrapping_add(1);
            state.pending_open_id = None;
            true
        });
        if !valid {
            return;
        }
        window.set_take_busy(true);
        window.set_recording_message("Deleting selected take…".into());
        begin_project_load(&window);
        let data = Arc::clone(&data);
        let playback = Arc::clone(&playback);
        let recording = Arc::clone(&recording);
        let weak = weak.clone();
        delete_selected_take(weak, data, playback, recording, selection);
    });
}

fn delete_selected_take(
    weak: slint::Weak<K3Window>,
    data: Arc<Mutex<AppData>>,
    playback: Arc<PlaybackService>,
    recording: Arc<Mutex<GuiRecordingController>>,
    selection: PendingTakeDeletion,
) {
    thread::spawn(move || {
        let result = (|| -> Result<_, String> {
            let unloaded = playback
                .execute(PlaybackCommand::Unload)
                .map_err(|e| e.to_string())?;
            if unloaded.status == PlaybackStatus::Error {
                return Err(unloaded
                    .error
                    .unwrap_or_else(|| "Could not unload audio".into()));
            }
            if let Ok(mut state) = data.lock() {
                state.discard_snapshots_through_generation = Some(unloaded.project_generation);
            }
            FileProjectRepository
                .delete_take(
                    &selection.project_root,
                    &selection.take_id,
                    selection.revision.as_ref(),
                )
                .map_err(|e| e.to_string())
        })();
        let _ = slint::invoke_from_event_loop(move || {
            let Some(window) = weak.upgrade() else { return };
            if let Ok(mut state) = data.lock() {
                state.take_operation_running = false;
                if let Ok(deleted) = &result {
                    let index = state
                        .takes
                        .iter()
                        .position(|take| take.id == selection.take_id)
                        .unwrap_or(0);
                    state.selected_take_id = deleted
                        .project
                        .takes()
                        .get(index.min(deleted.project.takes().len().saturating_sub(1)))
                        .map(|take| take.id().to_owned());
                }
            }
            window.set_take_busy(false);
            window.set_take_effect_message(SharedString::default());
            request_project_open(
                &weak,
                &data,
                &playback,
                &recording,
                selection.project_id,
                None,
                false,
            );
            match result {
                Ok(deleted) => {
                    let warning = deleted
                        .cleanup_warning
                        .map_or_else(String::new, |warning| format!(" · Warning: {warning}"));
                    window.set_recording_message(
                        format!("Deleted take {}{warning}", selection.take_id).into(),
                    );
                    clear_general_error(&window);
                }
                Err(error) => {
                    window.set_recording_message(SharedString::default());
                    show_error(&window, format!("Cannot delete take: {error}"));
                }
            }
            start_next_queued_separation(&window, &data);
        });
    });
}

fn install_select_take_callback(
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
        ui.unwrap().on_select_existing_take(move |index| {
            if ui.upgrade().is_none_or(|window| {
                window.get_recording_state() != RecordingState::Idle
                    || window.get_loading()
                    || window.get_current_project_separating()
            }) {
                return;
            }
            let selection = usize::try_from(index).ok().and_then(|index| {
                data.lock().ok().and_then(|state| {
                    let project_id = state.selected_id?;
                    let take = state.takes.get(index)?;
                    (!state.take_operation_running).then(|| (project_id, take.id.clone()))
                })
            });
            let Some((project_id, take_id)) = selection else {
                return;
            };
            if let Some(window) = ui.upgrade() {
                window.set_take_effect_message(SharedString::default());
            }
            request_project_open(
                &ui,
                &data,
                &playback,
                &recording,
                project_id,
                Some(take_id),
                true,
            );
        });
    }
}

const fn effect_for_index(index: i32) -> Option<VocalEffectPreset> {
    match index {
        0 => Some(VocalEffectPreset::Clean),
        1 => Some(VocalEffectPreset::Studio),
        2 => Some(VocalEffectPreset::Ktv),
        3 => Some(VocalEffectPreset::Theater),
        4 => Some(VocalEffectPreset::Church),
        _ => None,
    }
}

const fn effect_index(effect: VocalEffectPreset) -> i32 {
    match effect {
        VocalEffectPreset::Clean => 0,
        VocalEffectPreset::Studio => 1,
        VocalEffectPreset::Ktv => 2,
        VocalEffectPreset::Theater => 3,
        VocalEffectPreset::Church => 4,
    }
}

fn start_gui_recording(
    ui: slint::Weak<K3Window>,
    data: &Arc<Mutex<AppData>>,
    playback: &PlaybackService,
    recording: Arc<Mutex<GuiRecordingController>>,
) {
    if ui.upgrade().is_none_or(|window| {
        window.get_recording_state() != RecordingState::Idle || window.get_loading()
    }) {
        return;
    }
    let selection = data.lock().ok().and_then(|data| {
        if data.take_operation_running {
            return None;
        }
        let selected = data.selected_id?;
        if data.project_is_separating(selected) {
            return None;
        }
        data.projects
            .iter()
            .find(|project| project.id == selected)
            .map(|project| (project.path.clone(), data.settings.recording.default_effect))
    });
    let Some((project_root, effect_preset)) = selection else {
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
                    .start(&project_root, monitoring, volume, effect_preset)
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
                    if let Ok(mut state) = data.lock() {
                        state.selected_take_id = None;
                    }
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
            if ui.upgrade().is_some_and(|window| window.get_take_busy()) {
                return;
            }
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
            if track == TrackSelection::Take
                && ui.upgrade().is_some_and(|window| window.get_take_busy())
            {
                return;
            }
            if track == TrackSelection::Take && !recording_controls_active(&ui) {
                if let Some(window) = ui.upgrade() {
                    window.invoke_select_existing_take(window.get_selected_take_index());
                }
                return;
            }
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
                                data.takes.clear();
                                data.selected_take_id = None;
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
                        reset_root_selection(&ui, &data, &root);
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

fn reset_root_selection(ui: &K3Window, data: &Mutex<AppData>, root: &Path) {
    if let Ok(mut state) = data.lock() {
        state.selected_separation_sources.clear();
    }
    ui.set_projects_root(path_text(root));
    ui.set_take_options(ModelRc::new(VecModel::default()));
    ui.set_selected_take_index(0);
    ui.set_take_effect_message(SharedString::default());
    ui.set_separation_source(SharedString::default());
    ui.set_separation_selected_count(0);
    ui.set_separation_selection_message(SharedString::default());
    ui.set_separation_existing(false);
    ui.set_separation_state(SeparationState::Idle);
    ui.set_separation_message(SharedString::default());
    ui.set_separation_panel_open(false);
}

fn open_project(
    ui: slint::Weak<K3Window>,
    data: Arc<Mutex<AppData>>,
    playback: Arc<PlaybackService>,
    generation: u64,
    project: ProjectSummary,
    preferred_take: Option<String>,
    play_take: bool,
) {
    thread::spawn(move || {
        let loaded =
            load_project_for_playback(&data, &playback, &project.path, preferred_take, play_take);
        let _ = slint::invoke_from_event_loop(move || {
            let Some(ui) = ui.upgrade() else {
                return;
            };
            if !is_current_open(&data, generation) {
                return;
            }
            if play_take {
                if let Ok(mut state) = data.lock() {
                    state.take_operation_running = false;
                }
                ui.set_take_busy(false);
            }
            let (loaded, takes, selected) = match loaded {
                Ok(loaded) => loaded,
                Err(error) => {
                    clear_pending_open(&data, generation);
                    ui.set_loading(false);
                    show_error(&ui, error);
                    start_next_queued_separation(&ui, &data);
                    return;
                }
            };
            let selected_index = selected
                .as_ref()
                .and_then(|id| takes.iter().position(|take| take.id == *id))
                .unwrap_or(0);
            let take_options = take_options(
                takes.len(),
                GuiLanguage::from_index(ui.get_language_index()).unwrap_or_default(),
            );
            if let Ok(mut state) = data.lock() {
                if state.selected_id != Some(project.id) {
                    ui.set_take_effect_message(SharedString::default());
                }
                state.takes = takes;
                state.selected_take_id.clone_from(&selected);
            }
            ui.set_take_options(take_options);
            ui.set_selected_take_index(i32::try_from(selected_index).unwrap_or(i32::MAX));
            let command = if play_take && selected.is_some() {
                PlaybackCommand::LoadTake(loaded)
            } else {
                PlaybackCommand::Load(loaded)
            };
            match playback.dispatch(command) {
                Ok(()) => {}
                Err(error) => {
                    clear_pending_open(&data, generation);
                    ui.set_loading(false);
                    show_playback_error(&ui, error.to_string());
                }
            }
            if play_take {
                start_next_queued_separation(&ui, &data);
            }
        });
    });
}

fn load_project_for_playback(
    data: &Mutex<AppData>,
    playback: &PlaybackService,
    root: &Path,
    preferred_take: Option<String>,
    play_take: bool,
) -> Result<(LoadedProject, Vec<TakeChoice>, Option<String>), String> {
    // 同步经过播放队列，保证此前的调号命令已经保存，再读取实际工程。
    if play_take {
        let switched = playback
            .execute(PlaybackCommand::SwitchTrack(TrackKind::Accompaniment))
            .map_err(|error| error.to_string())?;
        if switched.status == PlaybackStatus::Error {
            return Err(switched
                .error
                .unwrap_or_else(|| "cannot release take audio".into()));
        }
        if switched.status == PlaybackStatus::Playing {
            playback
                .execute(PlaybackCommand::Toggle)
                .map_err(|error| error.to_string())?;
        }
    }
    let mut project = FileProjectRepository
        .open(root)
        .map_err(|error| error.to_string())?;
    let takes = project
        .takes()
        .iter()
        .map(|take| TakeChoice {
            id: take.id().to_owned(),
        })
        .collect::<Vec<_>>();
    let selected = preferred_take
        .filter(|id| takes.iter().any(|take| take.id == *id))
        .or_else(|| takes.last().map(|take| take.id.clone()));
    if play_take && let Some(take_id) = selected.as_deref() {
        let effect = data
            .lock()
            .map_err(|error| error.to_string())?
            .settings
            .recording
            .default_effect;
        let rendered =
            ensure_take_render(&mut project, take_id, effect).map_err(|error| error.to_string())?;
        if let Some(warning) = rendered.and_then(|rendered| rendered.cleanup_warning)
            && let Ok(log) = DiagnosticLog::initialize()
        {
            log.record(warning);
        }
    }
    let loaded = match selected.as_deref() {
        Some(take_id) => LoadedProject::from_project_with_lyrics_for_take(&project, take_id)
            .map_err(|error| error.to_string())?,
        None => {
            LoadedProject::from_project_with_lyrics(&project).map_err(|error| error.to_string())?
        }
    };
    Ok((loaded, takes, selected))
}

fn take_options(total: usize, language: GuiLanguage) -> ModelRc<SharedString> {
    let labels = (1..=total)
        .map(|index| {
            let english = if index == total {
                format!("Take {index} of {total} · latest")
            } else {
                format!("Take {index} of {total}")
            };
            i18n::message(&english, language).into()
        })
        .collect::<Vec<SharedString>>();
    ModelRc::new(VecModel::from(labels))
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
    ui.set_current_project_separating(
        data.selected_id
            .is_some_and(|id| data.project_is_separating(id)),
    );
    let items = ProjectLibrary::filter(&data.projects, query)
        .into_iter()
        .map(|project| ProjectItem {
            id: project.id.to_string().into(),
            title: project.title.as_str().into(),
            path: path_text(&project.path),
            status: if data.active_separation_project.as_ref() == Some(&project.path) {
                ProjectState::Separating
            } else if project.source_available {
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
        if data
            .discard_snapshots_through_generation
            .is_some_and(|generation| snapshot.project_generation <= generation)
        {
            return;
        }
        if !snapshot_matches_pending_open(data.pending_open_id, project_id) {
            return;
        }
        data.discard_snapshots_through_generation = None;
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
    ui.set_follow_lyrics(false);
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

#[cfg(test)]
mod selection_tests {
    use std::fs;

    use super::select_separation_sources;

    #[test]
    fn batch_allows_existing_projects_and_skips_colliding_names() {
        let sandbox = tempfile::tempdir().unwrap();
        let projects = sandbox.path().join("projects");
        fs::create_dir(&projects).unwrap();
        let first = sandbox.path().join("song.wav");
        let duplicate = sandbox.path().join("song.flac");
        let existing = sandbox.path().join("old.wav");
        for source in [&first, &duplicate, &existing] {
            fs::write(source, b"audio").unwrap();
        }
        fs::create_dir(projects.join("old")).unwrap();
        fs::write(projects.join("old/project.json"), b"{}").unwrap();

        let (selected, skipped) =
            select_separation_sources(vec![first.clone(), duplicate, existing.clone()], &projects);
        assert_eq!(selected.len(), 2);
        assert_eq!(selected[0].source, first);
        assert!(!selected[0].allow_replace);
        assert_eq!(selected[1].source, existing);
        assert!(selected[1].allow_replace);
        assert_eq!(skipped.len(), 1);
        assert!(
            skipped
                .iter()
                .any(|issue| issue.contains("same project name"))
        );
    }
}
