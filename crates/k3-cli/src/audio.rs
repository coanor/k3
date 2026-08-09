use std::{
    error::Error,
    fs::File,
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};

use rodio::{Decoder, DeviceSinkBuilder, MixerDeviceSink, Player, Source, cpal};

/// Owns the operating-system audio stream and one controllable decoded track.
///
/// This adapter deliberately knows nothing about the TUI or project format.
pub struct AudioPlayer {
    device: MixerDeviceSink,
    player: Player,
    duration: Option<Duration>,
    stream_error: Arc<Mutex<Option<String>>>,
}

impl AudioPlayer {
    pub fn open(path: &Path) -> Result<Self, Box<dyn Error>> {
        let stream_error = Arc::new(Mutex::new(None));
        let callback_state = Arc::clone(&stream_error);
        let mut device = DeviceSinkBuilder::from_default_device()?
            .with_error_callback(move |error| record_stream_error(&callback_state, &error))
            .open_sink_or_fallback()?;
        device.log_on_drop(false);
        let player = Player::connect_new(device.mixer());
        let mut this = Self {
            device,
            player,
            duration: None,
            stream_error,
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

    pub fn play_prepared(&self) {
        self.player.play();
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

    pub fn mixer(&self) -> rodio::mixer::Mixer {
        self.device.mixer().clone()
    }

    pub fn take_stream_error(&self) -> Option<String> {
        self.stream_error.lock().ok()?.take()
    }
}

fn record_stream_error(state: &Mutex<Option<String>>, error: &cpal::StreamError) {
    // WSLg can report a transient output underrun when its microphone stream starts.
    // CPAL keeps the stream alive, so treating this as fatal only corrupts the TUI.
    if *error == cpal::StreamError::BufferUnderrun {
        return;
    }
    if let Ok(mut state) = state.lock() {
        *state = Some(error.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::record_stream_error;
    use rodio::cpal::StreamError;
    use std::sync::Mutex;

    #[test]
    fn ignores_recoverable_buffer_underrun() {
        let state = Mutex::new(None);
        record_stream_error(&state, &StreamError::BufferUnderrun);
        assert_eq!(*state.lock().unwrap(), None);
    }

    #[test]
    fn retains_fatal_stream_error() {
        let state = Mutex::new(None);
        record_stream_error(&state, &StreamError::DeviceNotAvailable);
        assert!(
            state
                .lock()
                .unwrap()
                .as_deref()
                .unwrap()
                .contains("no longer available")
        );
    }
}
