use std::{
    path::PathBuf,
    sync::{
        Arc,
        mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use k3_core::LyricsTimeline;
use k3_core::{FileProjectRepository, ProjectMutation};
use thiserror::Error;

use crate::{LoadedProject, ProjectRevision, ProjectTrack, TrackKind};

/// Coarse playback state shared by audio adapters and frontends.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PlaybackStatus {
    #[default]
    Unavailable,
    Loading,
    Paused,
    Playing,
    Finished,
    Error,
}

/// Audio-only state returned by a [`PlaybackBackend`] adapter.
#[derive(Clone, Debug, PartialEq)]
pub struct AudioSnapshot {
    pub status: PlaybackStatus,
    pub position: Duration,
    pub duration: Option<Duration>,
    pub volume: f32,
}

impl Default for AudioSnapshot {
    fn default() -> Self {
        Self {
            status: PlaybackStatus::Unavailable,
            position: Duration::ZERO,
            duration: None,
            volume: 1.0,
        }
    }
}

/// Low-level commands implemented at the audio adapter seam.
#[derive(Clone, Debug, PartialEq)]
pub enum AudioCommand {
    Load {
        path: PathBuf,
        position: Duration,
        should_play: bool,
        key_shift_semitones: i8,
    },
    Toggle,
    SeekTo(Duration),
    SetVolume(f32),
    Refresh,
}

/// Adapter interface for the operating-system audio implementation.
pub trait PlaybackBackend: Send + 'static {
    /// # Errors
    ///
    /// Returns a user-facing adapter error when the audio device or media operation fails.
    fn execute(&mut self, command: AudioCommand) -> Result<AudioSnapshot, String>;
}

/// User-intent commands accepted by [`PlaybackService`].
#[derive(Clone, Debug)]
pub enum PlaybackCommand {
    Load(LoadedProject),
    Toggle,
    SeekBy(i64),
    SeekTo(Duration),
    Restart,
    SwitchTrack(TrackKind),
    SetVolume(f32),
    SetKeyShift(i8),
    Refresh,
    Retry,
}

/// Immutable presentation state produced after every playback command.
#[derive(Clone, Debug)]
pub struct PlaybackSnapshot {
    pub project_id: Option<uuid::Uuid>,
    pub project_generation: u64,
    pub title: Option<String>,
    pub tracks: Vec<ProjectTrack>,
    pub track: Option<TrackKind>,
    pub status: PlaybackStatus,
    pub position: Duration,
    pub duration: Option<Duration>,
    pub volume: f32,
    pub key_shift_semitones: i8,
    pub lyrics: Option<Arc<LyricsTimeline>>,
    pub error: Option<String>,
    pub document_revision: Option<ProjectRevision>,
    pub reload_required: bool,
}

impl Default for PlaybackSnapshot {
    fn default() -> Self {
        Self {
            project_id: None,
            project_generation: 0,
            title: None,
            tracks: Vec::new(),
            track: None,
            status: PlaybackStatus::Unavailable,
            position: Duration::ZERO,
            duration: None,
            volume: 1.0,
            key_shift_semitones: 0,
            lyrics: None,
            error: None,
            document_revision: None,
            reload_required: false,
        }
    }
}

/// Owns one audio adapter on a dedicated worker thread.
pub struct PlaybackService {
    sender: mpsc::Sender<Envelope>,
    worker: Option<JoinHandle<()>>,
}

impl PlaybackService {
    /// # Panics
    ///
    /// Panics only when the operating system refuses to create the playback worker thread.
    #[must_use]
    pub fn start(backend: impl PlaybackBackend) -> Self {
        let (sender, receiver) = mpsc::channel();
        let worker = thread::Builder::new()
            .name("k3-playback".into())
            .spawn(move || run_worker(backend, &receiver))
            .expect("failed to start playback worker");
        Self {
            sender,
            worker: Some(worker),
        }
    }

