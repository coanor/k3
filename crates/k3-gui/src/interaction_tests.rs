use std::{
    cell::Cell,
    cell::RefCell,
    collections::VecDeque,
    path::PathBuf,
    rc::Rc,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType, Rgb565Pixel};
use slint::platform::{
    Key, Platform, PlatformError, PointerEventButton, WindowAdapter, WindowEvent,
};
use slint::{
    ComponentHandle, LogicalPosition, Model, ModelRc, PhysicalSize, SharedString, VecModel,
};

use super::{
    AppData, ErrorKind, K3Window, LyricItem, LyricsPanelState, Overlay, PlaybackAction,
    PlaybackState, PlaybackTransitionTracker, ProjectItem, ProjectState, RecordingState,
    TrackSelection, ViewMode, apply_lyrics_search_result, begin_lyrics_context, begin_project_load,
    queue_netease_separation, should_apply_project_snapshot, snapshot_matches_pending_open,
    start_next_queued_separation,
};
use crate::settings::GuiSettings;
use k3_app::{
    LyricsChoice, LyricsSearch, PlaybackSnapshot, PlaybackStatus, ProjectSummary, TrackKind,
};
use uuid::Uuid;

thread_local! {
    static WINDOW: Rc<MinimalSoftwareWindow> =
        MinimalSoftwareWindow::new(RepaintBufferType::ReusedBuffer);
    static CLIPBOARD: RefCell<Option<String>> = const { RefCell::new(None) };
}

#[test]
fn netease_error_message_can_be_copied_in_full() {
    let window = setup_window();
    let ui = K3Window::new().expect("test UI should construct");
    let message = "NetEase download failed: a detailed error to report";
    ui.set_view_mode(ViewMode::Rehearsal);
    ui.set_separation_panel_open(true);
    ui.set_netease_message(message.into());
    ui.show().expect("test UI should show");
    window.draw_if_needed(|_| {});

    CLIPBOARD.with(|clipboard| *clipboard.borrow_mut() = None);
    ui.invoke_copy_netease_message();

    CLIPBOARD.with(|clipboard| assert_eq!(clipboard.borrow().as_deref(), Some(message)));
}

#[test]
fn netease_selection_can_be_queued_during_a_download_and_separation() {
    let _window = setup_window();
    let ui = K3Window::new().expect("test UI should construct");
    ui.set_netease_logged_in(true);
    ui.set_netease_selected_count(2);
    ui.set_netease_download_running(true);
    ui.set_separation_state(super::SeparationState::Running);
    assert!(ui.get_netease_can_queue());

    let data = Arc::new(Mutex::new(app_data_with_projects(Vec::new(), Uuid::nil())));
    data.lock().unwrap().separation_running = true;
    assert_eq!(
        queue_netease_separation(
            &ui,
            &data,
            PathBuf::from("one.wav"),
            &PathBuf::from("."),
            1,
            0,
            false
        ),
        1
    );
    assert_eq!(
        queue_netease_separation(
            &ui,
            &data,
            PathBuf::from("two.wav"),
            &PathBuf::from("."),
            2,
            0,
            false
        ),
        2
    );

    let started = Rc::new(RefCell::new(Vec::new()));
    let observed = Rc::clone(&started);
    let callback_data = Arc::clone(&data);
    ui.on_start_separation(move |source, profile, _, _| {
        callback_data.lock().unwrap().separation_running = true;
        observed.borrow_mut().push((source.to_string(), profile));
    });
    data.lock().unwrap().separation_running = false;
    start_next_queued_separation(&ui, &data);
    assert_eq!(started.borrow().as_slice(), [("one.wav".into(), 1)]);
    data.lock().unwrap().separation_running = false;
    start_next_queued_separation(&ui, &data);
    assert_eq!(
        started.borrow().as_slice(),
        [("one.wav".into(), 1), ("two.wav".into(), 2)]
    );
}

