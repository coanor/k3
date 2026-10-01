use std::{
    fs,
    path::Path,
    thread,
    time::{Duration, Instant},
};

use k3_app::{
    AudioCommand, AudioSnapshot, LoadedProject, PlaybackBackend, PlaybackCommand, PlaybackEngine,
    PlaybackService, PlaybackStatus, ProjectLibrary, ProjectTrack, SessionPlayback, TrackKind,
};
use k3_core::{CreateProject, FileProjectRepository, ProjectMutation, ProjectRepository};

#[test]
fn synchronous_playback_engine_owns_the_complete_transport_state_machine() {
    let directory = tempfile::tempdir().unwrap();
    let project = LoadedProject::open(&create_project(directory.path())).unwrap();
    let mut engine = PlaybackEngine::new(FakeAudio::default());

    let loaded = engine.execute(PlaybackCommand::Load(project));
    assert_eq!(loaded.track, Some(TrackKind::Original));
    assert_eq!(loaded.status, PlaybackStatus::Paused);

    assert_eq!(
        engine.execute(PlaybackCommand::Toggle).status,
        PlaybackStatus::Playing
    );
    assert_eq!(
        engine.execute(PlaybackCommand::SeekBy(5)).position,
        Duration::from_secs(5)
    );
}

#[test]
fn playback_service_loads_paused_then_applies_user_commands() {
    let directory = tempfile::tempdir().unwrap();
    let project = LoadedProject::open(&create_project(directory.path())).unwrap();
    let service = PlaybackService::start(FakeAudio::default());

    let loaded = service.execute(PlaybackCommand::Load(project)).unwrap();
    assert_eq!(loaded.track, Some(TrackKind::Original));
    assert_eq!(loaded.status, PlaybackStatus::Paused);
    assert_eq!(
        loaded.lyrics.as_ref().unwrap().lines()[0].text,
        "First line"
    );

    let playing = service.execute(PlaybackCommand::Toggle).unwrap();
    assert_eq!(playing.status, PlaybackStatus::Playing);

    let moved = service.execute(PlaybackCommand::SeekBy(5)).unwrap();
    assert_eq!(moved.position, Duration::from_secs(5));
}

#[test]
fn loading_a_selected_take_starts_that_take_from_the_beginning() {
    let directory = tempfile::tempdir().unwrap();
    let mut project = LoadedProject::open(&create_project(directory.path())).unwrap();
    let take_path = project.track(TrackKind::Original).unwrap().path.clone();
    project.tracks.push(ProjectTrack {
        kind: TrackKind::Take,
        path: take_path,
    });
    let mut engine = PlaybackEngine::new(FakeAudio::default());

    let snapshot = engine.execute(PlaybackCommand::LoadTake(project));

    assert_eq!(snapshot.track, Some(TrackKind::Take));
    assert_eq!(snapshot.status, PlaybackStatus::Playing);
    assert_eq!(snapshot.position, Duration::ZERO);
}

#[test]
fn recording_session_playback_keeps_synced_lyrics() {
    let directory = tempfile::tempdir().unwrap();
    let project_root = create_project(directory.path());
    let project = FileProjectRepository.open(&project_root).unwrap();

    let playback = SessionPlayback::open(&project);

    assert_eq!(
        playback.snapshot().lyrics.as_ref().unwrap().lines()[0].text,
        "First line",
    );
}

#[test]
fn playback_service_publishes_state_without_frontend_polling() {
    let directory = tempfile::tempdir().unwrap();
    let project = LoadedProject::open(&create_project(directory.path())).unwrap();
    let service = PlaybackService::start(FakeAudio::default());
    let states = service.subscribe().unwrap();

    service.dispatch(PlaybackCommand::Load(project)).unwrap();
    let loaded = states.recv_timeout(Duration::from_secs(1)).unwrap();
    assert_eq!(loaded.status, PlaybackStatus::Paused);

    service.dispatch(PlaybackCommand::Toggle).unwrap();
    let playing = states.recv_timeout(Duration::from_secs(1)).unwrap();
    assert_eq!(playing.status, PlaybackStatus::Playing);
}

