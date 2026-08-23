use std::{
    path::PathBuf,
    sync::{
        Arc,
        mpsc::{self, Receiver, SyncSender},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use k3_core::LyricsTimeline;
use k3_core::{FileProjectRepository, ProjectMutation};
use thiserror::Error;

use crate::{LoadedProject, ProjectTrack, TrackKind};

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
    pub document_revision: Option<Arc<[u8]>>,
    pub reload_required: bool,
}

impl Default for PlaybackSnapshot {
    fn default() -> Self {
        Self {
            project_id: None,
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
    /// Returns [`PlaybackError::WorkerStopped`] if the playback worker has terminated.
    pub fn execute(&self, command: PlaybackCommand) -> Result<PlaybackSnapshot, PlaybackError> {
        self.submit(command)?.recv()
    }

    /// Queues one user command and returns a response handle without waiting for audio work.
    ///
    /// # Errors
    ///
    /// Returns [`PlaybackError::WorkerStopped`] if the playback worker has terminated.
    pub fn submit(&self, command: PlaybackCommand) -> Result<PlaybackResponse, PlaybackError> {
        let (response, result) = mpsc::sync_channel(1);
        self.sender
            .send(Envelope::Execute(command, response))
            .map_err(|_| PlaybackError::WorkerStopped)?;
        Ok(PlaybackResponse { receiver: result })
    }
}

/// A pending playback result that may be awaited away from a frontend's event loop.
pub struct PlaybackResponse {
    receiver: Receiver<Result<PlaybackSnapshot, PlaybackError>>,
}

impl PlaybackResponse {
    /// # Errors
    ///
    /// Returns [`PlaybackError::WorkerStopped`] if the worker ends before answering.
    pub fn recv(self) -> Result<PlaybackSnapshot, PlaybackError> {
        self.receiver
            .recv()
            .map_err(|_| PlaybackError::WorkerStopped)?
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
    Execute(
        PlaybackCommand,
        SyncSender<Result<PlaybackSnapshot, PlaybackError>>,
    ),
    Stop,
}

struct WorkerState<B> {
    backend: B,
    project: Option<LoadedProject>,
    snapshot: PlaybackSnapshot,
}

fn run_worker(backend: impl PlaybackBackend, receiver: &Receiver<Envelope>) {
    let mut state = WorkerState {
        backend,
        project: None,
        snapshot: PlaybackSnapshot::default(),
    };
    while let Ok(envelope) = receiver.recv() {
        match envelope {
            Envelope::Execute(command, response) => {
                let _ = response.send(Ok(state.execute(command)));
            }
            Envelope::Stop => break,
        }
    }
}

impl<B: PlaybackBackend> WorkerState<B> {
    fn execute(&mut self, command: PlaybackCommand) -> PlaybackSnapshot {
        if let Err(error) = self.apply_command(command) {
            self.snapshot.status = PlaybackStatus::Error;
            if self.snapshot.error.is_none() {
                self.snapshot.error = Some(error.to_string());
            }
        }
        self.snapshot.clone()
    }

    fn apply_command(&mut self, command: PlaybackCommand) -> Result<(), PlaybackError> {
        match command {
            PlaybackCommand::Load(project) => self.load(project, false, Duration::ZERO),
            PlaybackCommand::Toggle | PlaybackCommand::Restart
                if self.snapshot.status == PlaybackStatus::Finished =>
            {
                let track = self.snapshot.track.ok_or(PlaybackError::NoProject)?;
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
            PlaybackCommand::SwitchTrack(kind) => self.switch_track(kind),
            PlaybackCommand::SetVolume(volume) => {
                self.audio(AudioCommand::SetVolume(volume.clamp(0.0, 1.0)))
            }
            PlaybackCommand::SetKeyShift(semitones) => self.set_key_shift(semitones),
            PlaybackCommand::Refresh => self.refresh(),
            PlaybackCommand::Retry => self.retry(),
        }
    }

    fn load(
        &mut self,
        project: LoadedProject,
        should_play: bool,
        position: Duration,
    ) -> Result<(), PlaybackError> {
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
            .ok_or(PlaybackError::NoPlayableTrack)?;
        self.snapshot.track = Some(track);
        self.load_track(track, position, should_play)
    }

    fn switch_track(&mut self, kind: TrackKind) -> Result<(), PlaybackError> {
        let should_play = self.snapshot.status == PlaybackStatus::Playing;
        let position = self.snapshot.position;
        self.load_track(kind, position, should_play)?;
        self.snapshot.track = Some(kind);
        Ok(())
    }

    fn set_key_shift(&mut self, semitones: i8) -> Result<(), PlaybackError> {
        if !(-6..=6).contains(&semitones) {
            return Err(PlaybackError::InvalidKeyShift(semitones));
        }
        let Some(track) = self.snapshot.track else {
            return Err(PlaybackError::NoProject);
        };
        let project_root = self
            .project
            .as_ref()
            .ok_or(PlaybackError::NoProject)?
            .root
            .clone();
        let expected_revision = self
            .project
            .as_ref()
            .and_then(|project| project.document_revision.clone());
        let (updated_project, changed_since_load) = FileProjectRepository
            .apply_checked(
                &project_root,
                ProjectMutation::SetKeyShift(semitones),
                expected_revision.as_deref(),
            )
            .map_err(|error| PlaybackError::Project(error.to_string()))?;
        self.snapshot.key_shift_semitones = semitones;
        if let Some(project) = &mut self.project {
            project.key_shift_semitones = semitones;
            project.document_revision = updated_project.document_revision().map(Arc::from);
            self.snapshot.document_revision = project.document_revision.clone();
        }
        self.snapshot.reload_required |= changed_since_load;
        self.load_track(
            track,
            self.snapshot.position,
            self.snapshot.status == PlaybackStatus::Playing,
        )
    }

    fn retry(&mut self) -> Result<(), PlaybackError> {
        let track = self.snapshot.track.ok_or(PlaybackError::NoProject)?;
        self.load_track(track, self.snapshot.position, false)
    }

    fn load_track(
        &mut self,
        kind: TrackKind,
        position: Duration,
        should_play: bool,
    ) -> Result<(), PlaybackError> {
        let path = self
            .project
            .as_ref()
            .and_then(|project| project.track(kind))
            .filter(|track| track.available())
            .and_then(|track| track.path.clone())
            .ok_or(PlaybackError::TrackUnavailable(kind))?;
        self.audio(AudioCommand::Load {
            path,
            position,
            should_play,
            key_shift_semitones: self.snapshot.key_shift_semitones,
        })
    }

    fn audio(&mut self, command: AudioCommand) -> Result<(), PlaybackError> {
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
                Err(PlaybackError::Audio(error))
            }
        }
    }

    fn refresh(&mut self) -> Result<(), PlaybackError> {
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
pub enum PlaybackError {
    #[error("playback worker stopped unexpectedly")]
    WorkerStopped,
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
