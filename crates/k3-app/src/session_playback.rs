use std::{error::Error, path::Path, path::PathBuf, time::Duration};

use k3_core::Project;

use crate::{
    AudioCommand, LoadedProject, PlaybackCommand, PlaybackEngine, PlaybackSnapshot, ProjectTrack,
    RodioBackend, TrackKind,
};

/// The TUI uses the canonical synchronous engine directly on its event-loop thread.
pub type SessionPlayback = PlaybackEngine<RodioBackend>;

/// Backwards-compatible name for the complete canonical track vocabulary.
pub type SessionTrackKind = TrackKind;

impl PlaybackEngine<RodioBackend> {
    #[must_use]
    pub fn open(project: &Project) -> Self {
        let mut playback = Self::new(RodioBackend::default());
        playback.execute(PlaybackCommand::Load(LoadedProject::from_project(project)));
        playback
    }

    #[must_use]
    pub fn snapshot(&self) -> PlaybackSnapshot {
        self.snapshot.clone()
    }

    #[must_use]
    pub fn position(&self) -> Duration {
        self.snapshot.position
    }

    #[must_use]
    pub fn selected_track(&self) -> TrackKind {
        self.snapshot.track.unwrap_or(TrackKind::Original)
    }

    #[must_use]
    pub fn has_track(&self, kind: TrackKind) -> bool {
        self.project
            .as_ref()
            .and_then(|project| project.track(kind))
            .is_some_and(ProjectTrack::available)
    }

    #[must_use]
    pub fn has_audio(&self) -> bool {
        self.backend.player().is_some()
    }

    #[must_use]
    pub fn error(&self) -> Option<&str> {
        self.snapshot.error.as_deref()
    }

    #[must_use]
    pub fn monitor_player(&self) -> Option<&crate::AudioPlayer> {
        self.backend.player()
    }

    pub fn refresh_stream_error(&mut self) {
        self.execute(PlaybackCommand::Refresh);
    }

    pub fn adjust_volume(&mut self, delta: f32) {
        self.execute(PlaybackCommand::SetVolume(self.snapshot.volume + delta));
    }

    pub fn switch_track(&mut self, kind: TrackKind) {
        self.execute(PlaybackCommand::SwitchTrack(kind));
    }

    /// Prepares the selected backing source at the beginning without starting playback.
    ///
    /// # Errors
    ///
    /// Returns an audio error when playback is unavailable or the source cannot be loaded.
    pub fn prepare_recording(&mut self) -> Result<(), Box<dyn Error>> {
        let track = self.snapshot.track.ok_or("playback is unavailable")?;
        self.load_track(track, Duration::ZERO, false)?;
        Ok(())
    }

    /// Starts a source prepared by [`Self::prepare_recording`].
    ///
    /// # Errors
    ///
    /// Returns an error when playback is unavailable.
    pub fn play_from_start(&mut self) -> Result<(), Box<dyn Error>> {
        self.audio(AudioCommand::Toggle)?;
        Ok(())
    }

    /// Loads and starts a rendered take without applying the project key shift again.
    ///
    /// # Errors
    ///
    /// Returns an audio error when the take cannot be opened.
    pub fn play_take(&mut self, path: &Path) -> Result<(), Box<dyn Error>> {
        let project = self.project.as_mut().ok_or("playback is unavailable")?;
        if let Some(track) = project
            .tracks
            .iter_mut()
            .find(|track| track.kind == TrackKind::Take)
        {
            track.path = Some(path.to_path_buf());
        } else {
            project.tracks.push(ProjectTrack {
                kind: TrackKind::Take,
                path: Some(path.to_path_buf()),
            });
        }
        self.snapshot.tracks.clone_from(&project.tracks);
        self.load_track(TrackKind::Take, Duration::ZERO, true)?;
        self.snapshot.track = Some(TrackKind::Take);
        Ok(())
    }

    /// Returns the accompaniment used when rendering a recorded take.
    ///
    /// # Errors
    ///
    /// Returns an error when the project has no accompaniment stem.
    pub fn accompaniment_path(&self) -> Result<PathBuf, Box<dyn Error>> {
        self.project
            .as_ref()
            .and_then(|project| project.track(TrackKind::Accompaniment))
            .and_then(|track| track.path.clone())
            .filter(|path| path.is_file())
            .ok_or_else(|| "project accompaniment is unavailable".into())
    }

    /// Applies a key shift in memory while the TUI owns project persistence.
    ///
    /// # Errors
    ///
    /// Returns an audio error when the selected source cannot be reloaded.
    pub fn set_key_shift(&mut self, semitones: i8) -> Result<(), Box<dyn Error>> {
        PlaybackEngine::apply_key_shift(self, semitones, false)?;
        Ok(())
    }
}