    /// Executes one user command on the playback thread and returns the resulting snapshot.
    ///
    /// # Errors
    ///
    /// Returns [`PlaybackServiceError::WorkerStopped`] if the playback worker has terminated.
    pub fn execute(
        &self,
        command: PlaybackCommand,
    ) -> Result<PlaybackSnapshot, PlaybackServiceError> {
        let (response, result) = mpsc::sync_channel(1);
        self.sender
            .send(Envelope::Execute(command, response))
            .map_err(|_| PlaybackServiceError::WorkerStopped)?;
        result
            .recv()
            .map_err(|_| PlaybackServiceError::WorkerStopped)
    }

    /// Queues one command without creating a per-command response waiter.
    ///
    /// # Errors
    ///
    /// Returns [`PlaybackServiceError::WorkerStopped`] if the playback worker has terminated.
    pub fn dispatch(&self, command: PlaybackCommand) -> Result<(), PlaybackServiceError> {
        self.sender
            .send(Envelope::Dispatch(command))
            .map_err(|_| PlaybackServiceError::WorkerStopped)
    }

    /// Subscribes to the ordered stream of future playback snapshots.
    ///
    /// # Errors
    ///
    /// Returns [`PlaybackServiceError::WorkerStopped`] if the playback worker has terminated.
    pub fn subscribe(&self) -> Result<Receiver<PlaybackSnapshot>, PlaybackServiceError> {
        let (sender, receiver) = mpsc::channel();
        self.sender
            .send(Envelope::Subscribe(sender))
            .map_err(|_| PlaybackServiceError::WorkerStopped)?;
        Ok(receiver)
    }
}

