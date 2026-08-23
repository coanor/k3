use std::{
    error::Error,
    fs::{self, File},
    path::{Path, PathBuf},
};

use k3_core::{Project, ProjectPath, SeparationState, VocalEffectPreset};
use rodio::{ChannelCount, Decoder, SampleRate, source::UniformSourceIterator};

use crate::{effects::VocalEffect, pitch::PitchShiftSource};

const VOICE_GAIN: f32 = 4.0;
const BACKING_GAIN: f32 = 0.65;

pub struct RenderedTake {
    pub path: PathBuf,
    pub relative_path: ProjectPath,
}

/// Rebuilds one preview from its immutable dry recording and project accompaniment.
pub fn render_take_preview(
    project: &Project,
    take_id: &str,
    preset: VocalEffectPreset,
) -> Result<RenderedTake, Box<dyn Error>> {
    let take = project
        .take(take_id)
        .ok_or_else(|| format!("take is not part of this project: {take_id}"))?;
    let SeparationState::Ready(separation) = project.separation() else {
        return Err("project accompaniment is not ready".into());
    };
    let relative_path = take
        .mix_audio()
        .cloned()
        .map_or_else(|| ProjectPath::new(format!("takes/{take_id}-mix.wav")), Ok)?;
    let destination = project.root().join(Path::new(relative_path.as_str()));
    let temporary = project
        .root()
        .join(format!("takes/.{take_id}-effect.partial"));
    if temporary.exists() {
        return Err(format!(
            "effect render is already in progress: {}",
            temporary.display()
        )
        .into());
    }
    let render = render_take_mix(
        &project
            .root()
            .join(Path::new(separation.accompaniment.as_str())),
        &project.root().join(Path::new(take.dry_audio().as_str())),
        &temporary,
        project.latency_compensation_ms(),
        project.key_shift_semitones(),
        preset,
    );
    if let Err(error) = render {
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }
    replace_rendered_file(&temporary, &destination)?;
    Ok(RenderedTake {
        path: destination,
        relative_path,
    })
}