#[test]
fn recording_shortcuts_keep_all_rehearsal_controls_available() {
    let window = setup_window();
    let ui = K3Window::new().expect("test UI should construct");
    ui.set_view_mode(ViewMode::Rehearsal);
    ui.set_playback_state(PlaybackState::Paused);
    ui.set_position_seconds(5.0);
    ui.set_original_available(true);
    ui.set_accompaniment_available(true);
    ui.set_vocals_available(true);
    ui.set_take_available(true);

    let recording = Rc::new(RefCell::new(Vec::<bool>::new()));
    let observed_recording = Rc::clone(&recording);
    ui.on_recording_command(move |start| observed_recording.borrow_mut().push(start));
    let monitoring = Rc::new(RefCell::new(Vec::<bool>::new()));
    let observed_monitoring = Rc::clone(&monitoring);
    ui.on_set_monitoring(move |enabled| observed_monitoring.borrow_mut().push(enabled));
    let playback = Rc::new(RefCell::new(Vec::<PlaybackAction>::new()));
    let observed_playback = Rc::clone(&playback);
    ui.on_playback_command(move |command| observed_playback.borrow_mut().push(command));
    let volumes = Rc::new(RefCell::new(Vec::<f32>::new()));
    let observed_volumes = Rc::clone(&volumes);
    ui.on_set_volume(move |volume| observed_volumes.borrow_mut().push(volume));
    let tracks = Rc::new(RefCell::new(Vec::<TrackSelection>::new()));
    let observed_tracks = Rc::clone(&tracks);
    ui.on_switch_track(move |track| observed_tracks.borrow_mut().push(track));
    let keys = Rc::new(RefCell::new(Vec::<i32>::new()));
    let observed_keys = Rc::clone(&keys);
    ui.on_set_key(move |key| observed_keys.borrow_mut().push(key));

    ui.show().expect("test UI should show");
    window.draw_if_needed(|_| {});
    press_key(&ui, "m");
    press_key(&ui, "r");
    ui.set_recording_state(RecordingState::Recording);
    press_key(&ui, " ");
    press_key(&ui, Key::LeftArrow);
    press_key(&ui, Key::RightArrow);
    press_key(&ui, Key::UpArrow);
    press_key(&ui, Key::DownArrow);
    press_key(&ui, "1");
    press_key(&ui, "2");
    press_key(&ui, "3");
    press_key(&ui, "4");
    press_key(&ui, ",");
    ui.set_key_shift(-1);
    press_key(&ui, ".");
    ui.set_key_shift(1);
    press_key(&ui, "/");
    press_key(&ui, "m");
    press_key(&ui, "r");

    assert_eq!(recording.borrow().as_slice(), [true, false]);
    assert_eq!(monitoring.borrow().as_slice(), [true, false]);
    assert!(!ui.get_monitoring());
    assert_eq!(
        playback.borrow().as_slice(),
        [
            PlaybackAction::Toggle,
            PlaybackAction::Back,
            PlaybackAction::Forward
        ]
    );
    assert_eq!(volumes.borrow().as_slice(), [1.0, 0.95]);
    assert_eq!(
        tracks.borrow().as_slice(),
        [
            TrackSelection::Original,
            TrackSelection::Accompaniment,
            TrackSelection::Vocals,
            TrackSelection::Take
        ]
    );
    assert_eq!(keys.borrow().as_slice(), [-1, 0, 0]);
}

#[test]
fn lyrics_shortcut_opens_search_for_a_loaded_project() {
    let window = setup_window();
    let ui = K3Window::new().expect("test UI should construct");
    ui.set_view_mode(ViewMode::Rehearsal);
    ui.set_playback_state(PlaybackState::Paused);
    let opens = Rc::new(Cell::new(0));
    let observed_opens = Rc::clone(&opens);
    ui.on_begin_lyrics_search(move || observed_opens.set(observed_opens.get() + 1));

    ui.show().expect("test UI should show");
    window.draw_if_needed(|_| {});
    press_key(&ui, "l");

    assert_eq!(opens.get(), 1);
}

struct TestPlatform;

impl Platform for TestPlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, PlatformError> {
        Ok(WINDOW.with(Clone::clone))
    }

    fn set_clipboard_text(&self, text: &str, clipboard: slint::platform::Clipboard) {
        if clipboard == slint::platform::Clipboard::DefaultClipboard {
            CLIPBOARD.with(|contents| *contents.borrow_mut() = Some(text.to_owned()));
        }
    }
}

