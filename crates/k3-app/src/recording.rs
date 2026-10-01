use std::{
    error::Error,
    fs,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use k3_core::{
    FileProjectRepository, Project, ProjectPath, ProjectRepository, RecordingSession,
    SeparationState, Take, VocalEffectPreset,
};

use crate::{
    AudioRecorder, PlaybackCommand, PlaybackSnapshot, PlaybackStatus, RecordingSummary,
    RecordingTimelineAnchor, SessionPlayback, SessionTrackKind, place_recording_on_timeline,
    render_take_mix,
};

/// GUI 开始录音后可立即展示的信息。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GuiRecordingStarted {
    pub device: String,
}

/// GUI 停止并保存录音后返回的结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GuiRecordingResult {
    pub take_id: String,
    pub dry_path: PathBuf,
    pub duration: Duration,
    pub device: String,
    pub warning: Option<String>,
}

/// 拥有一次 GUI 录音所需的播放、麦克风和工程事务。
///
/// GUI 只负责在后台线程调用此接口，不直接接触音频设备回调。
#[derive(Default)]
pub struct GuiRecordingController {
    active: Option<ActiveRecording>,
}

struct ActiveRecording {
    recorder: AudioRecorder,
    session: RecordingSession,
    playback: SessionPlayback,
    paths: RecordingPaths,
    timeline: Vec<RecordingTimelineAnchor>,
    effect_preset: VocalEffectPreset,
}

impl GuiRecordingController {
    #[must_use]
    pub fn is_recording(&self) -> bool {
        self.active.is_some()
    }

    /// 从头播放伴奏并开始采集默认麦克风。
    ///
    /// # Errors
    ///
    /// 工程、伴奏、输入设备或输出设备不可用时返回错误。
    pub fn start(
        &mut self,
        project_root: &Path,
        monitoring: bool,
        volume: f32,
        effect_preset: VocalEffectPreset,
    ) -> Result<GuiRecordingStarted, Box<dyn Error>> {
        if self.active.is_some() {
            return Err("a recording is already active".into());
        }
        let project = FileProjectRepository.open(project_root)?;
        let paths = recording_paths(&project)?;
        let mut session = RecordingSession::new(project.clone());
        session.arm()?;

        let mut playback = SessionPlayback::open(&project);
        if !playback.has_track(SessionTrackKind::Accompaniment) {
            return Err("project accompaniment is unavailable".into());
        }
        playback.switch_track(SessionTrackKind::Accompaniment);
        playback.execute(crate::PlaybackCommand::SetVolume(volume));
        if let Some(error) = playback.error() {
            return Err(error.to_owned().into());
        }
        playback.prepare_recording()?;

        let recorder = AudioRecorder::start(
            &paths.dry_temporary_path,
            playback.monitor_player(),
            monitoring,
        )?;
        let device = recorder.device().to_owned();
        if let Err(error) = playback.play_from_start() {
            cleanup_failed_start(recorder, &paths.dry_temporary_path);
            return Err(error);
        }
        if let Err(error) = session.start() {
            cleanup_failed_start(recorder, &paths.dry_temporary_path);
            return Err(error.into());
        }

        self.active = Some(ActiveRecording {
            recorder,
            session,
            playback,
            paths,
            timeline: vec![RecordingTimelineAnchor {
                capture_frame: 0,
                song_position: Duration::ZERO,
            }],
            effect_preset,
        });
        Ok(GuiRecordingStarted { device })
    }

    pub fn set_monitoring(&self, enabled: bool) {
        if let Some(active) = &self.active {
            active.recorder.set_monitoring(enabled);
        }
    }

