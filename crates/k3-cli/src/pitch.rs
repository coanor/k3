use std::time::Duration;

use pitch_shift::{Shifter, TOTAL_F32};
use rodio::{ChannelCount, SampleRate, Source, source::SeekError};

const BLOCK_FRAMES: usize = 128;
const ALGORITHM_LATENCY_FRAMES: usize = 1024 - BLOCK_FRAMES;

type ShifterState = Box<[f32; TOTAL_F32]>;

/// A duration-preserving, latency-compensated pitch shifter for interleaved audio.
///
/// Channel splitting, fixed-size processing and phase-vocoder latency stay private so
/// playback and offline rendering share the same small semitone-based interface.
pub struct PitchShiftSource<S> {
    input: S,
    semitones: i8,
    channels: usize,
    sample_rate: u32,
    shifters: Vec<Shifter<ShifterState>>,
    input_block: Vec<Vec<f32>>,
    shifted_block: Vec<Vec<f32>>,
    output_block: Vec<f32>,
    output_index: usize,
    skipped_frames: usize,
    input_frames: usize,
    emitted_frames: usize,
    input_ended: bool,
}

impl<S> PitchShiftSource<S>
where
    S: Source,
{
    #[must_use]
    pub fn new(input: S, semitones: i8) -> Self {
        let channels = usize::from(input.channels().get());
        let sample_rate = input.sample_rate().get();
        let mut this = Self {
            input,
            semitones,
            channels,
            sample_rate,
            shifters: Vec::new(),
            input_block: Vec::new(),
            shifted_block: Vec::new(),
            output_block: Vec::new(),
            output_index: 0,
            skipped_frames: 0,
            input_frames: 0,
            emitted_frames: 0,
            input_ended: false,
        };
        this.reset_processing();
        this
    }

    fn reset_processing(&mut self) {
        self.shifters = (0..self.channels).map(|_| new_shifter()).collect();
        self.input_block = vec![vec![0.0; BLOCK_FRAMES]; self.channels];
        self.shifted_block = vec![vec![0.0; BLOCK_FRAMES]; self.channels];
        self.output_block.clear();
        self.output_block.reserve(BLOCK_FRAMES * self.channels);
        self.output_index = 0;
        self.skipped_frames = 0;
        self.input_frames = 0;
        self.emitted_frames = 0;
        self.input_ended = false;
    }

    fn prepare_output(&mut self) -> bool {
        self.output_block.clear();
        self.output_index = 0;

        while self.output_block.is_empty() {
            if self.input_ended && self.emitted_frames >= self.input_frames {
                return false;
            }

            for channel in &mut self.input_block {
                channel.fill(0.0);
            }
            let mut actual_frames = 0;
            if !self.input_ended {
                for frame in 0..BLOCK_FRAMES {
                    let mut complete = true;
                    for channel in 0..self.channels {
                        if let Some(sample) = self.input.next() {
                            self.input_block[channel][frame] = sample;
                        } else {
                            complete = false;
                            self.input_ended = true;
                            break;
                        }
                    }
                    if !complete {
                        break;
                    }
                    actual_frames += 1;
                }
                self.input_frames += actual_frames;
            }

            if actual_frames == 0 && !self.input_ended {
                continue;
            }

            for channel in 0..self.channels {
                let shifted = self.shifters[channel].shift(
                    &self.input_block[channel],
                    f32::from(self.semitones),
                    BLOCK_FRAMES,
                    sample_rate_as_f32(self.sample_rate),
                );
                self.shifted_block[channel].copy_from_slice(shifted);
            }
            let skip = (ALGORITHM_LATENCY_FRAMES - self.skipped_frames).min(BLOCK_FRAMES);
            self.skipped_frames += skip;
            let available = BLOCK_FRAMES - skip;
            let remaining = self.input_frames.saturating_sub(self.emitted_frames);
            let frames_to_emit = available.min(remaining);
            self.output_block.reserve(frames_to_emit * self.channels);
            for frame in skip..skip + frames_to_emit {
                for channel in &self.shifted_block {
                    self.output_block.push(channel[frame]);
                }
            }
            self.emitted_frames += frames_to_emit;
        }
        true
    }
}

