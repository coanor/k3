use std::{error::Error, path::Path, path::PathBuf, time::Duration};

use k3_core::{Project, SeparationState, Take};

use crate::{AudioPlayer, PlaybackCommand, PlaybackStatus, TrackKind};

/// A selectable source in the synchronous playback module used by the TUI.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionTrackKind {
    Original,
    Accompaniment,
    Vocals,
    Take,
}

/// Immutable presentation state produced by [`SessionPlayback`].
#[derive(Clone, Debug, PartialEq)]
pub struct SessionPlaybackSnapshot {
    pub status: PlaybackStatus,
    pub position: Duration,
    pub duration: Option<Duration>,
    pub volume: f32,
    pub track: SessionTrackKind,
    pub error: Option<String>,
}

impl SessionTrackKind {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Original => "original",
            Self::Accompaniment => "accompaniment",
            Self::Vocals => "vocals",
            Self::Take => "take",
        }
    }
}

impl From<TrackKind> for SessionTrackKind {
    fn from(kind: TrackKind) -> Self {
        match kind {
            TrackKind::Original => Self::Original,
            TrackKind::Accompaniment => Self::Accompaniment,
            TrackKind::Vocals => Self::Vocals,
        }
    }
}

struct SessionTrack {
    kind: SessionTrackKind,
    path: PathBuf,
}

/// Synchronous playback state shared by terminal workflows that also need the live output mixer.
pub struct SessionPlayback {
    tracks: Vec<SessionTrack>,
    selected: usize,
    audio: Option<AudioPlayer>,
    error: Option<String>,
    key_shift_semitones: i8,
}

impl SessionPlayback {
    #[must_use]
    pub fn open(project: &Project) -> Self {
        let tracks = project_tracks(project);
        let selected = tracks
            .iter()
            .position(|track| track.kind == SessionTrackKind::Accompaniment)
            .unwrap_or(0);
        let mut error = None;
        let audio =
            match AudioPlayer::open_paused(&tracks[selected].path, project.key_shift_semitones()) {
                Ok(player) => Some(player),
                Err(open_error) => {
                    error = Some(open_error.to_string());
                    None
                }
            };
        Self {
            tracks,
            selected,
            audio,
            error,
            key_shift_semitones: project.key_shift_semitones(),
        }
    }

    pub fn execute(&mut self, command: &PlaybackCommand) {
        match command {
            PlaybackCommand::Toggle => {
                if let Some(player) = &self.audio {
                    player.toggle();
                    self.error = None;
                }
            }
            PlaybackCommand::SeekBy(seconds) => {
                let result = self
                    .audio
                    .as_ref()
                    .map_or(Ok(()), |player| player.seek_by(*seconds));
                self.update_error(result);
            }
            PlaybackCommand::SeekTo(position) => {
                let result = self
                    .audio
                    .as_ref()
                    .map_or(Ok(()), |player| player.seek_to(*position));
                self.update_error(result);
            }
            PlaybackCommand::Restart => {
                let path = self.tracks[self.selected].path.clone();
                let key_shift = self.selected_key_shift();
                let result = self.audio.as_mut().map_or(Ok(()), |player| {
                    player.load(&path, Duration::ZERO, true, key_shift)
                });
                self.update_error(result);
            }
            PlaybackCommand::SwitchTrack(kind) => self.switch_track((*kind).into()),
            PlaybackCommand::SetVolume(volume) => {
                if let Some(player) = &self.audio {
                    player.set_volume(*volume);
                }
            }
            PlaybackCommand::Refresh => self.refresh_stream_error(),
            PlaybackCommand::Retry => self.retry(),
            PlaybackCommand::SetKeyShift(semitones) => {
                if let Err(error) = self.set_key_shift(*semitones) {
                    self.error = Some(error.to_string());
                }
            }
            PlaybackCommand::Load(_) => {
                self.error = Some("session playback cannot replace its project".into());
            }
        }
    }

    #[must_use]
    pub fn snapshot(&self) -> SessionPlaybackSnapshot {
        SessionPlaybackSnapshot {
            status: self.status(),
            position: self.position(),
            duration: self.duration(),
            volume: self.volume(),
            track: self.selected_track(),
            error: self.error.clone(),
        }
    }

    #[must_use]
    pub fn position(&self) -> Duration {
        self.audio
            .as_ref()
            .map_or(Duration::ZERO, AudioPlayer::position)
    }

    #[must_use]
    pub fn duration(&self) -> Option<Duration> {
        self.audio.as_ref().and_then(AudioPlayer::duration)
    }

    #[must_use]
    pub fn volume(&self) -> f32 {
        self.audio.as_ref().map_or(0.0, AudioPlayer::volume)
    }

    #[must_use]
    pub fn status(&self) -> PlaybackStatus {
        if self.error.is_some() {
            return PlaybackStatus::Error;
        }
        self.audio
            .as_ref()
            .map_or(PlaybackStatus::Unavailable, |player| {
                if player.is_finished() {
                    PlaybackStatus::Finished
                } else if player.is_paused() {
                    PlaybackStatus::Paused
                } else {
                    PlaybackStatus::Playing
                }
            })
    }

    #[must_use]
    pub fn selected_track(&self) -> SessionTrackKind {
        self.tracks[self.selected].kind
    }

    #[must_use]
    pub fn has_track(&self, kind: SessionTrackKind) -> bool {
        self.tracks.iter().any(|track| track.kind == kind)
    }

    #[must_use]
    pub fn has_audio(&self) -> bool {
        self.audio.is_some()
    }

    #[must_use]
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    #[must_use]
    pub fn monitor_player(&self) -> Option<&AudioPlayer> {
        self.audio.as_ref()
    }