pub fn render_take_mix(
    backing_path: &Path,
    dry_path: &Path,
    destination: &Path,
    latency_compensation_ms: i32,
    key_shift_semitones: i8,
    effect_preset: VocalEffectPreset,
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
    let backing = UniformSourceIterator::new(
        backing,
        ChannelCount::new(2).expect("mix channel count is non-zero"),
        SampleRate::new(dry_spec.sample_rate).expect("recording sample rate is non-zero"),
    );
    let mut backing = PitchShiftSource::new(backing, key_shift_semitones);
    let spec = hound::WavSpec {
        channels: 2,
        sample_rate: dry_spec.sample_rate,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let mut effect = VocalEffect::new(effect_preset, dry_spec.sample_rate);
    let output_frames = frames.saturating_add(effect.tail_frames());
    let mut writer = hound::WavWriter::create(destination, spec)?;
    for frame in 0..output_frames {
        let dry_frame = i64::try_from(frame)? + voice_offset_frames;
        let left = dry_sample(&dry_samples, dry_channels, frames, dry_frame, 0) * VOICE_GAIN;
        let right = dry_sample(&dry_samples, dry_channels, frames, dry_frame, 1) * VOICE_GAIN;
        let (left, right) = effect.process(left, right);
        let backing_left = backing.next().unwrap_or(0.0);
        let backing_right = backing.next().unwrap_or(0.0);
        writer.write_sample((left + backing_left * BACKING_GAIN).clamp(-1.0, 1.0))?;
        writer.write_sample((right + backing_right * BACKING_GAIN).clamp(-1.0, 1.0))?;
    }
    writer.finalize()?;
    Ok(())
}

fn dry_sample(samples: &[f32], channels: usize, frames: usize, frame: i64, channel: usize) -> f32 {
    usize::try_from(frame)
        .ok()
        .filter(|frame| *frame < frames)
        .map_or(0.0, |frame| {
            samples[frame * channels + channel.min(channels - 1)]
        })
}

fn replace_rendered_file(temporary: &Path, destination: &Path) -> Result<(), Box<dyn Error>> {
    if !destination.exists() {
        fs::rename(temporary, destination)?;
        return Ok(());
    }
    let backup = destination.with_extension("wav.k3-backup");
    if backup.exists() {
        return Err(format!("stale mix backup requires attention: {}", backup.display()).into());
    }
    fs::rename(destination, &backup)?;
    if let Err(error) = fs::rename(temporary, destination) {
        let _ = fs::rename(&backup, destination);
        return Err(error.into());
    }
    fs::remove_file(backup)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{render_take_mix, render_take_preview};
    use k3_core::{FileProjectRepository, ProjectRepository, VocalEffectPreset};
    use std::fs;

    #[test]
    fn renders_stereo_preview_without_changing_dry_take() {
        let sandbox = tempfile::tempdir().unwrap();
        let dry = sandbox.path().join("dry.wav");
        let backing = sandbox.path().join("backing.wav");
        let mix = sandbox.path().join("mix.wav");
        write_wav(&dry, 1, &[0.1; 4]);
        write_wav(&backing, 2, &[0.2; 8]);

        render_take_mix(&backing, &dry, &mix, 0, 0, VocalEffectPreset::Clean).unwrap();

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

        render_take_mix(&backing, &dry, &mix, 2, 0, VocalEffectPreset::Clean).unwrap();

        let mut reader = hound::WavReader::open(mix).unwrap();
        let samples: Vec<f32> = reader.samples::<f32>().map(Result::unwrap).collect();
        assert!((samples[0] - 0.4).abs() < 0.001);
        assert!((samples[1] - 0.4).abs() < 0.001);
    }

    #[test]
    fn church_preset_adds_a_finite_reverb_tail() {
        let sandbox = tempfile::tempdir().unwrap();
        let dry = sandbox.path().join("dry.wav");
        let backing = sandbox.path().join("backing.wav");
        let mix = sandbox.path().join("mix.wav");
        let mut impulse = vec![0.0; 100];
        impulse[0] = 0.1;
        write_wav_at_rate(&dry, 1, 1_000, &impulse);
        write_wav_at_rate(&backing, 2, 1_000, &[0.0; 200]);

        render_take_mix(&backing, &dry, &mix, 0, 0, VocalEffectPreset::Church).unwrap();

        let mut reader = hound::WavReader::open(mix).unwrap();
        let samples: Vec<f32> = reader.samples::<f32>().map(Result::unwrap).collect();
        assert!(samples.len() > impulse.len() * 2);
        assert!(samples.iter().all(|sample| sample.is_finite()));
        assert!(samples[2..].iter().any(|sample| sample.abs() > 0.001));
    }

    #[test]
    fn shifts_only_the_backing_without_changing_duration() {
        let sandbox = tempfile::tempdir().unwrap();
        let dry = sandbox.path().join("dry.wav");
        let backing = sandbox.path().join("backing.wav");
        let mix = sandbox.path().join("mix.wav");
        let frames = 8_000;
        write_wav_at_rate(&dry, 1, 8_000, &vec![0.0; frames]);
        let backing_samples = (0..frames)
            .flat_map(|frame| {
                let frame = u16::try_from(frame).unwrap();
                let phase = f32::from(frame) * std::f32::consts::TAU * 220.0 / 8_000.0;
                [phase.sin() * 0.25, phase.sin() * 0.25]
            })
            .collect::<Vec<_>>();
        write_wav_at_rate(&backing, 2, 8_000, &backing_samples);

        render_take_mix(&backing, &dry, &mix, 0, 6, VocalEffectPreset::Clean).unwrap();

        let mut reader = hound::WavReader::open(mix).unwrap();
        let samples = reader
            .samples::<f32>()
            .map(Result::unwrap)
            .collect::<Vec<_>>();
        assert_eq!(samples.len(), frames * 2);
        let left = samples
            .chunks_exact(2)
            .map(|frame| frame[0])
            .collect::<Vec<_>>();
        let crossings = left[1_000..7_000]
            .windows(2)
            .filter(|pair| pair[0] <= 0.0 && pair[1] > 0.0)
            .count();
        let crossings = u16::try_from(crossings).unwrap();
        let frequency = f32::from(crossings) * 8_000.0 / 6_000.0;
        assert!(
            (frequency - 311.1).abs() < 12.0,
            "frequency was {frequency}"
        );
    }

    #[test]
    fn recorded_take_can_be_rerendered_and_persisted_with_another_effect() {
        let sandbox = tempfile::tempdir().unwrap();
        let root = sandbox.path();
        fs::create_dir(root.join("stems")).unwrap();
        fs::create_dir(root.join("takes")).unwrap();
        write_wav_at_rate(
            &root.join("stems/accompaniment.wav"),
            2,
            1_000,
            &[0.05; 400],
        );
        let mut voice = vec![0.0; 200];
        voice[0] = 0.1;
        write_wav_at_rate(&root.join("takes/take-1-dry.wav"), 1, 1_000, &voice);
        fs::write(
            root.join("project.json"),
            r#"{
  "schema_version": 1,
  "id": "135a282d-5915-4b7f-a8da-1c5beff3eee3",
  "title": "Effects",
  "source": "source/song.wav",
  "lyrics": null,
  "separation": {
    "status": "ready",
    "details": {
      "vocals": "stems/vocals.wav",
      "accompaniment": "stems/accompaniment.wav",
      "provenance": {
        "provider": "test",
        "architecture": "test",
        "checkpoint_id": "test",
        "checkpoint_sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "profile": "quality"
      }
    }
  },
  "takes": [{"id": "take-1", "dry_audio": "takes/take-1-dry.wav"}],
  "latency_compensation_ms": 0,
  "effects_schema_version": 1
}"#,
        )
        .unwrap();
        let repository = FileProjectRepository;
        let mut project = repository.open(root).unwrap();

        let ktv = render_take_preview(&project, "take-1", VocalEffectPreset::Ktv).unwrap();
        let ktv_audio = fs::read(&ktv.path).unwrap();
        project
            .set_take_render("take-1", VocalEffectPreset::Ktv, ktv.relative_path)
            .unwrap();
        repository.save(&mut project).unwrap();

        let theater = render_take_preview(&project, "take-1", VocalEffectPreset::Theater).unwrap();
        assert_ne!(ktv_audio, fs::read(&theater.path).unwrap());
        project
            .set_take_render("take-1", VocalEffectPreset::Theater, theater.relative_path)
            .unwrap();
        repository.save(&mut project).unwrap();

        let reopened = repository.open(root).unwrap();
        assert_eq!(
            reopened.take("take-1").unwrap().effect_preset(),
            VocalEffectPreset::Theater
        );
        assert!(!root.join("takes/take-1-mix.wav.k3-backup").exists());
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
