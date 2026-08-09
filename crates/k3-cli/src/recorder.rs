use std::{
    error::Error,
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, SyncSender, TrySendError},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use rodio::cpal::{
    FromSample, I24, Sample, SampleFormat, SizedSample, U24,
    traits::{DeviceTrait, HostTrait, StreamTrait},
};
use rodio::{ChannelCount, SampleRate, Source, cpal, mixer::Mixer};

const WRITER_QUEUE_DEPTH: usize = 64;
const MONITOR_BUFFER_MS: usize = 250;
const MONITOR_GAIN: f32 = 4.0;

use crate::audio_config;

enum WriterMessage {
    Samples(Vec<f32>),
    Finish,
}

pub struct RecordingSummary {
    pub device: String,
    pub duration: Duration,
    pub warning: Option<String>,
}

/// Captures the default input device while a dedicated thread writes float WAV.
///
/// The device callback never performs filesystem I/O.
pub struct AudioRecorder {
    stream: cpal::Stream,
    sender: SyncSender<WriterMessage>,
    writer: JoinHandle<Result<u64, String>>,
    overrun: Arc<AtomicBool>,
    stream_error: Arc<Mutex<Option<String>>>,
    device: String,
    sample_rate: u32,
    channels: u16,
    destination: PathBuf,
    monitor_enabled: Option<Arc<AtomicBool>>,
    monitor_closed: Option<Arc<AtomicBool>>,
}

impl AudioRecorder {
    pub fn start(
        destination: &Path,
        monitor_mixer: Option<Mixer>,
        monitor_enabled: bool,
    ) -> Result<Self, Box<dyn Error>> {
        let host = cpal::default_host();
        let device = host
            .default_input_device()
            .ok_or("no default audio input device is available")?;
        let device_name = device.description()?.to_string();
        let default_config = device.default_input_config()?;
        let supported_ranges: Vec<cpal::SupportedStreamConfigRange> = device
            .supported_input_configs()
            .map(Iterator::collect)
            .unwrap_or_default();
        let supported = select_input_config(default_config, &supported_ranges);
        let sample_format = supported.sample_format();
        let supported_buffer_size = *supported.buffer_size();
        let mut config: cpal::StreamConfig = supported.into();
        config.buffer_size = audio_config::input_buffer_size(&supported_buffer_size);
        let channels = config.channels;
        let sample_rate = config.sample_rate;

        let spec = hound::WavSpec {
            channels,
            sample_rate,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        };
        let writer = hound::WavWriter::create(destination, spec)?;
        let (sender, receiver) = mpsc::sync_channel(WRITER_QUEUE_DEPTH);
        let writer_thread = thread::spawn(move || {
            let mut writer = writer;
            let mut samples_written = 0_u64;
            while let Ok(message) = receiver.recv() {
                match message {
                    WriterMessage::Samples(samples) => {
                        for sample in samples {
                            writer
                                .write_sample(sample)
                                .map_err(|error| error.to_string())?;
                            samples_written += 1;
                        }
                    }
                    WriterMessage::Finish => break,
                }
            }
            writer.finalize().map_err(|error| error.to_string())?;
            Ok(samples_written)
        });

        let overrun = Arc::new(AtomicBool::new(false));
        let stream_error = Arc::new(Mutex::new(None));
        let (monitor_tap, monitor_enabled, monitor_closed) =
            monitor_mixer.map_or((None, None, None), |mixer| {
                let (tap, source) = live_monitor(
                    channels,
                    sample_rate,
                    monitor_enabled,
                    audio_config::monitor_prefill_ms(),
                );
                let enabled = Arc::clone(&tap.enabled);
                let closed = Arc::clone(&tap.closed);
                mixer.add(source);
                (Some(tap), Some(enabled), Some(closed))
            });
        let stream = match build_stream(
            &device,
            &config,
            sample_format,
            sender.clone(),
            Arc::clone(&overrun),
            &stream_error,
            monitor_tap,
        ) {
            Ok(stream) => stream,
            Err(error) => {
                let _ = sender.send(WriterMessage::Finish);
                let _ = writer_thread.join();
                let _ = fs::remove_file(destination);
                return Err(error);
            }
        };
        if let Err(error) = stream.play() {
            let _ = sender.send(WriterMessage::Finish);
            let _ = writer_thread.join();
            let _ = fs::remove_file(destination);
            return Err(error.into());
        }

        Ok(Self {
            stream,
            sender,
            writer: writer_thread,
            overrun,
            stream_error,
            device: device_name,
            sample_rate,
            channels,
            destination: destination.to_path_buf(),
            monitor_enabled,
            monitor_closed,
        })
    }

    pub fn device(&self) -> &str {
        &self.device
    }

    pub fn set_monitoring(&self, enabled: bool) {
        if let Some(state) = &self.monitor_enabled {
            state.store(enabled, Ordering::Relaxed);
        }
    }