#[test]
fn playback_service_does_not_discard_newer_snapshots_when_commands_burst() {
    let service = PlaybackService::start(FakeAudio::default());
    let states = service.subscribe().unwrap();

    service.dispatch(PlaybackCommand::Refresh).unwrap();
    service.dispatch(PlaybackCommand::Refresh).unwrap();
    service.execute(PlaybackCommand::Refresh).unwrap();

    for _ in 0..3 {
        states.recv_timeout(Duration::from_secs(1)).unwrap();
    }
}

#[test]
fn queued_relative_seeks_accumulate_in_order() {
    let directory = tempfile::tempdir().unwrap();
    let project = LoadedProject::open(&create_project(directory.path())).unwrap();
    let service = PlaybackService::start(FakeAudio::default());
    service.execute(PlaybackCommand::Load(project)).unwrap();

    service.dispatch(PlaybackCommand::SeekBy(5)).unwrap();
    service.dispatch(PlaybackCommand::SeekBy(5)).unwrap();
    assert_eq!(
        service.execute(PlaybackCommand::Refresh).unwrap().position,
        Duration::from_secs(10)
    );

    service.dispatch(PlaybackCommand::SeekBy(-5)).unwrap();
    service.dispatch(PlaybackCommand::SeekBy(-5)).unwrap();
    assert_eq!(
        service.execute(PlaybackCommand::Refresh).unwrap().position,
        Duration::ZERO
    );
}

#[test]
fn track_switch_publishes_loading_before_the_new_track_is_ready() {
    let directory = tempfile::tempdir().unwrap();
    let project = LoadedProject::open(&create_project(directory.path())).unwrap();
    let service = PlaybackService::start(FakeAudio::default());
    service.execute(PlaybackCommand::Load(project)).unwrap();
    let states = service.subscribe().unwrap();

    service
        .dispatch(PlaybackCommand::SwitchTrack(TrackKind::Original))
        .unwrap();

    assert_eq!(
        states.recv_timeout(Duration::from_secs(1)).unwrap().status,
        PlaybackStatus::Loading
    );
    assert_eq!(
        states.recv_timeout(Duration::from_secs(1)).unwrap().status,
        PlaybackStatus::Paused
    );
}

#[test]
fn playback_service_persists_key_shift_as_a_narrow_project_change() {
    let directory = tempfile::tempdir().unwrap();
    let project_root = create_project(directory.path());
    let project = LoadedProject::open(&project_root).unwrap();
    let service = PlaybackService::start(FakeAudio::default());

    service.execute(PlaybackCommand::Load(project)).unwrap();
    let shifted = service.execute(PlaybackCommand::SetKeyShift(3)).unwrap();

    assert_eq!(shifted.key_shift_semitones, 3);
    let saved = FileProjectRepository.open(&project_root).unwrap();
    assert_eq!(saved.key_shift_semitones(), 3);
}

#[test]
fn key_shift_keeps_reload_required_when_another_frontend_changed_the_project() {
    let directory = tempfile::tempdir().unwrap();
    let project_root = create_project(directory.path());
    let project = LoadedProject::open(&project_root).unwrap();
    let service = PlaybackService::start(FakeAudio::default());
    service.execute(PlaybackCommand::Load(project)).unwrap();
    FileProjectRepository
        .apply(&project_root, ProjectMutation::SetLatencyCompensation(120))
        .unwrap();

    let shifted = service.execute(PlaybackCommand::SetKeyShift(2)).unwrap();

    assert!(shifted.reload_required);
    let saved = FileProjectRepository.open(&project_root).unwrap();
    assert_eq!(saved.key_shift_semitones(), 2);
    assert_eq!(saved.latency_compensation_ms(), 120);
}

#[test]
fn later_external_write_differs_from_the_key_shift_committed_revision() {
    let library_root = tempfile::tempdir().unwrap();
    let project_root = create_project(library_root.path());
    let project = LoadedProject::open(&project_root).unwrap();
    let service = PlaybackService::start(FakeAudio::default());
    service.execute(PlaybackCommand::Load(project)).unwrap();
    let shifted = service.execute(PlaybackCommand::SetKeyShift(2)).unwrap();

    FileProjectRepository
        .apply(&project_root, ProjectMutation::SetLatencyCompensation(120))
        .unwrap();
    let scanned = ProjectLibrary::scan(library_root.path()).unwrap();

    assert_ne!(shifted.document_revision, scanned[0].document_revision);
}

