use std::{
    error::Error,
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, SyncSender, TrySendError},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use k3_app::{AudioPlayer, MonitorControl, MonitorTap};
use rodio::cpal;
use rodio::cpal::{
    FromSample, I24, Sample, SampleFormat, SizedSample, U24,
    traits::{DeviceTrait, HostTrait, StreamTrait},
};

const WRITER_QUEUE_DEPTH: usize = 64;
use crate::audio_config;

enum WriterMessage {
    Samples(Vec<f32>),
    Finish,
}

struct CaptureState {
    sender: SyncSender<WriterMessage>,
    overrun: Arc<AtomicBool>,
    captured_samples: Arc<AtomicU64>,
    stream_error: Arc<Mutex<Option<String>>>,
}

pub struct RecordingSummary {
    pub device: String,
    pub duration: Duration,
    pub warning: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RecordingTimelineAnchor {
    pub capture_frame: u64,
    pub song_position: Duration,
}

/// 按录音期间的播放位置锚点，把连续采集的人声放回歌曲时间轴。
///
/// 后录制的片段会覆盖相同歌曲位置上的旧片段，用于录音中按歌词回退重唱。
pub fn place_recording_on_timeline(
    source: &Path,
    destination: &Path,
    anchors: &[RecordingTimelineAnchor],
) -> Result<(), Box<dyn Error>> {
    if anchors.is_empty() {
        return Err("recording timeline has no initial anchor".into());
    }
    if anchors.len() == 1 && anchors[0].capture_frame == 0 && anchors[0].song_position.is_zero() {
        fs::rename(source, destination)?;
        return Ok(());
    }

    let mut reader = hound::WavReader::open(source)?;
    let spec = reader.spec();
    let channels = usize::from(spec.channels);
    let input = reader.samples::<f32>().collect::<Result<Vec<_>, _>>()?;
    drop(reader);
    let input_frames = input.len() / channels;
    let mut output = Vec::<f32>::new();

    for (index, anchor) in anchors.iter().enumerate() {
        let source_start = usize::try_from(anchor.capture_frame)
            .unwrap_or(usize::MAX)
            .min(input_frames);
        let source_end = anchors
            .get(index + 1)
            .and_then(|next| usize::try_from(next.capture_frame).ok())
            .unwrap_or(input_frames)
            .clamp(source_start, input_frames);
        let segment_frames = source_end - source_start;
        let target_start = duration_to_frames(anchor.song_position, spec.sample_rate)?;
        let target_end = target_start
            .checked_add(segment_frames)
            .ok_or("recording timeline is too long")?;
        let output_samples = target_end
            .checked_mul(channels)
            .ok_or("recording timeline is too large")?;
        output.resize(output.len().max(output_samples), 0.0);

        for frame in 0..segment_frames {
            let source_offset = (source_start + frame) * channels;
            let target_offset = (target_start + frame) * channels;
            output[target_offset..target_offset + channels]
                .copy_from_slice(&input[source_offset..source_offset + channels]);
        }
    }

    let mut writer = hound::WavWriter::create(destination, spec)?;
    for sample in output {
        writer.write_sample(sample)?;
    }
    writer.finalize()?;
    fs::remove_file(source)?;
    Ok(())
}

fn duration_to_frames(duration: Duration, sample_rate: u32) -> Result<usize, Box<dyn Error>> {
    let frames = duration
        .as_nanos()
        .checked_mul(u128::from(sample_rate))
        .ok_or("recording position is too large")?
        / 1_000_000_000;
    Ok(usize::try_from(frames)?)
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
    monitor_control: Option<MonitorControl>,
    captured_samples: Arc<AtomicU64>,
}

impl AudioRecorder {
    pub fn start(
        destination: &Path,
        monitor_player: Option<&AudioPlayer>,
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
        let captured_samples = Arc::new(AtomicU64::new(0));
        let stream_error = Arc::new(Mutex::new(None));
        let (monitor_tap, monitor_control) = monitor_player.map_or((None, None), |player| {
            let (tap, control) = player.live_monitor(
                channels,
                sample_rate,
                monitor_enabled,
                audio_config::monitor_prefill_ms(),
            );
            (Some(tap), Some(control))
        });
        let stream = match build_stream(
            &device,
            &config,
            sample_format,
            CaptureState {
                sender: sender.clone(),
                overrun: Arc::clone(&overrun),
                captured_samples: Arc::clone(&captured_samples),
                stream_error: Arc::clone(&stream_error),
            },
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
            monitor_control,
            captured_samples,
        })
    }

