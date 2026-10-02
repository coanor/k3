use std::{
    error::Error,
    fs::{self, File},
    path::{Path, PathBuf},
};

use k3_audio::PitchShiftSource;
use k3_core::{FileProjectRepository, Project, ProjectPath, SeparationState, VocalEffectPreset};
use rodio::{ChannelCount, Decoder, SampleRate, source::UniformSourceIterator};

use crate::effects::VocalEffect;

const VOICE_GAIN: f32 = 4.0;
const BACKING_GAIN: f32 = 0.65;

/// 已提交的新混音及旧文件清理警告。
pub struct RenderedTake {
    pub path: PathBuf,
    pub cleanup_warning: Option<String>,
}

/// 从不可变的干声和伴奏生成独立混音，并提交效果、调号和文件引用。
///
/// 渲染时不持有工程锁；提交失败会删除新文件，保留原混音和工程对象。
///
/// # Errors
///
/// take 或伴奏不存在、音频无效、目录越界、工程版本冲突或文件操作失败时返回错误。
pub fn render_and_save_take(
    project: &mut Project,
    take_id: &str,
    preset: VocalEffectPreset,
) -> Result<RenderedTake, Box<dyn Error>> {
    prepare_take_render(project, take_id, preset)?.commit(project, take_id, preset)
}

/// 播放前根据当前工程的效果、调号和文件状态检查混音，必要时重新渲染并保存。
///
/// # Errors
///
/// take 不存在，或重新渲染、提交失败时返回错误。
pub fn ensure_take_render(
    project: &mut Project,
    take_id: &str,
    preset: VocalEffectPreset,
) -> Result<Option<RenderedTake>, Box<dyn Error>> {
    let take = project
        .take(take_id)
        .ok_or_else(|| format!("take is not part of this project: {take_id}"))?;
    if take.effect_preset() == preset
        && take.rendered_key_semitones() == project.key_shift_semitones()
        && take
            .mix_audio()
            .is_some_and(|path| path.resolve(project.root()).is_file())
    {
        return Ok(None);
    }
    render_and_save_take(project, take_id, preset).map(Some)
}

struct PreparedTakeRender {
    path: PathBuf,
    relative_path: ProjectPath,
    committed: bool,
}

impl PreparedTakeRender {
    fn commit(
        mut self,
        project: &mut Project,
        take_id: &str,
        preset: VocalEffectPreset,
    ) -> Result<RenderedTake, Box<dyn Error>> {
        let cleanup_warning = FileProjectRepository.commit_take_render(
            project,
            take_id,
            preset,
            self.relative_path.clone(),
        )?;
        self.committed = true;
        Ok(RenderedTake {
            path: self.path.clone(),
            cleanup_warning,
        })
    }
}

impl Drop for PreparedTakeRender {
    fn drop(&mut self) {
        if !self.committed {
            let _ = fs::remove_file(&self.path);
        }
    }
}

fn prepare_take_render(
    project: &Project,
    take_id: &str,
    preset: VocalEffectPreset,
) -> Result<PreparedTakeRender, Box<dyn Error>> {
    let take = project
        .take(take_id)
        .ok_or_else(|| format!("take is not part of this project: {take_id}"))?;
    let SeparationState::Ready(separation) = project.separation() else {
        return Err("project accompaniment is not ready".into());
    };
    let takes = project.root().join("takes");
    if takes.canonicalize()? != takes {
        return Err("take render directory is redirected outside its project location".into());
    }
    let relative_path = ProjectPath::new(format!("takes/mix-{}.wav", uuid::Uuid::new_v4()))?;
    let rendered = PreparedTakeRender {
        path: relative_path.resolve(project.root()),
        relative_path,
        committed: false,
    };
    render_take_mix(
        &separation.accompaniment.resolve(project.root()),
        &take.dry_audio().resolve(project.root()),
        &rendered.path,
        project.latency_compensation_ms(),
        project.key_shift_semitones(),
        preset,
    )?;
    Ok(rendered)
}