pub(super) fn setup_window() -> Rc<MinimalSoftwareWindow> {
    let _ = slint::platform::set_platform(Box::new(TestPlatform));
    let window = WINDOW.with(Clone::clone);
    window.set_size(PhysicalSize::new(1280, 800));
    window
}

fn app_data_with_projects(projects: Vec<ProjectSummary>, selected_id: Uuid) -> AppData {
    AppData {
        settings: GuiSettings::default(),
        settings_writable: false,
        projects,
        selected_id: Some(selected_id),
        pending_open_id: None,
        selected_document_revision: None,
        takes: Vec::new(),
        selected_take_id: None,
        take_effect_running: false,
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
    }
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

fn render_pixels(
    window: &MinimalSoftwareWindow,
    previous: Option<&[Rgb565Pixel]>,
) -> Vec<Rgb565Pixel> {
    const WIDTH: usize = 1280;
    const HEIGHT: usize = 800;
    let mut pixels = previous.map_or_else(
        || vec![Rgb565Pixel::default(); WIDTH * HEIGHT],
        <[Rgb565Pixel]>::to_vec,
    );
    assert!(window.draw_if_needed(|renderer| {
        renderer.render(&mut pixels, WIDTH);
    }));
    pixels
}

fn vertical_color_span(
    pixels: &[Rgb565Pixel],
    color: Rgb565Pixel,
    x_range: std::ops::Range<usize>,
    y_range: std::ops::Range<usize>,
) -> std::ops::RangeInclusive<usize> {
    let mut rows = y_range.filter(|y| x_range.clone().any(|x| pixels[y * 1280 + x] == color));
    let first = rows
        .next()
        .expect("expected control color was not rendered");
    let last = rows.next_back().unwrap_or(first);
    first..=last
}

fn release_key(ui: &K3Window, text: impl Into<SharedString>) {
    ui.window()
        .dispatch_event(WindowEvent::KeyReleased { text: text.into() });
}

#[test]
fn custom_buttons_activate_on_first_click() {
    let window = setup_window();
    let ui = K3Window::new().expect("test UI should construct");
    ui.set_view_mode(ViewMode::Rehearsal);
    ui.set_playback_state(PlaybackState::Paused);
    ui.set_accompaniment_available(true);
    ui.set_projects(ModelRc::new(VecModel::from(vec![ProjectItem {
        id: SharedString::from("project-1"),
        title: SharedString::from("Project one"),
        path: SharedString::from("/tmp/project-1"),
        status: ProjectState::Ready,
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
        assert_eq!(command, PlaybackAction::Toggle);
        observed_playback_clicks.set(observed_playback_clicks.get() + 1);
    });

    let track_clicks = Rc::new(Cell::new(0));
    let observed_track_clicks = Rc::clone(&track_clicks);
    ui.on_switch_track(move |track| {
        assert_eq!(track, TrackSelection::Accompaniment);
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
    click(&ui, LogicalPosition::new(630.0, 715.0));
    click(&ui, LogicalPosition::new(162.0, 715.0));
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
fn transport_record_button_enables_monitoring_by_default() {
    let window = setup_window();
    let ui = K3Window::new().expect("test UI should construct");
    ui.set_view_mode(ViewMode::Rehearsal);
    ui.set_playback_state(PlaybackState::Paused);
    ui.set_accompaniment_available(true);

    let recording = Rc::new(RefCell::new(Vec::<bool>::new()));
    let observed_recording = Rc::clone(&recording);
    ui.on_recording_command(move |start| observed_recording.borrow_mut().push(start));
    let monitoring = Rc::new(RefCell::new(Vec::<bool>::new()));
    let observed_monitoring = Rc::clone(&monitoring);
    ui.on_set_monitoring(move |enabled| observed_monitoring.borrow_mut().push(enabled));

    ui.show().expect("test UI should show");
    window.draw_if_needed(|_| {});
    click(&ui, LogicalPosition::new(557.0, 715.0));
    assert!(ui.get_monitoring());
    ui.set_recording_state(RecordingState::Recording);
    click(&ui, LogicalPosition::new(671.0, 715.0));
    click(&ui, LogicalPosition::new(557.0, 715.0));

    assert_eq!(recording.borrow().as_slice(), [true, false]);
    assert_eq!(monitoring.borrow().as_slice(), [true, false]);
    assert!(!ui.get_monitoring());
}

#[test]
fn transport_round_buttons_share_one_size_and_center_line() {
    const CUE: Rgb565Pixel = Rgb565Pixel(0xfbad);
    const PAPER: Rgb565Pixel = Rgb565Pixel(0xf77c);

    let window = setup_window();
    let ui = K3Window::new().expect("test UI should construct");
    ui.set_view_mode(ViewMode::Rehearsal);
    ui.set_playback_state(PlaybackState::Playing);
    ui.set_accompaniment_available(true);
    ui.set_recording_state(RecordingState::Recording);
    ui.set_monitoring(true);
    ui.show().expect("test UI should show");
    let pixels = render_pixels(&window, None);

    let record = vertical_color_span(&pixels, CUE, 530..590, 680..800);
    let play = vertical_color_span(&pixels, PAPER, 590..650, 680..800);
    let monitor = vertical_color_span(&pixels, CUE, 650..710, 680..800);

    assert_eq!(record, play, "Record and Play circles must align");
    assert_eq!(monitor, play, "Monitor and Play circles must align");
}

#[test]
fn global_shortcuts_emit_playback_volume_track_and_refresh_intents() {
    let window = setup_window();
    let ui = K3Window::new().expect("test UI should construct");
    ui.set_view_mode(ViewMode::Rehearsal);
    ui.set_playback_state(PlaybackState::Paused);
    ui.set_position_seconds(5.0);
    ui.set_original_available(true);
    ui.set_accompaniment_available(true);
    ui.set_vocals_available(true);
    ui.set_take_available(true);

    let playback = Rc::new(RefCell::new(Vec::<PlaybackAction>::new()));
    let observed_playback = Rc::clone(&playback);
    ui.on_playback_command(move |command| {
        observed_playback.borrow_mut().push(command);
    });
    let tracks = Rc::new(RefCell::new(Vec::<TrackSelection>::new()));
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
    press_key(&ui, "4");
    press_key(&ui, Key::Control);
    press_key(&ui, "r");
    release_key(&ui, "r");
    release_key(&ui, Key::Control);

    assert_eq!(
        playback.borrow().as_slice(),
        [
            PlaybackAction::Toggle,
            PlaybackAction::Back,
            PlaybackAction::Forward
        ]
    );
    assert_eq!(
        tracks.borrow().as_slice(),
        [TrackSelection::Accompaniment, TrackSelection::Take]
    );
    assert_eq!(volumes.borrow().as_slice(), [1.0]);
    assert_eq!(refreshes.get(), 1);
}

#[test]
fn search_focus_keeps_typing_from_triggering_playback_shortcuts() {
    let window = setup_window();
    let ui = K3Window::new().expect("test UI should construct");
    ui.set_view_mode(ViewMode::Rehearsal);
    ui.set_playback_state(PlaybackState::Paused);
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
    ui.set_view_mode(ViewMode::Rehearsal);
    ui.set_playback_state(PlaybackState::Paused);
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
    ui.set_view_mode(ViewMode::Setup);
    ui.set_projects_root(SharedString::from("/tmp/projects"));
    ui.set_overlay(Overlay::About);

    ui.show().expect("test UI should show");
    window.draw_if_needed(|_| {});
    press_key(&ui, Key::Escape);
    assert_eq!(ui.get_overlay(), Overlay::None);
    assert_eq!(ui.get_view_mode(), ViewMode::Setup);

    ui.set_overlay(Overlay::ConfirmRootChange);
    press_key(&ui, Key::Escape);
    assert_eq!(ui.get_overlay(), Overlay::None);
    assert_eq!(ui.get_view_mode(), ViewMode::Setup);

    let closed_lyrics = Rc::new(Cell::new(0));
    let observed_closed_lyrics = Rc::clone(&closed_lyrics);
    ui.on_close_lyrics_search(move || observed_closed_lyrics.set(1));
    ui.set_overlay(Overlay::LyricsSearch);
    ui.set_lyrics_panel_state(LyricsPanelState::Searching);
    press_key(&ui, Key::Escape);
    assert_eq!(ui.get_overlay(), Overlay::None);
    assert_eq!(closed_lyrics.get(), 1);

    press_key(&ui, Key::Escape);
    assert_eq!(ui.get_view_mode(), ViewMode::Rehearsal);
}

#[test]
fn delayed_lyrics_results_cannot_cross_project_boundaries() {
    let _window = setup_window();
    let first_id = Uuid::new_v4();
    let second_id = Uuid::new_v4();
    let projects = vec![
        ProjectSummary {
            id: first_id,
            title: "First".into(),
            path: PathBuf::from("/projects/first"),
            source_available: true,
            document_revision: None,
        },
        ProjectSummary {
            id: second_id,
            title: "Second".into(),
            path: PathBuf::from("/projects/second"),
            source_available: true,
            document_revision: None,
        },
    ];
    let mut app_data = app_data_with_projects(projects, first_id);
    let first_context = begin_lyrics_context(&mut app_data).expect("first project is selected");
    app_data.selected_id = Some(second_id);
    let app_data = Mutex::new(app_data);
    let ui = K3Window::new().expect("test UI should construct");
    ui.set_overlay(Overlay::LyricsSearch);
    let stale_choice =
        LyricsChoice::for_test("test", "First", "Singer", 180.0, "[00:01.00] stale lyric");

    apply_lyrics_search_result(
        &ui.as_weak(),
        &app_data,
        &first_context,
        Ok(LyricsSearch::Candidates(vec![stale_choice])),
    );

    assert_eq!(ui.get_lyrics_candidates().row_count(), 0);
    assert!(app_data.lock().unwrap().lyrics_choices.is_empty());
}

#[test]
fn reopened_lyrics_search_rejects_the_previous_search_result() {
    let _window = setup_window();
    let project_id = Uuid::new_v4();
    let project = ProjectSummary {
        id: project_id,
        title: "Same project".into(),
        path: PathBuf::from("/projects/same"),
        source_available: true,
        document_revision: None,
    };
    let mut app_data = app_data_with_projects(vec![project], project_id);
    let previous_context = begin_lyrics_context(&mut app_data).expect("project is selected");
    super::invalidate_lyrics_context(&mut app_data);
    let current_context = begin_lyrics_context(&mut app_data).expect("project is selected");
    assert_ne!(previous_context, current_context);

    let app_data = Mutex::new(app_data);
    let ui = K3Window::new().expect("test UI should construct");
    ui.set_overlay(Overlay::LyricsSearch);
    apply_lyrics_search_result(
        &ui.as_weak(),
        &app_data,
        &previous_context,
        Ok(LyricsSearch::Candidates(vec![LyricsChoice::for_test(
            "test",
            "Same project",
            "Singer",
            180.0,
            "[00:01.00] stale lyric",
        )])),
    );

    assert_eq!(ui.get_lyrics_candidates().row_count(), 0);
    assert!(app_data.lock().unwrap().lyrics_choices.is_empty());
}

#[test]
fn major_playback_states_render_without_losing_the_project() {
    let window = setup_window();
    let ui = K3Window::new().expect("test UI should construct");
    ui.set_view_mode(ViewMode::Rehearsal);
    ui.set_playback_state(PlaybackState::Paused);
    ui.set_song_title(SharedString::from("State fixture"));
    ui.set_lyrics(ModelRc::new(VecModel::from(vec![LyricItem {
        text: SharedString::from("Current lyric"),
        at_seconds: 1.0,
    }])));

    ui.show().expect("test UI should show");
    for (state, error, changed) in [
        (PlaybackState::Paused, ErrorKind::None, false),
        (PlaybackState::Playing, ErrorKind::None, false),
        (PlaybackState::Error, ErrorKind::Playback, false),
        (PlaybackState::Paused, ErrorKind::None, true),
    ] {
        ui.set_playback_state(state);
        ui.set_error_kind(error);
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
fn reloading_the_same_project_refreshes_its_presentation_once() {
    let project_id = Uuid::new_v4();
    let snapshot = PlaybackSnapshot {
        project_id: Some(project_id),
        project_generation: 2,
        ..PlaybackSnapshot::default()
    };

    assert!(should_apply_project_snapshot(
        Some(project_id),
        1,
        &snapshot
    ));
    assert!(!should_apply_project_snapshot(
        Some(project_id),
        snapshot.project_generation,
        &snapshot,
    ));
}

#[test]
fn switching_projects_clears_the_previous_progress_immediately() {
    let _window = setup_window();
    let ui = K3Window::new().expect("test UI should construct");
    ui.set_position_seconds(93.0);
    ui.set_duration_seconds(240.0);

    begin_project_load(&ui);

    assert_eq!(ui.get_playback_state(), PlaybackState::Loading);
    assert!(ui.get_position_seconds().abs() < f32::EPSILON);
    assert!(ui.get_duration_seconds().abs() < f32::EPSILON);
}

#[test]
fn switching_projects_resets_a_previously_manipulated_timeline() {
    let window = setup_window();
    let ui = K3Window::new().expect("test UI should construct");
    ui.set_view_mode(ViewMode::Rehearsal);
    ui.set_playback_state(PlaybackState::Paused);
    ui.set_position_seconds(93.0);
    ui.set_duration_seconds(240.0);
    let seeks = Rc::new(RefCell::new(Vec::<f32>::new()));
    let observed_seeks = Rc::clone(&seeks);
    ui.on_seek_to(move |position| observed_seeks.borrow_mut().push(position));
    ui.show().expect("test UI should show");
    window.draw_if_needed(|_| {});

    click(&ui, LogicalPosition::new(900.0, 663.0));
    assert!(
        seeks
            .borrow()
            .last()
            .is_some_and(|position| *position > 0.0)
    );

    begin_project_load(&ui);
    ui.set_loading(false);
    ui.set_duration_seconds(240.0);
    ui.set_playback_state(PlaybackState::Paused);
    release_key(&ui, Key::RightArrow);

    assert!(
        seeks
            .borrow()
            .last()
            .is_some_and(|position| position.abs() < f32::EPSILON),
        "switching projects must reset the Slider's own value"
    );
}

#[test]
fn switching_projects_ignores_snapshots_from_the_previous_project() {
    let previous_project_id = Uuid::new_v4();
    let pending_project_id = Uuid::new_v4();

    assert!(!snapshot_matches_pending_open(
        Some(pending_project_id),
        previous_project_id,
    ));
    assert!(snapshot_matches_pending_open(
        Some(pending_project_id),
        pending_project_id,
    ));
    assert!(snapshot_matches_pending_open(None, previous_project_id));
}

#[test]
fn lyric_countdown_dots_are_rendered_above_the_upcoming_line() {
    let window = setup_window();
    let ui = K3Window::new().expect("test UI should construct");
    ui.set_view_mode(ViewMode::Rehearsal);
    ui.set_playback_state(PlaybackState::Playing);
    ui.set_lyrics(ModelRc::new(VecModel::from(vec![
        LyricItem {
            text: "Current line".into(),
            at_seconds: 1.0,
        },
        LyricItem {
            text: "Upcoming line".into(),
            at_seconds: 5.0,
        },
    ])));
    ui.set_active_lyric(0);
    ui.show().expect("test UI should show");
    let without_countdown = render_pixels(&window, None);

    ui.set_lyric_countdown_index(1);
    ui.set_lyric_countdown_dots(3);
    window.request_redraw();
    let with_countdown = render_pixels(&window, Some(&without_countdown));

    for (x, y) in [(777, 206), (792, 206), (807, 206)] {
        let pixel = y * 1280 + x;
        assert_ne!(
            with_countdown[pixel], without_countdown[pixel],
            "countdown dot at ({x}, {y}) was not rendered",
        );
    }
}

#[test]
fn window_and_thousand_project_first_frame_stay_within_budget() {
    let window = setup_window();
    let started = Instant::now();
    let ui = K3Window::new().expect("test UI should construct");
    ui.set_view_mode(ViewMode::Rehearsal);
    ui.set_projects(ModelRc::new(VecModel::from(
        (0..1_000)
            .map(|index| ProjectItem {
                id: format!("project-{index:04}").into(),
                title: format!("Project {index:04}").into(),
                path: format!("/tmp/project-{index:04}").into(),
                status: ProjectState::Ready,
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