    pub fn refresh_stream_error(&mut self) {
        if let Some(error) = self.audio.as_ref().and_then(AudioPlayer::take_stream_error) {
            self.error = Some(error);
        }
    }

    pub fn adjust_volume(&self, delta: f32) {
        if let Some(player) = &self.audio {
            player.adjust_volume(delta);
        }
    }

    pub fn switch_track(&mut self, kind: SessionTrackKind) {
        let Some(next) = self.tracks.iter().position(|track| track.kind == kind) else {
            self.error = Some(format!("{} track is unavailable", kind.label()));
            return;
        };
        if next == self.selected {
            return;
        }

        let path = self.tracks[next].path.clone();
        let key_shift = if kind == SessionTrackKind::Take {
            0
        } else {
            self.key_shift_semitones
        };
        let result = if let Some(player) = &mut self.audio {
            let position = player.position();
            let should_play = !player.is_paused() && !player.is_finished();
            player.load(&path, position, should_play, key_shift)
        } else {
            AudioPlayer::open(&path, key_shift).map(|player| {
                self.audio = Some(player);
            })
        };
        if result.is_ok() {
            self.selected = next;
        }
        self.update_error(result);
    }

    /// Prepares the selected backing source at the beginning without starting playback.
    ///
    /// # Errors
    ///
    /// Returns an audio error when playback is unavailable or the source cannot be loaded.
    pub fn prepare_recording(&mut self) -> Result<(), Box<dyn Error>> {
        let key_shift = self.selected_key_shift();
        let path = self.tracks[self.selected].path.clone();
        let player = self
            .audio
            .as_mut()
            .ok_or("playback is unavailable; cannot synchronize recording")?;
        player.load(&path, Duration::ZERO, false, key_shift)?;
        self.error = None;
        Ok(())
    }

    /// Starts a source prepared by [`Self::prepare_recording`].
    ///
    /// # Errors
    ///
    /// Returns an error when playback is unavailable.
    pub fn play_from_start(&mut self) -> Result<(), Box<dyn Error>> {
        let player = self
            .audio
            .as_ref()
            .ok_or("playback is unavailable; cannot synchronize recording")?;
        player.play_prepared();
        self.error = None;
        Ok(())
    }

    /// Loads and starts a rendered take without applying the project key shift again.
    ///
    /// # Errors
    ///
    /// Returns an audio error when the take cannot be opened.
    pub fn play_take(&mut self, path: &Path) -> Result<(), Box<dyn Error>> {
        let index = if let Some(index) = self
            .tracks
            .iter()
            .position(|track| track.kind == SessionTrackKind::Take)
        {
            self.tracks[index].path = path.to_path_buf();
            index
        } else {
            self.tracks.push(SessionTrack {
                kind: SessionTrackKind::Take,
                path: path.to_path_buf(),
            });
            self.tracks.len() - 1
        };
        if let Some(player) = &mut self.audio {
            player.load(path, Duration::ZERO, true, 0)?;
        } else {
            self.audio = Some(AudioPlayer::open(path, 0)?);
        }
        self.selected = index;
        self.error = None;
        Ok(())
    }

    /// Returns the accompaniment used when rendering a recorded take.
    ///
    /// # Errors
    ///
    /// Returns an error when the project has no accompaniment stem.
    pub fn accompaniment_path(&self) -> Result<PathBuf, Box<dyn Error>> {
        self.tracks
            .iter()
            .find(|track| track.kind == SessionTrackKind::Accompaniment)
            .map(|track| track.path.clone())
            .ok_or_else(|| "project accompaniment is unavailable".into())
    }

    /// Applies a key shift to source playback while preserving position and state.
    ///
    /// # Errors
    ///
    /// Returns an audio error when the selected source cannot be reloaded.
    pub fn set_key_shift(&mut self, semitones: i8) -> Result<(), Box<dyn Error>> {
        self.key_shift_semitones = semitones;
        let key_shift = self.selected_key_shift();
        let path = self.tracks[self.selected].path.clone();
        let Some(player) = &mut self.audio else {
            return Ok(());
        };
        let position = player.position();
        let should_play = !player.is_paused() && !player.is_finished();
        player.load(&path, position, should_play, key_shift)
    }

    fn retry(&mut self) {
        let path = self.tracks[self.selected].path.clone();
        let key_shift = self.selected_key_shift();
        let position = self.position();
        let result = if let Some(player) = &mut self.audio {
            player.load(&path, position, false, key_shift)
        } else {
            AudioPlayer::open_paused(&path, key_shift).map(|player| {
                self.audio = Some(player);
            })
        };
        self.update_error(result);
    }

    fn selected_key_shift(&self) -> i8 {
        if self.selected_track() == SessionTrackKind::Take {
            0
        } else {
            self.key_shift_semitones
        }
    }

    fn update_error(&mut self, result: Result<(), Box<dyn Error>>) {
        self.error = result.err().map(|error| error.to_string());
    }
}

fn project_tracks(project: &Project) -> Vec<SessionTrack> {
    let mut tracks = vec![SessionTrack {
        kind: SessionTrackKind::Original,
        path: project.source_path(),
    }];
    if let SeparationState::Ready(manifest) = project.separation() {
        tracks.push(SessionTrack {
            kind: SessionTrackKind::Accompaniment,
            path: manifest.accompaniment.resolve(project.root()),
        });
        tracks.push(SessionTrack {
            kind: SessionTrackKind::Vocals,
            path: manifest.vocals.resolve(project.root()),
        });
    }
    if let Some(mix_audio) = project.takes().last().and_then(Take::mix_audio) {
        tracks.push(SessionTrack {
            kind: SessionTrackKind::Take,
            path: mix_audio.resolve(project.root()),
        });
    }
    tracks
}