#[test]
fn playback_commands_can_be_submitted_without_blocking_a_ui_thread() {
    let service = PlaybackService::start(SlowAudio);
    let states = service.subscribe().unwrap();

    let started = Instant::now();
    service.dispatch(PlaybackCommand::Refresh).unwrap();

    assert!(started.elapsed() < Duration::from_millis(50));
    assert_eq!(
        states.recv_timeout(Duration::from_secs(1)).unwrap().status,
        PlaybackStatus::Unavailable
    );
}

#[test]
fn recoverable_audio_failure_is_reported_in_a_snapshot_and_can_be_retried() {
    let directory = tempfile::tempdir().unwrap();
    let project = LoadedProject::open(&create_project(directory.path())).unwrap();
    let service = PlaybackService::start(FailOnceAudio { fail_next: true });

    let failed = service.execute(PlaybackCommand::Load(project)).unwrap();
    assert_eq!(failed.status, PlaybackStatus::Error);
    assert_eq!(failed.error.as_deref(), Some("device unavailable"));

    let refreshed = service.execute(PlaybackCommand::Refresh).unwrap();
    assert_eq!(refreshed.status, PlaybackStatus::Error);
    assert_eq!(refreshed.error.as_deref(), Some("device unavailable"));

    let recovered = service.execute(PlaybackCommand::Retry).unwrap();
    assert_eq!(recovered.status, PlaybackStatus::Paused);
    assert_eq!(recovered.error, None);
}

#[test]
fn finished_track_can_be_started_again_with_toggle_or_restart() {
    let directory = tempfile::tempdir().unwrap();
    let project = LoadedProject::open(&create_project(directory.path())).unwrap();
    let service = PlaybackService::start(FinishOnRefreshAudio::default());
    service.execute(PlaybackCommand::Load(project)).unwrap();
    assert_eq!(
        service.execute(PlaybackCommand::Refresh).unwrap().status,
        PlaybackStatus::Finished
    );

    assert_eq!(
        service.execute(PlaybackCommand::Toggle).unwrap().status,
        PlaybackStatus::Playing
    );
    assert_eq!(
        service.execute(PlaybackCommand::Refresh).unwrap().status,
        PlaybackStatus::Finished
    );
    assert_eq!(
        service.execute(PlaybackCommand::Restart).unwrap().status,
        PlaybackStatus::Playing
    );
}

#[test]
fn seeking_backward_from_finished_restarts_the_track_at_the_requested_position() {
    let directory = tempfile::tempdir().unwrap();
    let project = LoadedProject::open(&create_project(directory.path())).unwrap();
    let service = PlaybackService::start(FinishOnRefreshAudio::default());
    service.execute(PlaybackCommand::Load(project)).unwrap();
    assert_eq!(
        service.execute(PlaybackCommand::Refresh).unwrap().status,
        PlaybackStatus::Finished
    );

    let moved = service.execute(PlaybackCommand::SeekBy(-5)).unwrap();
    assert_eq!(moved.status, PlaybackStatus::Playing);
    assert_eq!(moved.position, Duration::from_secs(25));

    service.execute(PlaybackCommand::Refresh).unwrap();
    let moved = service
        .execute(PlaybackCommand::SeekTo(Duration::from_secs(10)))
        .unwrap();
    assert_eq!(moved.status, PlaybackStatus::Playing);
    assert_eq!(moved.position, Duration::from_secs(10));
}

#[test]
fn seeking_past_the_end_clamps_before_the_next_relative_seek() {
    let directory = tempfile::tempdir().unwrap();
    let project = LoadedProject::open(&create_project(directory.path())).unwrap();
    let service = PlaybackService::start(FakeAudio::default());
    service.execute(PlaybackCommand::Load(project)).unwrap();
    service
        .execute(PlaybackCommand::SeekTo(Duration::from_secs(118)))
        .unwrap();

    assert_eq!(
        service
            .execute(PlaybackCommand::SeekBy(5))
            .unwrap()
            .position,
        Duration::from_secs(120)
    );
    assert_eq!(
        service
            .execute(PlaybackCommand::SeekBy(-5))
            .unwrap()
            .position,
        Duration::from_secs(115)
    );
    assert_eq!(
        service
            .execute(PlaybackCommand::SeekTo(Duration::from_secs(200)))
            .unwrap()
            .position,
        Duration::from_secs(120)
    );
}

