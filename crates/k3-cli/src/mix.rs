use std::{error::Error, fs::File, path::Path};

use rodio::{ChannelCount, Decoder, SampleRate, source::UniformSourceIterator};

const VOICE_GAIN: f32 = 4.0;
const BACKING_GAIN: f32 = 0.65;

pub fn render_take_mix(
    backing_path: &Path,
    dry_path: &Path,
    destination: &Path,
    latency_compensation_ms: i32,
) -> Result<(), Box<dyn Error>> {
    let mut dry_reader = hound::WavReader::open(dry_path)?;
    let dry_spec = dry_reader.spec();
    if dry_spec.sample_format != hound::SampleFormat::Float || dry_spec.bits_per_sample != 32 {
        return Err("dry take must be a 32-bit float WAV".into());
    }
    let dry_samples: Vec<f32> = dry_reader.samples::<f32>().collect::<Result<_, _>>()?;
    let dry_channels = usize::from(dry_spec.channels);
    let frames = dry_samples.len() / dry_channels;
    let voice_offset_frames =
        i64::from(latency_compensation_ms) * i64::from(dry_spec.sample_rate) / 1_000;

    let backing = Decoder::try_from(File::open(backing_path)?)?;
    let mut backing = UniformSourceIterator::new(
        backing,
        ChannelCount::new(2).expect("mix channel count is non-zero"),
        SampleRate::new(dry_spec.sample_rate).expect("recording sample rate is non-zero"),
    );
    let spec = hound::WavSpec {
        channels: 2,
        sample_rate: dry_spec.sample_rate,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let mut writer = hound::WavWriter::create(destination, spec)?;
    for frame in 0..frames {
        for channel in 0..2 {
            let dry_channel = channel.min(dry_channels - 1);
            let dry_frame = i64::try_from(frame)? + voice_offset_frames;
            let voice = usize::try_from(dry_frame)
                .ok()
                .filter(|dry_frame| *dry_frame < frames)
                .map_or(0.0, |dry_frame| {
                    dry_samples[dry_frame * dry_channels + dry_channel]
                });
            let backing = backing.next().unwrap_or(0.0);
            writer.write_sample((voice * VOICE_GAIN + backing * BACKING_GAIN).clamp(-1.0, 1.0))?;
        }
    }
    writer.finalize()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::render_take_mix;

    #[test]
    fn renders_stereo_preview_without_changing_dry_take() {
        let sandbox = tempfile::tempdir().unwrap();
        let dry = sandbox.path().join("dry.wav");
        let backing = sandbox.path().join("backing.wav");
        let mix = sandbox.path().join("mix.wav");
        write_wav(&dry, 1, &[0.1; 4]);
        write_wav(&backing, 2, &[0.2; 8]);

        render_take_mix(&backing, &dry, &mix, 0).unwrap();

        let mut reader = hound::WavReader::open(mix).unwrap();
        assert_eq!(reader.spec().channels, 2);
        let samples: Vec<f32> = reader.samples::<f32>().map(Result::unwrap).collect();
        assert_eq!(samples.len(), 8);
        assert!(samples.iter().all(|sample| (*sample - 0.53).abs() < 0.001));
        assert_eq!(hound::WavReader::open(dry).unwrap().duration(), 4);
    }

    #[test]
    fn advances_a_delayed_voice_by_the_configured_latency() {
        let sandbox = tempfile::tempdir().unwrap();
        let dry = sandbox.path().join("dry.wav");
        let backing = sandbox.path().join("backing.wav");
        let mix = sandbox.path().join("mix.wav");
        write_wav_at_rate(&dry, 1, 1_000, &[0.0, 0.0, 0.1, 0.0]);
        write_wav_at_rate(&backing, 2, 1_000, &[0.0; 8]);

        render_take_mix(&backing, &dry, &mix, 2).unwrap();

        let mut reader = hound::WavReader::open(mix).unwrap();
        let samples: Vec<f32> = reader.samples::<f32>().map(Result::unwrap).collect();
        assert!((samples[0] - 0.4).abs() < 0.001);
        assert!((samples[1] - 0.4).abs() < 0.001);
    }

    fn write_wav(path: &std::path::Path, channels: u16, samples: &[f32]) {
        write_wav_at_rate(path, channels, 44_100, samples);
    }

    fn write_wav_at_rate(path: &std::path::Path, channels: u16, sample_rate: u32, samples: &[f32]) {
        let spec = hound::WavSpec {
            channels,
            sample_rate,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        };
        let mut writer = hound::WavWriter::create(path, spec).unwrap();
        for sample in samples {
            writer.write_sample(*sample).unwrap();
        }
        writer.finalize().unwrap();
    }
}