    pub fn device(&self) -> &str {
        &self.device
    }

    pub fn set_monitoring(&self, enabled: bool) {
        if let Some(control) = &self.monitor_control {
            control.set_enabled(enabled);
        }
    }

    pub fn captured_frames(&self) -> u64 {
        self.captured_samples.load(Ordering::Relaxed) / u64::from(self.channels)
    }

    pub fn stop(self) -> Result<RecordingSummary, Box<dyn Error>> {
        drop(self.stream);
        if let Some(control) = &self.monitor_control {
            control.close();
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
    state: CaptureState,
    monitor: Option<MonitorTap>,
) -> Result<cpal::Stream, Box<dyn Error>> {
    macro_rules! stream {
        ($sample:ty) => {
            build_typed_stream::<$sample>(device, config, state, monitor)
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
    state: CaptureState,
    mut monitor: Option<MonitorTap>,
) -> Result<cpal::Stream, Box<dyn Error>>
where
    T: Sample + SizedSample + Copy,
    f32: FromSample<T>,
{
    let CaptureState {
        sender,
        overrun,
        captured_samples,
        stream_error: error_state,
    } = state;
    Ok(device.build_input_stream(
        config,
        move |data: &[T], _| {
            let samples: Vec<f32> = data.iter().copied().map(f32::from_sample).collect();
            if let Some(monitor) = &mut monitor {
                monitor.send(&samples);
            }
            let sample_count = u64::try_from(samples.len()).unwrap_or(u64::MAX);
            match sender.try_send(WriterMessage::Samples(samples)) {
                Ok(()) => {
                    captured_samples.fetch_add(sample_count, Ordering::Relaxed);
                }
                Err(TrySendError::Full(_)) => overrun.store(true, Ordering::Relaxed),
                Err(TrySendError::Disconnected(_)) => {}
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
    use super::{RecordingTimelineAnchor, place_recording_on_timeline, select_input_config};
    use rodio::cpal::{
        SampleFormat, SupportedBufferSize, SupportedStreamConfig, SupportedStreamConfigRange,
    };
    use std::time::Duration;

    #[test]
    fn later_recording_segment_overwrites_the_revisited_song_position() {
        let sandbox = tempfile::tempdir().unwrap();
        let raw = sandbox.path().join("raw.wav");
        let aligned = sandbox.path().join("aligned.wav");
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 2,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        };
        let mut writer = hound::WavWriter::create(&raw, spec).unwrap();
        for sample in [1.0_f32, 2.0, 3.0, 4.0, 5.0, 6.0] {
            writer.write_sample(sample).unwrap();
        }
        writer.finalize().unwrap();

        place_recording_on_timeline(
            &raw,
            &aligned,
            &[
                RecordingTimelineAnchor {
                    capture_frame: 0,
                    song_position: Duration::ZERO,
                },
                RecordingTimelineAnchor {
                    capture_frame: 4,
                    song_position: Duration::from_secs(1),
                },
            ],
        )
        .unwrap();

        let mut reader = hound::WavReader::open(aligned).unwrap();
        assert_eq!(
            reader
                .samples::<f32>()
                .collect::<Result<Vec<_>, _>>()
                .unwrap(),
            vec![1.0, 2.0, 5.0, 6.0]
        );
        assert!(!raw.exists());
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