/// 将干声效果和完整伴奏合成为立体声 WAV，伴奏使用指定调号。
///
/// # Errors
///
/// 音频不可读、干声不是有效的 32 位浮点 WAV，或输出写入失败时返回错误。
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
    let sample_rate =
        SampleRate::new(dry_spec.sample_rate).ok_or("dry take sample rate is zero")?;
    if dry_spec.channels == 0 {
        return Err("dry take channel count is zero".into());
    }
    let dry_samples: Vec<f32> = dry_reader.samples::<f32>().collect::<Result<_, _>>()?;
    let dry_channels = usize::from(dry_spec.channels);
    let frames = dry_samples.len() / dry_channels;
    let voice_offset_frames =
        i64::from(latency_compensation_ms) * i64::from(dry_spec.sample_rate) / 1_000;

    let backing = Decoder::try_from(File::open(backing_path)?)?;
    let backing = UniformSourceIterator::new(
        backing,
        ChannelCount::new(2).ok_or("mix channel count is zero")?,
        sample_rate,
    );
    let mut backing = PitchShiftSource::new(backing, key_shift_semitones);
    let spec = hound::WavSpec {
        channels: 2,
        sample_rate: dry_spec.sample_rate,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let mut effect = VocalEffect::new(effect_preset, dry_spec.sample_rate);
    let minimum_frames = frames.saturating_add(effect.tail_frames());
    let mut writer = hound::WavWriter::create(destination, spec)?;
    let mut frame = 0;
    loop {
        let backing_left = backing.next();
        let backing_right = backing.next();
        if frame >= minimum_frames && backing_left.is_none() && backing_right.is_none() {
            break;
        }
        let dry_frame = i64::try_from(frame)? + voice_offset_frames;
        let left = dry_sample(&dry_samples, dry_channels, frames, dry_frame, 0) * VOICE_GAIN;
        let right = dry_sample(&dry_samples, dry_channels, frames, dry_frame, 1) * VOICE_GAIN;
        let (left, right) = effect.process(left, right);
        writer
            .write_sample((left + backing_left.unwrap_or(0.0) * BACKING_GAIN).clamp(-1.0, 1.0))?;
        writer
            .write_sample((right + backing_right.unwrap_or(0.0) * BACKING_GAIN).clamp(-1.0, 1.0))?;
        frame += 1;
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

#[cfg(test)]
mod tests {
    use super::{ensure_take_render, prepare_take_render, render_and_save_take, render_take_mix};
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
    fn continues_backing_after_recording_stops() {
        let sandbox = tempfile::tempdir().unwrap();
        let dry = sandbox.path().join("dry.wav");
        let backing = sandbox.path().join("backing.wav");
        let mix = sandbox.path().join("mix.wav");
        write_wav(&dry, 1, &[0.1; 4]);
        write_wav(&backing, 2, &[0.2; 16]);

        render_take_mix(&backing, &dry, &mix, 0, 0, VocalEffectPreset::Clean).unwrap();

        let mut reader = hound::WavReader::open(mix).unwrap();
        let samples = reader
            .samples::<f32>()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(samples.len(), 16);
        assert!(
            samples[8..]
                .iter()
                .all(|sample| (*sample - 0.13).abs() < 0.001)
        );
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
            .as_chunks::<2>()
            .0
            .iter()
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
        let mut project = FileProjectRepository.open(root).unwrap();
        exercise_effect_changes(&mut project);
    }

    fn exercise_effect_changes(project: &mut k3_core::Project) {
        let root = project.root().to_path_buf();
        let repository = FileProjectRepository;
        let ktv = render_and_save_take(project, "take-1", VocalEffectPreset::Ktv).unwrap();
        let ktv_audio = fs::read(&ktv.path).unwrap();
        assert!(ktv.cleanup_warning.is_none());

        // 渲染期间另一前端提交，不能覆盖旧文件或留下未提交的新文件。
        let failed = prepare_take_render(project, "take-1", VocalEffectPreset::Church).unwrap();
        let failed_path = failed.path.clone();
        let mut concurrent = repository.open(&root).unwrap();
        concurrent.set_latency_compensation_ms(50);
        repository.save(&mut concurrent).unwrap();
        assert!(
            failed
                .commit(project, "take-1", VocalEffectPreset::Church)
                .is_err()
        );
        assert_eq!(fs::read(&ktv.path).unwrap(), ktv_audio);
        assert!(!failed_path.exists());
        assert_eq!(
            project.take("take-1").unwrap().effect_preset(),
            VocalEffectPreset::Ktv
        );
        let reopened = repository.open(&root).unwrap();
        assert_eq!(reopened.latency_compensation_ms(), 50);
        assert_eq!(
            reopened.take("take-1").unwrap().effect_preset(),
            VocalEffectPreset::Ktv
        );
        *project = reopened;

        // 真实写入错误也必须保留原混音和内存状态。
        fs::create_dir(root.join("project.json.tmp")).unwrap();
        let failed = prepare_take_render(project, "take-1", VocalEffectPreset::Church).unwrap();
        let failed_path = failed.path.clone();
        assert!(
            failed
                .commit(project, "take-1", VocalEffectPreset::Church)
                .is_err()
        );
        assert_eq!(fs::read(&ktv.path).unwrap(), ktv_audio);
        assert!(!failed_path.exists());
        assert_eq!(
            project.take("take-1").unwrap().effect_preset(),
            VocalEffectPreset::Ktv
        );
        fs::remove_dir(root.join("project.json.tmp")).unwrap();

        project.set_key_shift_semitones(2).unwrap();
        let theater = render_and_save_take(project, "take-1", VocalEffectPreset::Theater).unwrap();
        assert_ne!(ktv_audio, fs::read(&theater.path).unwrap());
        assert!(theater.cleanup_warning.is_none());
        assert!(!ktv.path.exists());
        let reopened = repository.open(&root).unwrap();
        let saved = reopened.take("take-1").unwrap();
        assert_eq!(saved.effect_preset(), VocalEffectPreset::Theater);
        assert_eq!(saved.rendered_key_semitones(), 2);
        assert_eq!(saved.mix_audio().unwrap().resolve(&root), theater.path);
        assert_eq!(fs::read_dir(root.join("takes")).unwrap().count(), 2);

        assert!(
            ensure_take_render(project, "take-1", VocalEffectPreset::Theater)
                .unwrap()
                .is_none()
        );
        project.set_key_shift_semitones(-2).unwrap();
        let shifted = ensure_take_render(project, "take-1", VocalEffectPreset::Theater)
            .unwrap()
            .expect("changed key must rebuild the mix before playback");
        assert_ne!(shifted.path, theater.path);
        assert_eq!(project.take("take-1").unwrap().rendered_key_semitones(), -2);
        assert!(!theater.path.exists());
        fs::remove_file(&shifted.path).unwrap();
        assert!(
            ensure_take_render(project, "take-1", VocalEffectPreset::Theater)
                .unwrap()
                .is_some()
        );
        assert!(
            ensure_take_render(project, "take-1", VocalEffectPreset::Clean)
                .unwrap()
                .is_some()
        );
        assert_eq!(fs::read_dir(root.join("takes")).unwrap().count(), 2);
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