#[test]
fn project_without_playable_media_replaces_the_previous_snapshot() {
    let first_directory = tempfile::tempdir().unwrap();
    let missing_directory = tempfile::tempdir().unwrap();
    let first = LoadedProject::open(&create_project(first_directory.path())).unwrap();
    let missing_root = create_project(missing_directory.path());
    let missing = LoadedProject::open(&missing_root).unwrap();
    let missing_id = missing.id;
    fs::remove_file(
        missing
            .track(TrackKind::Original)
            .unwrap()
            .path
            .as_ref()
            .unwrap(),
    )
    .unwrap();
    let service = PlaybackService::start(FakeAudio::default());
    service.execute(PlaybackCommand::Load(first)).unwrap();

    let failed = service.execute(PlaybackCommand::Load(missing)).unwrap();

    assert_eq!(failed.project_id, Some(missing_id));
    assert_eq!(failed.track, None);
    assert_eq!(failed.status, PlaybackStatus::Error);
}

struct FailOnceAudio {
    fail_next: bool,
}

impl PlaybackBackend for FailOnceAudio {
    fn execute(&mut self, _command: AudioCommand) -> Result<AudioSnapshot, String> {
        if self.fail_next {
            self.fail_next = false;
            Err("device unavailable".into())
        } else {
            Ok(AudioSnapshot {
                status: PlaybackStatus::Paused,
                ..AudioSnapshot::default()
            })
        }
    }
}

struct SlowAudio;

#[derive(Default)]
struct FinishOnRefreshAudio {
    snapshot: AudioSnapshot,
}

impl PlaybackBackend for FinishOnRefreshAudio {
    fn execute(&mut self, command: AudioCommand) -> Result<AudioSnapshot, String> {
        match command {
            AudioCommand::Load {
                position,
                should_play,
                ..
            } => {
                self.snapshot.status = if should_play {
                    PlaybackStatus::Playing
                } else {
                    PlaybackStatus::Paused
                };
                self.snapshot.position = position;
                self.snapshot.duration = Some(Duration::from_secs(30));
            }
            AudioCommand::Refresh => {
                self.snapshot.status = PlaybackStatus::Finished;
                self.snapshot.position = Duration::from_secs(30);
            }
            AudioCommand::Toggle | AudioCommand::SeekTo(_) | AudioCommand::SetVolume(_) => {}
        }
        Ok(self.snapshot.clone())
    }
}

impl PlaybackBackend for SlowAudio {
    fn execute(&mut self, _command: AudioCommand) -> Result<AudioSnapshot, String> {
        thread::sleep(Duration::from_millis(100));
        Ok(AudioSnapshot::default())
    }
}

#[derive(Default)]
struct FakeAudio {
    snapshot: AudioSnapshot,
}

impl PlaybackBackend for FakeAudio {
    fn execute(&mut self, command: AudioCommand) -> Result<AudioSnapshot, String> {
        match command {
            AudioCommand::Load {
                position,
                should_play,
                ..
            } => {
                self.snapshot.position = position;
                self.snapshot.duration = Some(Duration::from_mins(2));
                self.snapshot.status = if should_play {
                    PlaybackStatus::Playing
                } else {
                    PlaybackStatus::Paused
                };
            }
            AudioCommand::Toggle => {
                self.snapshot.status = match self.snapshot.status {
                    PlaybackStatus::Playing => PlaybackStatus::Paused,
                    _ => PlaybackStatus::Playing,
                };
            }
            AudioCommand::SeekTo(position) => self.snapshot.position = position,
            AudioCommand::SetVolume(volume) => self.snapshot.volume = volume,
            AudioCommand::Refresh => {}
        }
        Ok(self.snapshot.clone())
    }
}

fn create_project(root: &Path) -> std::path::PathBuf {
    let song = root.join("song.wav");
    let lyrics = root.join("song.lrc");
    fs::write(&song, b"test audio").unwrap();
    fs::write(&lyrics, "[00:00.00]First line\n").unwrap();
    let project_root = root.join("project");
    FileProjectRepository
        .create(CreateProject {
            root: project_root.clone(),
            song,
            lyrics: Some(lyrics),
            title: Some("Test Song".into()),
        })
        .unwrap();
    project_root
}
