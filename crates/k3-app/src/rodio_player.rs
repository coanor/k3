use std::{
    error::Error,
    fs::File,
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use k3_audio::PitchShiftSource;
use rodio::{
    ChannelCount, Decoder, DeviceSinkBuilder, MixerDeviceSink, Player, SampleRate, Source, cpal,
};

use crate::audio_config;

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
    /// Opens a media file and starts playback immediately.
    ///
    /// # Errors
    ///
    /// Returns an audio device, file, decode, or seek error.
    pub fn open(path: &Path, key_shift_semitones: i8) -> Result<Self, Box<dyn Error>> {
        Self::open_with_state(path, key_shift_semitones, true)
    }

    /// Opens a media file without starting playback.
    ///
    /// # Errors
    ///
    /// Returns an audio device, file, decode, or seek error.
    pub fn open_paused(path: &Path, key_shift_semitones: i8) -> Result<Self, Box<dyn Error>> {
        Self::open_with_state(path, key_shift_semitones, false)
    }

    fn open_with_state(
        path: &Path,
        key_shift_semitones: i8,
        should_play: bool,
    ) -> Result<Self, Box<dyn Error>> {
        let stream_error = Arc::new(Mutex::new(None));
        let callback_state = Arc::clone(&stream_error);
        let mut builder = DeviceSinkBuilder::from_default_device()?;
        if let Some(buffer_size) = audio_config::output_buffer_size() {
            builder = builder.with_buffer_size(buffer_size);
        }
        let mut device = builder
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
        this.load(path, Duration::ZERO, should_play, key_shift_semitones)?;
        Ok(this)
    }

    /// Replaces the current source while choosing position, state, and key shift.
    ///
    /// # Errors
    ///
    /// Returns a file, decode, or seek error.
    pub fn load(
        &mut self,
        path: &Path,
        position: Duration,
        should_play: bool,
        key_shift_semitones: i8,
    ) -> Result<(), Box<dyn Error>> {
        let decoder = Decoder::try_from(File::open(path)?)?;
        let duration = decoder.total_duration();
        self.player.clear();
        self.player
            .append(PitchShiftSource::new(decoder, key_shift_semitones));
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

    /// Moves relative to the current playback position.
    ///
    /// # Errors
    ///
    /// Returns an error when the decoder cannot seek to the target position.
    pub fn seek_by(&self, seconds: i64) -> Result<(), Box<dyn Error>> {
        let current = self.position();
        let target = if seconds.is_negative() {
            current.saturating_sub(Duration::from_secs(seconds.unsigned_abs()))
        } else {
            current.saturating_add(Duration::from_secs(seconds.unsigned_abs()))
        };
        self.seek_to(target)
    }

    /// Moves to an absolute playback position.
    ///
    /// # Errors
    ///
    /// Returns an error when the decoder cannot seek to the target position.
    pub fn seek_to(&self, position: Duration) -> Result<(), Box<dyn Error>> {
        self.player.try_seek(position)?;
        Ok(())
    }

    pub fn set_volume(&self, volume: f32) {
        self.player.set_volume(volume.clamp(0.0, 2.0));
    }

    pub fn adjust_volume(&self, delta: f32) {
        self.set_volume(self.volume() + delta);
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

    pub fn take_stream_error(&self) -> Option<String> {
        self.stream_error.lock().ok()?.take()
    }

    /// Connects a microphone monitor to the same output mixer as playback.
    #[must_use]
    pub fn live_monitor(
        &self,
        channels: u16,
        sample_rate: u32,
        enabled: bool,
        prefill_ms: usize,
    ) -> (MonitorTap, MonitorControl) {
        let (tap, source) = live_monitor(channels, sample_rate, enabled, prefill_ms);
        let control = tap.control();
        self.device.mixer().add(source);
        (tap, control)
    }
}

const MONITOR_BUFFER_MS: usize = 250;
const MONITOR_GAIN: f32 = 4.0;

/// Producer handle for microphone samples mixed into the active playback device.
pub struct MonitorTap {
    producer: rtrb::Producer<f32>,
    enabled: Arc<AtomicBool>,
    closed: Arc<AtomicBool>,
}

impl MonitorTap {
    pub fn send(&mut self, samples: &[f32]) {
        if self.enabled.load(Ordering::Relaxed) {
            let _ = self.producer.push_partial_slice(samples);
        }
    }

    fn control(&self) -> MonitorControl {
        MonitorControl {
            enabled: Arc::clone(&self.enabled),
            closed: Arc::clone(&self.closed),
        }
    }
}

/// Cloneable control handle for a live microphone monitor.
#[derive(Clone)]
pub struct MonitorControl {
    enabled: Arc<AtomicBool>,
    closed: Arc<AtomicBool>,
}

impl MonitorControl {
    pub fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::Relaxed);
    }

    pub fn close(&self) {
        self.enabled.store(false, Ordering::Relaxed);
        self.closed.store(true, Ordering::Relaxed);
    }
}

