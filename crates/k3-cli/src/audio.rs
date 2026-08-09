use std::{error::Error, fs::File, path::Path, time::Duration};

use rodio::{Decoder, DeviceSinkBuilder, MixerDeviceSink, Player, Source};

/// Owns the operating-system audio stream and one controllable decoded track.
///
/// This adapter deliberately knows nothing about the TUI or project format.
pub struct AudioPlayer {
    _device: MixerDeviceSink,
    player: Player,
    duration: Option<Duration>,
}

impl AudioPlayer {
    pub fn open(path: &Path) -> Result<Self, Box<dyn Error>> {
        let device = DeviceSinkBuilder::open_default_sink()?;
        let player = Player::connect_new(device.mixer());
        let mut this = Self {
            _device: device,
            player,
            duration: None,
        };
        this.load(path, Duration::ZERO, true)?;
        Ok(this)
    }

    pub fn load(
        &mut self,
        path: &Path,
        position: Duration,
        should_play: bool,
    ) -> Result<(), Box<dyn Error>> {
        let decoder = Decoder::try_from(File::open(path)?)?;
        let duration = decoder.total_duration();
        self.player.clear();
        self.player.append(decoder);
        self.player.try_seek(position)?;
        if should_play {
            self.player.play();
        } else {
            self.player.pause();
        }
        self.duration = duration;
        Ok(())
    }

    pub fn toggle(&self) {
        if self.player.is_paused() {
            self.player.play();
        } else {
            self.player.pause();
        }
    }

    pub fn seek_by(&self, seconds: i64) -> Result<(), Box<dyn Error>> {
        let current = self.position();
        let target = if seconds.is_negative() {
            current.saturating_sub(Duration::from_secs(seconds.unsigned_abs()))
        } else {
            current.saturating_add(Duration::from_secs(seconds.unsigned_abs()))
        };
        self.player.try_seek(target)?;
        Ok(())
    }

    pub fn adjust_volume(&self, delta: f32) {
        self.player
            .set_volume((self.player.volume() + delta).clamp(0.0, 2.0));
    }

    pub fn position(&self) -> Duration {
        self.player.get_pos()
    }

    pub fn duration(&self) -> Option<Duration> {
        self.duration
    }

    pub fn volume(&self) -> f32 {
        self.player.volume()
    }

    pub fn is_paused(&self) -> bool {
        self.player.is_paused()
    }

    pub fn is_finished(&self) -> bool {
        self.player.empty()
    }
}
