use std::time::Duration;

use rodio::{
    ChannelCount, SampleRate, Source,
    source::{SeekError, Speed, UniformSourceIterator},
};
use rodio_wsola::Wsola;

type ShiftedInput<S> = UniformSourceIterator<Speed<Wsola<S>>>;

pub struct Shifted<S>
where
    S: Source,
{
    input: ShiftedInput<S>,
    duration: Option<Duration>,
    remaining_samples: Option<usize>,
}

/// 保持时长不变的变调音源。
///
/// WSOLA 先在不改变音高的前提下补偿时长，随后通过重采样改变音高。
/// 播放与离线混音共用这个只暴露半音数的小接口。
pub enum PitchShiftSource<S>
where
    S: Source,
{
    Unchanged(S),
    Shifted(Box<Shifted<S>>),
}

impl<S> PitchShiftSource<S>
where
    S: Source,
{
    #[must_use]
    pub fn new(input: S, semitones: i8) -> Self {
        if semitones == 0 {
            return Self::Unchanged(input);
        }

        let channels = input.channels();
        let sample_rate = input.sample_rate();
        let duration = input.total_duration();
        let pitch_factor = 2.0_f32.powf(f32::from(semitones) / 12.0);
        let duration_compensation = pitch_factor.recip();
        let stretched = Wsola::new(input, duration_compensation).speed(pitch_factor);
        Self::Shifted(Box::new(Shifted {
            input: UniformSourceIterator::new(stretched, channels, sample_rate),
            duration,
            remaining_samples: duration
                .and_then(|duration| sample_count(duration, sample_rate, channels)),
        }))
    }
}

impl<S> Iterator for PitchShiftSource<S>
where
    S: Source,
{
    type Item = f32;

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::Unchanged(input) => input.next(),
            Self::Shifted(shifted) => match &mut shifted.remaining_samples {
                Some(0) => None,
                Some(remaining) => {
                    *remaining -= 1;
                    Some(shifted.input.next().unwrap_or(0.0))
                }
                None => shifted.input.next(),
            },
        }
    }
}

impl<S> Source for PitchShiftSource<S>
where
    S: Source,
{
    fn current_span_len(&self) -> Option<usize> {
        match self {
            Self::Unchanged(input) => input.current_span_len(),
            Self::Shifted(shifted) => shifted.remaining_samples,
        }
    }

    fn channels(&self) -> ChannelCount {
        match self {
            Self::Unchanged(input) => input.channels(),
            Self::Shifted(shifted) => shifted.input.channels(),
        }
    }

    fn sample_rate(&self) -> SampleRate {
        match self {
            Self::Unchanged(input) => input.sample_rate(),
            Self::Shifted(shifted) => shifted.input.sample_rate(),
        }
    }

    fn total_duration(&self) -> Option<Duration> {
        match self {
            Self::Unchanged(input) => input.total_duration(),
            Self::Shifted(shifted) => shifted.duration,
        }
    }

    fn try_seek(&mut self, position: Duration) -> Result<(), SeekError> {
        match self {
            Self::Unchanged(input) => input.try_seek(position),
            Self::Shifted(shifted) => {
                shifted.input.try_seek(position)?;
                shifted.remaining_samples = shifted.duration.and_then(|duration| {
                    sample_count(
                        duration.saturating_sub(position),
                        shifted.input.sample_rate(),
                        shifted.input.channels(),
                    )
                });
                Ok(())
            }
        }
    }
}

fn sample_count(
    duration: Duration,
    sample_rate: SampleRate,
    channels: ChannelCount,
) -> Option<usize> {
    let frames = duration
        .as_nanos()
        .checked_mul(u128::from(sample_rate.get()))?
        / 1_000_000_000;
    usize::try_from(frames.checked_mul(u128::from(channels.get()))?).ok()
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
    fn shifted_stereo_keeps_duration_pitch_and_channel_alignment() {
        let frames = 16_000_u16;
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
        let shifted = PitchShiftSource::new(source, -4).collect::<Vec<_>>();

        let frame_delta = shifted.len().abs_diff(samples.len()) / 2;
        assert!(frame_delta <= 2, "duration differs by {frame_delta} frames");
        assert!(
            shifted
                .chunks_exact(2)
                .all(|frame| frame[1].abs() < 0.000_1)
        );
        let left = shifted
            .chunks_exact(2)
            .map(|frame| frame[0])
            .collect::<Vec<_>>();
        let frequency = estimate_frequency(&left[4_000..12_000], 8_000.0);
        assert!((frequency - 174.6).abs() < 8.0, "frequency was {frequency}");
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