impl<S> Iterator for PitchShiftSource<S>
where
    S: Source,
{
    type Item = f32;

    fn next(&mut self) -> Option<Self::Item> {
        if self.semitones == 0 {
            return self.input.next();
        }
        if self.output_index >= self.output_block.len() && !self.prepare_output() {
            return None;
        }
        let sample = self.output_block[self.output_index];
        self.output_index += 1;
        Some(sample)
    }
}

impl<S> Source for PitchShiftSource<S>
where
    S: Source,
{
    fn current_span_len(&self) -> Option<usize> {
        None
    }

    fn channels(&self) -> ChannelCount {
        self.input.channels()
    }

    fn sample_rate(&self) -> SampleRate {
        self.input.sample_rate()
    }

    fn total_duration(&self) -> Option<Duration> {
        self.input.total_duration()
    }

    fn try_seek(&mut self, position: Duration) -> Result<(), SeekError> {
        self.input.try_seek(position)?;
        self.reset_processing();
        Ok(())
    }
}

fn new_shifter() -> Shifter<ShifterState> {
    let state: ShifterState = vec![0.0; TOTAL_F32]
        .into_boxed_slice()
        .try_into()
        .expect("pitch shifter state has the declared size");
    Shifter::new(state)
}

#[allow(clippy::cast_precision_loss)]
fn sample_rate_as_f32(sample_rate: u32) -> f32 {
    // Audio sample rates are several orders of magnitude below f32's exact integer range.
    sample_rate as f32
}

#[cfg(test)]
mod tests {
    use super::PitchShiftSource;
    use rodio::{ChannelCount, SampleRate, buffer::SamplesBuffer};

    #[test]
    fn zero_shift_is_bit_exact() {
        let input = vec![0.1, -0.2, 0.3, -0.4];
        let source = SamplesBuffer::new(
            ChannelCount::new(2).unwrap(),
            SampleRate::new(44_100).unwrap(),
            input.clone(),
        );
        assert_eq!(PitchShiftSource::new(source, 0).collect::<Vec<_>>(), input);
    }

    #[test]
    fn shifted_stereo_keeps_duration_and_channel_alignment() {
        let frames = 4_096_u16;
        let samples = (0..frames)
            .flat_map(|frame| {
                let phase = f32::from(frame) * std::f32::consts::TAU * 220.0 / 8_000.0;
                [phase.sin(), 0.0]
            })
            .collect::<Vec<_>>();
        let source = SamplesBuffer::new(
            ChannelCount::new(2).unwrap(),
            SampleRate::new(8_000).unwrap(),
            samples.clone(),
        );
        let shifted = PitchShiftSource::new(source, 6).collect::<Vec<_>>();

        assert_eq!(shifted.len(), samples.len());
        assert!(
            shifted
                .chunks_exact(2)
                .all(|frame| frame[1].abs() < 0.000_1)
        );
        let frequency = estimate_frequency(
            &shifted
                .chunks_exact(2)
                .map(|frame| frame[0])
                .collect::<Vec<_>>()[512..3_584],
            8_000.0,
        );
        assert!(
            (frequency - 311.1).abs() < 12.0,
            "frequency was {frequency}"
        );
    }

    fn estimate_frequency(samples: &[f32], sample_rate: f32) -> f32 {
        let crossings = samples
            .windows(2)
            .filter(|pair| pair[0] <= 0.0 && pair[1] > 0.0)
            .count();
        let crossings = u16::try_from(crossings).expect("test window is small");
        let sample_count = u16::try_from(samples.len()).expect("test window is small");
        f32::from(crossings) * sample_rate / f32::from(sample_count)
    }
}