    pub fn stop(self) -> Result<RecordingSummary, Box<dyn Error>> {
        drop(self.stream);
        if let Some(enabled) = &self.monitor_enabled {
            enabled.store(false, Ordering::Relaxed);
        }
        if let Some(closed) = &self.monitor_closed {
            closed.store(true, Ordering::Relaxed);
        }
        self.sender.send(WriterMessage::Finish)?;
        drop(self.sender);
        let samples = self
            .writer
            .join()
            .map_err(|_| "recording writer thread panicked")?
            .map_err(|error| format!("cannot finalize recording: {error}"))?;
        if samples == 0 {
            let _ = fs::remove_file(&self.destination);
            return Err("microphone produced no audio samples".into());
        }

        let frames = samples / u64::from(self.channels);
        let sample_rate = u64::from(self.sample_rate);
        let duration = Duration::from_secs(frames / sample_rate)
            + Duration::from_nanos((frames % sample_rate) * 1_000_000_000 / sample_rate);
        let stream_error = self
            .stream_error
            .lock()
            .map_err(|_| "recording error state is poisoned")?
            .take();
        let warning = match (
            self.overrun.load(Ordering::Relaxed),
            stream_error.as_deref(),
        ) {
            (true, Some(error)) => Some(format!("audio buffers were dropped; {error}")),
            (true, None) => {
                Some("audio buffers were dropped because disk writing fell behind".into())
            }
            (false, Some(error)) => Some(error.to_owned()),
            (false, None) => None,
        };
        Ok(RecordingSummary {
            device: self.device,
            duration,
            warning,
        })
    }
}

fn build_stream(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    format: SampleFormat,
    sender: SyncSender<WriterMessage>,
    overrun: Arc<AtomicBool>,
    stream_error: &Arc<Mutex<Option<String>>>,
    monitor: Option<MonitorTap>,
) -> Result<cpal::Stream, Box<dyn Error>> {
    macro_rules! stream {
        ($sample:ty) => {
            build_typed_stream::<$sample>(device, config, sender, overrun, stream_error, monitor)
        };
    }
    match format {
        SampleFormat::I8 => stream!(i8),
        SampleFormat::I16 => stream!(i16),
        SampleFormat::I24 => stream!(I24),
        SampleFormat::I32 => stream!(i32),
        SampleFormat::I64 => stream!(i64),
        SampleFormat::U8 => stream!(u8),
        SampleFormat::U16 => stream!(u16),
        SampleFormat::U24 => stream!(U24),
        SampleFormat::U32 => stream!(u32),
        SampleFormat::U64 => stream!(u64),
        SampleFormat::F32 => stream!(f32),
        SampleFormat::F64 => stream!(f64),
        unsupported => Err(format!("unsupported microphone sample format: {unsupported}").into()),
    }
}

fn build_typed_stream<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    sender: SyncSender<WriterMessage>,
    overrun: Arc<AtomicBool>,
    stream_error: &Arc<Mutex<Option<String>>>,
    mut monitor: Option<MonitorTap>,
) -> Result<cpal::Stream, Box<dyn Error>>
where
    T: Sample + SizedSample + Copy,
    f32: FromSample<T>,
{
    let error_state = Arc::clone(stream_error);
    Ok(device.build_input_stream(
        config,
        move |data: &[T], _| {
            let samples: Vec<f32> = data.iter().copied().map(f32::from_sample).collect();
            if let Some(monitor) = &mut monitor {
                monitor.send(&samples);
            }
            if let Err(error) = sender.try_send(WriterMessage::Samples(samples)) {
                match error {
                    TrySendError::Full(_) => overrun.store(true, Ordering::Relaxed),
                    TrySendError::Disconnected(_) => {}
                }
            }
        },
        move |error| {
            if let Ok(mut state) = error_state.lock() {
                *state = Some(error.to_string());
            }
        },
        None,
    )?)
}

struct MonitorTap {
    producer: rtrb::Producer<f32>,
    enabled: Arc<AtomicBool>,
    closed: Arc<AtomicBool>,
}

impl MonitorTap {
    fn send(&mut self, samples: &[f32]) {
        if self.enabled.load(Ordering::Relaxed) {
            let _ = self.producer.push_partial_slice(samples);
        }
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

fn select_input_config(
    default: cpal::SupportedStreamConfig,
    supported: &[cpal::SupportedStreamConfigRange],
) -> cpal::SupportedStreamConfig {
    for sample_rate in [44_100, 48_000] {
        if let Some(config) = supported
            .iter()
            .filter_map(|range| range.try_with_sample_rate(sample_rate))
            .min_by_key(|config| {
                let channel_rank = match config.channels() {
                    1 => 0,
                    2 => 1,
                    channels => channels,
                };
                let format_rank = match config.sample_format() {
                    SampleFormat::I16 => 0,
                    SampleFormat::F32 => 1,
                    SampleFormat::I32 => 2,
                    _ => 3,
                };
                (channel_rank, format_rank)
            })
        {
            return config;
        }
    }
    default
}

#[cfg(test)]
mod tests {
    use super::{live_monitor, select_input_config};
    use rodio::cpal::{
        SampleFormat, SupportedBufferSize, SupportedStreamConfig, SupportedStreamConfigRange,
    };
    use std::sync::atomic::Ordering;

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
        tap.closed.store(true, Ordering::Relaxed);
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

    #[test]
    fn prefers_native_voice_capture_over_low_quality_default() {
        let default =
            SupportedStreamConfig::new(2, 8_000, SupportedBufferSize::Unknown, SampleFormat::F32);
        let supported = [
            SupportedStreamConfigRange::new(
                2,
                44_100,
                44_100,
                SupportedBufferSize::Unknown,
                SampleFormat::F32,
            ),
            SupportedStreamConfigRange::new(
                1,
                44_100,
                44_100,
                SupportedBufferSize::Unknown,
                SampleFormat::I16,
            ),
        ];

        let selected = select_input_config(default, &supported);

        assert_eq!(selected.channels(), 1);
        assert_eq!(selected.sample_rate(), 44_100);
        assert_eq!(selected.sample_format(), SampleFormat::I16);
    }
}