impl Drop for PlaybackService {
    fn drop(&mut self) {
        let _ = self.sender.send(Envelope::Stop);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

enum Envelope {
    Execute(PlaybackCommand, SyncSender<PlaybackSnapshot>),
    Dispatch(PlaybackCommand),
    Subscribe(Sender<PlaybackSnapshot>),
    Stop,
}

/// Canonical synchronous playback state machine shared by every frontend.
pub struct PlaybackEngine<B> {
    pub(crate) backend: B,
    pub(crate) project: Option<LoadedProject>,
    pub(crate) snapshot: PlaybackSnapshot,
}

impl<B: PlaybackBackend> PlaybackEngine<B> {
    #[must_use]
    pub fn new(backend: B) -> Self {
        Self {
            backend,
            project: None,
            snapshot: PlaybackSnapshot::default(),
        }
    }
}

fn run_worker(backend: impl PlaybackBackend, receiver: &Receiver<Envelope>) {
    let mut state = PlaybackEngine::new(backend);
    let mut subscribers = Vec::new();
    loop {
        match receiver.recv_timeout(Duration::from_millis(80)) {
            Ok(Envelope::Execute(command, response)) => {
                publish_command_started(&mut subscribers, &state.snapshot, &command);
                let snapshot = state.execute(command);
                let _ = response.send(snapshot.clone());
                publish_snapshot(&mut subscribers, &snapshot);
            }
            Ok(Envelope::Dispatch(command)) => {
                publish_command_started(&mut subscribers, &state.snapshot, &command);
                let snapshot = state.execute(command);
                publish_snapshot(&mut subscribers, &snapshot);
            }
            Ok(Envelope::Subscribe(subscriber)) => subscribers.push(subscriber),
            Ok(Envelope::Stop) | Err(RecvTimeoutError::Disconnected) => break,
            Err(RecvTimeoutError::Timeout) if state.snapshot.project_id.is_some() => {
                let snapshot = state.execute(PlaybackCommand::Refresh);
                publish_snapshot(&mut subscribers, &snapshot);
            }
            Err(RecvTimeoutError::Timeout) => {}
        }
    }
}

fn publish_command_started(
    subscribers: &mut Vec<Sender<PlaybackSnapshot>>,
    current: &PlaybackSnapshot,
    command: &PlaybackCommand,
) {
    if matches!(command, PlaybackCommand::SwitchTrack(_)) {
        let mut loading = current.clone();
        loading.status = PlaybackStatus::Loading;
        loading.error = None;
        publish_snapshot(subscribers, &loading);
    }
}

fn publish_snapshot(subscribers: &mut Vec<Sender<PlaybackSnapshot>>, snapshot: &PlaybackSnapshot) {
    subscribers.retain(|subscriber| subscriber.send(snapshot.clone()).is_ok());
}

impl<B: PlaybackBackend> PlaybackEngine<B> {
    pub fn execute(&mut self, command: PlaybackCommand) -> PlaybackSnapshot {
        if let Err(error) = self.apply_command(command) {
            self.snapshot.status = PlaybackStatus::Error;
            if self.snapshot.error.is_none() {
                self.snapshot.error = Some(error.to_string());
            }
        }
        self.snapshot.clone()
    }

    fn apply_command(&mut self, command: PlaybackCommand) -> Result<(), PlaybackFailure> {
        match command {
            PlaybackCommand::Load(project) => self.load(project, false, Duration::ZERO),
            PlaybackCommand::Toggle | PlaybackCommand::Restart
                if self.snapshot.status == PlaybackStatus::Finished =>
            {
                let track = self.snapshot.track.ok_or(PlaybackFailure::NoProject)?;
                self.load_track(track, Duration::ZERO, true)
            }
            PlaybackCommand::Toggle => self.audio(AudioCommand::Toggle),
            PlaybackCommand::SeekBy(seconds) => {
                let target = if seconds.is_negative() {
                    self.snapshot
                        .position
                        .saturating_sub(Duration::from_secs(seconds.unsigned_abs()))
                } else {
                    self.snapshot
                        .position
                        .saturating_add(Duration::from_secs(seconds.unsigned_abs()))
                };
                self.audio(AudioCommand::SeekTo(target))
            }
            PlaybackCommand::SeekTo(position) => self.audio(AudioCommand::SeekTo(position)),
            PlaybackCommand::Restart => self.audio(AudioCommand::SeekTo(Duration::ZERO)),
            PlaybackCommand::SwitchTrack(kind) => self.apply_track_switch(kind),
            PlaybackCommand::SetVolume(volume) => {
                self.audio(AudioCommand::SetVolume(volume.clamp(0.0, 1.0)))
            }
            PlaybackCommand::SetKeyShift(semitones) => self.apply_key_shift(semitones, true),
            PlaybackCommand::Refresh => self.refresh(),
            PlaybackCommand::Retry => self.retry(),
        }
    }

    fn load(
        &mut self,
        project: LoadedProject,
        should_play: bool,
        position: Duration,
    ) -> Result<(), PlaybackFailure> {
        self.snapshot.project_generation = self.snapshot.project_generation.wrapping_add(1);
        self.project = Some(project);
        self.snapshot.track = None;
        self.snapshot.status = PlaybackStatus::Loading;
        self.snapshot.position = Duration::ZERO;
        self.snapshot.duration = None;
        self.snapshot.error = None;
        self.snapshot.reload_required = false;
        self.sync_project_snapshot();
        let track = self
            .project
            .as_ref()
            .and_then(LoadedProject::default_track)
            .ok_or(PlaybackFailure::NoPlayableTrack)?;
        self.snapshot.track = Some(track);
        self.load_track(track, position, should_play)
    }

    fn apply_track_switch(&mut self, kind: TrackKind) -> Result<(), PlaybackFailure> {
        let should_play = self.snapshot.status == PlaybackStatus::Playing;
        let position = self.snapshot.position;
        self.load_track(kind, position, should_play)?;
        self.snapshot.track = Some(kind);
        Ok(())
    }

    pub(crate) fn apply_key_shift(
        &mut self,
        semitones: i8,
        persist: bool,
    ) -> Result<(), PlaybackFailure> {
        if !(-6..=6).contains(&semitones) {
            return Err(PlaybackFailure::InvalidKeyShift(semitones));
        }
        let Some(track) = self.snapshot.track else {
            return Err(PlaybackFailure::NoProject);
        };
        let persisted = if persist {
            let project = self.project.as_ref().ok_or(PlaybackFailure::NoProject)?;
            Some(
                FileProjectRepository
                    .apply_checked(
                        &project.root,
                        ProjectMutation::SetKeyShift(semitones),
                        project.document_revision.as_ref(),
                    )
                    .map_err(|error| PlaybackFailure::Project(error.to_string()))?,
            )
        } else {
            None
        };
        self.snapshot.key_shift_semitones = semitones;
        if let Some(project) = &mut self.project {
            project.key_shift_semitones = semitones;
            if let Some((updated_project, _)) = &persisted {
                project.document_revision = updated_project.document_revision().cloned();
            }
            self.snapshot.document_revision = project.document_revision.clone();
        }
        if let Some((_, changed_since_load)) = persisted {
            self.snapshot.reload_required |= changed_since_load;
        }
        self.load_track(
            track,
            self.snapshot.position,
            self.snapshot.status == PlaybackStatus::Playing,
        )
    }

    fn retry(&mut self) -> Result<(), PlaybackFailure> {
        let track = self.snapshot.track.ok_or(PlaybackFailure::NoProject)?;
        self.load_track(track, self.snapshot.position, false)
    }

    pub(crate) fn load_track(
        &mut self,
        kind: TrackKind,
        position: Duration,
        should_play: bool,
    ) -> Result<(), PlaybackFailure> {
        let path = self
            .project
            .as_ref()
            .and_then(|project| project.track(kind))
            .filter(|track| track.available())
            .and_then(|track| track.path.clone())
            .ok_or(PlaybackFailure::TrackUnavailable(kind))?;
        self.audio(AudioCommand::Load {
            path,
            position,
            should_play,
            key_shift_semitones: if kind == TrackKind::Take {
                0
            } else {
                self.snapshot.key_shift_semitones
            },
        })
    }

    pub(crate) fn audio(&mut self, command: AudioCommand) -> Result<(), PlaybackFailure> {
        match self.backend.execute(command) {
            Ok(audio) => {
                self.snapshot.status = audio.status;
                self.snapshot.position = audio.position;
                self.snapshot.duration = audio.duration;
                self.snapshot.volume = audio.volume;
                self.snapshot.error = None;
                Ok(())
            }
            Err(error) => {
                self.snapshot.status = PlaybackStatus::Error;
                self.snapshot.error = Some(error.clone());
                Err(PlaybackFailure::Audio(error))
            }
        }
    }

    fn refresh(&mut self) -> Result<(), PlaybackFailure> {
        let latched_error = self.snapshot.error.clone();
        self.audio(AudioCommand::Refresh)?;
        if let Some(error) = latched_error {
            self.snapshot.status = PlaybackStatus::Error;
            self.snapshot.error = Some(error);
        }
        Ok(())
    }

    fn sync_project_snapshot(&mut self) {
        let Some(project) = &self.project else {
            return;
        };
        self.snapshot.project_id = Some(project.id);
        self.snapshot.title = Some(project.title.clone());
        self.snapshot.tracks.clone_from(&project.tracks);
        self.snapshot.key_shift_semitones = project.key_shift_semitones;
        self.snapshot.lyrics = project.lyrics.clone().map(Arc::new);
        self.snapshot.document_revision = project.document_revision.clone();
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub(crate) enum PlaybackFailure {
    #[error("no project is loaded")]
    NoProject,
    #[error("project has no playable track")]
    NoPlayableTrack,
    #[error("{0:?} track is unavailable")]
    TrackUnavailable(TrackKind),
    #[error("key shift must be between -6 and +6 semitones: {0}")]
    InvalidKeyShift(i8),
    #[error("audio: {0}")]
    Audio(String),
    #[error("project: {0}")]
    Project(String),
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum PlaybackServiceError {
    #[error("playback worker stopped unexpectedly")]
    WorkerStopped,
}
