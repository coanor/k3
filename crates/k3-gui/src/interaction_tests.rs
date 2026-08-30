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

use super::{
    ErrorKind, K3Window, LyricItem, Overlay, PlaybackAction, PlaybackState,
    PlaybackTransitionTracker, ProjectItem, ProjectState, RecordingState, TrackSelection, ViewMode,
    should_apply_project_snapshot,
};
use k3_app::{PlaybackSnapshot, PlaybackStatus, TrackKind};
use uuid::Uuid;

thread_local! {
    static WINDOW: Rc<MinimalSoftwareWindow> =
        MinimalSoftwareWindow::new(RepaintBufferType::ReusedBuffer);
}

#[test]
fn recording_shortcuts_keep_all_rehearsal_controls_available() {
    let window = setup_window();
    let ui = K3Window::new().expect("test UI should construct");
    ui.set_view_mode(ViewMode::Rehearsal);
    ui.set_playback_state(PlaybackState::Paused);
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
    click(&ui, LogicalPosition::new(630.0, 759.0));
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
    ui.set_view_mode(ViewMode::Rehearsal);
    ui.set_playback_state(PlaybackState::Paused);
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

    press_key(&ui, Key::Escape);
    assert_eq!(ui.get_view_mode(), ViewMode::Rehearsal);
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