    /// 在录音播放实例上执行音量或跳转命令，并在跳转后记录录音时间轴锚点。
    pub fn execute(&mut self, command: PlaybackCommand) -> Option<PlaybackSnapshot> {
        let active = self.active.as_mut()?;
        let was_playing = active.playback.snapshot().status == PlaybackStatus::Playing;
        let anchors_timeline = matches!(
            command,
            PlaybackCommand::SeekBy(_)
                | PlaybackCommand::SeekTo(_)
                | PlaybackCommand::Restart
                | PlaybackCommand::SwitchTrack(_)
                | PlaybackCommand::SetKeyShift(_)
        );
        let snapshot = match command {
            PlaybackCommand::SetKeyShift(semitones) => {
                if active.session.set_key_shift_semitones(semitones).is_ok() {
                    let _ = active.playback.set_key_shift(semitones);
                }
                active.playback.snapshot()
            }
            command => active.playback.execute(command),
        };
        let resumed = snapshot.status == PlaybackStatus::Playing && !was_playing;
        if was_playing && snapshot.status == PlaybackStatus::Paused {
            active.recorder.set_paused(true);
        } else if resumed {
            active.recorder.set_paused(false);
        }
        if (anchors_timeline || resumed) && snapshot.error.is_none() {
            active.timeline.push(RecordingTimelineAnchor {
                capture_frame: active.recorder.captured_frames(),
                song_position: snapshot.position,
            });
        }
        Some(snapshot)
    }

    /// 刷新并返回录音伴奏的播放快照。
    pub fn snapshot(&mut self) -> Option<PlaybackSnapshot> {
        let active = self.active.as_mut()?;
        active.playback.refresh_stream_error();
        Some(active.playback.snapshot())
    }

    /// 停止采集、保存 dry take，并原子写回 `project.json`。
    ///
    /// # Errors
    ///
    /// WAV 完成、文件移动或工程保存失败时返回错误。
    pub fn stop(&mut self) -> Result<GuiRecordingResult, Box<dyn Error>> {
        let Some(active) = self.active.take() else {
            return Err("no recording is active".into());
        };
        finish_recording(active)
    }

    /// 放弃当前录音并清理临时 WAV。
    pub fn abort(&mut self) {
        let Some(active) = self.active.take() else {
            return;
        };
        let temporary = active.paths.dry_temporary_path.clone();
        let _ = active.recorder.stop();
        let _ = fs::remove_file(temporary);
    }
}

fn cleanup_failed_start(recorder: AudioRecorder, temporary: &Path) {
    let _ = recorder.stop();
    let _ = fs::remove_file(temporary);
}

fn finish_recording(active: ActiveRecording) -> Result<GuiRecordingResult, Box<dyn Error>> {
    let ActiveRecording {
        recorder,
        mut session,
        playback: _,
        paths,
        timeline,
        effect_preset,
    } = active;
    let RecordingSummary {
        device,
        duration,
        warning,
    } = recorder.stop()?;
    place_recording_on_timeline(&paths.dry_temporary_path, &paths.dry_final_path, &timeline)?;
    save_completed_recording(
        &mut session,
        paths,
        device,
        duration,
        warning,
        effect_preset,
    )
}

fn save_completed_recording(
    session: &mut RecordingSession,
    paths: RecordingPaths,
    device: String,
    duration: Duration,
    warning: Option<String>,
    effect_preset: VocalEffectPreset,
) -> Result<GuiRecordingResult, Box<dyn Error>> {
    let dry_path = ProjectPath::new(paths.dry_relative_path)?;
    let mut take = Take::new(paths.id.clone(), dry_path).with_effect_preset(effect_preset);
    let mix_result = match session.project().separation() {
        SeparationState::Ready(manifest) => render_take_mix(
            &manifest.accompaniment.resolve(session.project().root()),
            &paths.dry_final_path,
            &paths.mix_temporary_path,
            session.project().latency_compensation_ms(),
            session.project().key_shift_semitones(),
            effect_preset,
        )
        .and_then(|()| {
            fs::rename(&paths.mix_temporary_path, &paths.mix_final_path).map_err(Into::into)
        }),
        _ => Err("project accompaniment is unavailable".into()),
    };
    let mix_warning = match mix_result {
        Ok(()) => {
            take = take.with_mix_audio_at_key(
                ProjectPath::new(paths.mix_relative_path)?,
                session.project().key_shift_semitones(),
            );
            None
        }
        Err(error) => {
            let _ = fs::remove_file(&paths.mix_temporary_path);
            Some(format!("cannot render mix preview: {error}"))
        }
    };
    session.stop(take)?;
    FileProjectRepository.save(session.project_mut())?;
    let warning = match (warning, mix_warning) {
        (Some(recording), Some(mix)) => Some(format!("{recording}; {mix}")),
        (Some(recording), None) => Some(recording),
        (None, Some(mix)) => Some(mix),
        (None, None) => None,
    };
    Ok(GuiRecordingResult {
        take_id: paths.id,
        dry_path: paths.dry_final_path,
        duration,
        device,
        warning,
    })
}

