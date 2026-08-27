use crate::{
    AudioCommand, AudioSnapshot, PlaybackBackend, PlaybackStatus, rodio_player::AudioPlayer,
};

/// Production playback adapter backed by Rodio and the operating system's default output device.
pub struct RodioBackend {
    player: Option<AudioPlayer>,
    volume: f32,
}

impl Default for RodioBackend {
    fn default() -> Self {
        Self {
            player: None,
            volume: 1.0,
        }
    }
}

impl PlaybackBackend for RodioBackend {
    fn execute(&mut self, command: AudioCommand) -> Result<AudioSnapshot, String> {
        match command {
            AudioCommand::Load {
                path,
                position,
                should_play,
                key_shift_semitones,
            } => {
                if let Some(player) = &mut self.player {
                    player
                        .load(&path, position, should_play, key_shift_semitones)
                        .map_err(|error| error.to_string())?;
                } else {
                    let player = AudioPlayer::open_paused(&path, key_shift_semitones)
                        .map_err(|error| error.to_string())?;
                    player.set_volume(self.volume);
                    if !position.is_zero() {
                        player
                            .seek_to(position)
                            .map_err(|error| error.to_string())?;
                    }
                    if should_play {
                        player.play_prepared();
                    }
                    self.player = Some(player);
                }
            }
            AudioCommand::Toggle => self
                .player
                .as_ref()
                .ok_or_else(|| "no audio track is loaded".to_owned())?
                .toggle(),
            AudioCommand::SeekTo(position) => self
                .player
                .as_ref()
                .ok_or_else(|| "no audio track is loaded".to_owned())?
                .seek_to(position)
                .map_err(|error| error.to_string())?,
            AudioCommand::SetVolume(volume) => {
                self.volume = volume.clamp(0.0, 1.0);
                if let Some(player) = &self.player {
                    player.set_volume(self.volume);
                }
            }
            AudioCommand::Refresh => {}
        }
        self.snapshot()
    }
}

impl RodioBackend {
    #[must_use]
    pub(crate) fn player(&self) -> Option<&AudioPlayer> {
        self.player.as_ref()
    }

    fn snapshot(&self) -> Result<AudioSnapshot, String> {
        let Some(player) = &self.player else {
            return Ok(AudioSnapshot {
                volume: self.volume,
                ..AudioSnapshot::default()
            });
        };
        if let Some(error) = player.take_stream_error() {
            return Err(error);
        }
        let status = if player.is_finished() {
            PlaybackStatus::Finished
        } else if player.is_paused() {
            PlaybackStatus::Paused
        } else {
            PlaybackStatus::Playing
        };
        Ok(AudioSnapshot {
            status,
            position: player.position(),
            duration: player.duration(),
            volume: player.volume(),
        })
    }
}