struct LiveMonitorSource {
    consumer: rtrb::Consumer<f32>,
    enabled: Arc<AtomicBool>,
    closed: Arc<AtomicBool>,
    channels: ChannelCount,
    sample_rate: SampleRate,
    prefill_samples: usize,
    started: bool,
}

impl Iterator for LiveMonitorSource {
    type Item = f32;

    fn next(&mut self) -> Option<Self::Item> {
        if !self.enabled.load(Ordering::Relaxed) {
            while self.consumer.pop().is_ok() {}
            self.started = false;
            return (!self.closed.load(Ordering::Relaxed)).then_some(0.0);
        }

        if self.closed.load(Ordering::Relaxed) && self.consumer.is_empty() {
            return None;
        }
        if !self.started {
            if self.consumer.slots() < self.prefill_samples {
                return Some(0.0);
            }
            self.started = true;
        }

        match self.consumer.pop() {
            Ok(sample) => Some((sample * MONITOR_GAIN).clamp(-1.0, 1.0)),
            Err(rtrb::PopError::Empty) => {
                self.started = false;
                Some(0.0)
            }
        }
    }
}

impl Source for LiveMonitorSource {
    fn current_span_len(&self) -> Option<usize> {
        None
    }

    fn channels(&self) -> ChannelCount {
        self.channels
    }

    fn sample_rate(&self) -> SampleRate {
        self.sample_rate
    }

    fn total_duration(&self) -> Option<Duration> {
        None
    }
}

fn live_monitor(
    channels: u16,
    sample_rate: u32,
    enabled: bool,
    prefill_ms: usize,
) -> (MonitorTap, LiveMonitorSource) {
    let samples_per_millisecond = sample_rate as usize * channels as usize / 1_000;
    let capacity = (samples_per_millisecond * MONITOR_BUFFER_MS).max(1);
    let prefill_samples = (samples_per_millisecond * prefill_ms).max(1);
    let (producer, consumer) = rtrb::RingBuffer::new(capacity);
    let enabled = Arc::new(AtomicBool::new(enabled));
    let closed = Arc::new(AtomicBool::new(false));
    let tap = MonitorTap {
        producer,
        enabled: Arc::clone(&enabled),
        closed: Arc::clone(&closed),
    };
    let source = LiveMonitorSource {
        consumer,
        enabled,
        closed,
        channels: ChannelCount::new(channels).expect("CPAL channel count is non-zero"),
        sample_rate: SampleRate::new(sample_rate).expect("CPAL sample rate is non-zero"),
        prefill_samples,
        started: false,
    };
    (tap, source)
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
    use super::{live_monitor, record_stream_error};
    use rodio::cpal::StreamError;
    use std::{sync::Mutex, sync::atomic::Ordering};

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

    #[test]
    fn live_monitor_can_be_toggled_and_applies_monitor_gain() {
        let (mut tap, mut source) = live_monitor(1, 1_000, false, 100);
        tap.send(&[0.25]);
        assert_eq!(source.next(), Some(0.0));

        tap.enabled.store(true, Ordering::Relaxed);
        let mut samples = vec![0.0; 100];
        samples[0] = 0.125;
        samples[1] = -0.25;
        tap.send(&samples);
        assert_eq!(source.next(), Some(0.5));
        assert_eq!(source.next(), Some(-1.0));

        tap.enabled.store(false, Ordering::Relaxed);
        assert_eq!(source.next(), Some(0.0));
    }

    #[test]
    fn live_monitor_ends_when_recording_closes() {
        let (tap, mut source) = live_monitor(1, 44_100, true, 100);
        tap.control().close();
        assert_eq!(source.next(), None);
    }

    #[test]
    fn live_monitor_prefills_before_playing_input() {
        let (mut tap, mut source) = live_monitor(1, 1_000, true, 100);
        tap.send(&[0.125]);
        assert_eq!(source.next(), Some(0.0));

        tap.send(&vec![0.125; 99]);
        assert_eq!(source.next(), Some(0.5));
    }
}