#[cfg(test)]
mod tests {
    use super::{recording_paths, save_completed_recording};
    use crate::{LoadedProject, TrackKind};
    use k3_core::{FileProjectRepository, ProjectRepository, RecordingSession, VocalEffectPreset};
    use std::{fs, time::Duration};

    #[test]
    fn saved_gui_take_contains_backing_audio() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        fs::create_dir(root.join("stems")).unwrap();
        fs::create_dir(root.join("takes")).unwrap();
        write_wav(&root.join("stems/accompaniment.wav"), 2, &[0.2; 8]);
        write_wav(&root.join("takes/dry.wav"), 1, &[0.0; 4]);
        fs::write(
            root.join("project.json"),
            r#"{
  "schema_version": 1,
  "id": "135a282d-5915-4b7f-a8da-1c5beff3eee3",
  "title": "Recording",
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
  "takes": [],
  "latency_compensation_ms": 0,
  "effects_schema_version": 1
}"#,
        )
        .unwrap();
        let project = FileProjectRepository.open(root).unwrap();
        let paths = recording_paths(&project).unwrap();
        fs::rename(root.join("takes/dry.wav"), &paths.dry_final_path).unwrap();
        let mut session = RecordingSession::new(project);
        session.arm().unwrap();
        session.start().unwrap();

        save_completed_recording(
            &mut session,
            paths,
            "test microphone".into(),
            Duration::from_millis(4),
            None,
            VocalEffectPreset::Church,
        )
        .unwrap();

        let saved = FileProjectRepository.open(root).unwrap();
        let take = saved.takes().last().unwrap();
        assert!(take.mix_audio().is_some());
        assert_eq!(take.effect_preset(), VocalEffectPreset::Church);
        let mut dry_reader = hound::WavReader::open(take.dry_audio().resolve(root)).unwrap();
        assert!(
            dry_reader
                .samples::<f32>()
                .all(|sample| sample.unwrap() == 0.0)
        );
        let loaded = LoadedProject::open(root).unwrap();
        let take_path = loaded
            .track(TrackKind::Take)
            .unwrap()
            .path
            .as_ref()
            .unwrap();
        let mut reader = hound::WavReader::open(take_path).unwrap();
        let samples = reader
            .samples::<f32>()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert!(
            samples.len() > 8,
            "church mix should include its reverb tail"
        );
        assert!(
            samples.iter().any(|sample| *sample > 0.1),
            "saved take has no backing audio: {samples:?}"
        );
    }

    fn write_wav(path: &std::path::Path, channels: u16, samples: &[f32]) {
        let spec = hound::WavSpec {
            channels,
            sample_rate: 1_000,
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

struct RecordingPaths {
    id: String,
    dry_relative_path: String,
    dry_temporary_path: PathBuf,
    dry_final_path: PathBuf,
    mix_relative_path: String,
    mix_temporary_path: PathBuf,
    mix_final_path: PathBuf,
}

fn recording_paths(project: &Project) -> Result<RecordingPaths, Box<dyn Error>> {
    let timestamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis();
    let id = format!("take-{timestamp}");
    let dry_relative_path = format!("takes/{id}-dry.wav");
    let dry_final_path = project.root().join(&dry_relative_path);
    let dry_temporary_path = project.root().join(format!("takes/.{id}-dry.wav.partial"));
    let mix_relative_path = format!("takes/{id}-mix.wav");
    let mix_final_path = project.root().join(&mix_relative_path);
    let mix_temporary_path = project.root().join(format!("takes/.{id}-mix.wav.partial"));
    if [
        &dry_final_path,
        &dry_temporary_path,
        &mix_final_path,
        &mix_temporary_path,
    ]
    .iter()
    .any(|path| path.exists())
    {
        return Err(format!("recording destination already exists for {id}").into());
    }
    Ok(RecordingPaths {
        id,
        dry_relative_path,
        dry_temporary_path,
        dry_final_path,
        mix_relative_path,
        mix_temporary_path,
        mix_final_path,
    })
}
