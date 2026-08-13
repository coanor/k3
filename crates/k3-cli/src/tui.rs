use std::{
    error::Error,
    fs,
    io::{self, stdout},
    ops::Range,
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver, TryRecvError},
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use crossterm::{
    event::{self, Event, KeyCode, KeyEvent, KeyEventKind},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use k3_core::{
    FileProjectRepository, LyricsTimeline, Project, ProjectPath, ProjectRepository,
    RecordingSession, RecordingState, SeparationState, Take, VocalEffectPreset,
};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
};

use crate::{
    audio::AudioPlayer,
    library::{self, LibraryConfig, LibrarySnapshot},
    lyrics_download::{LyricsDownload, download_missing_lyrics},
    mix::{render_take_mix, render_take_preview},
    recorder::{AudioRecorder, RecordingTimelineAnchor, place_recording_on_timeline},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LibraryFocus {
    Projects,
    Project,
    Sources,
}

impl LibraryFocus {
    const fn next(self) -> Self {
        match self {
            Self::Projects => Self::Project,
            Self::Project => Self::Sources,
            Self::Sources => Self::Projects,
        }
    }

    const fn previous(self) -> Self {
        match self {
            Self::Projects => Self::Sources,
            Self::Project => Self::Projects,
            Self::Sources => Self::Project,
        }
    }
}

struct ImportJob {
    source: PathBuf,
    result: Receiver<Result<PathBuf, String>>,
}

struct MediaLibrary {
    config: LibraryConfig,
    snapshot: LibrarySnapshot,
    focus: LibraryFocus,
    project_selected: usize,
    source_selected: usize,
    job: Option<ImportJob>,
    message: Option<String>,
}

impl MediaLibrary {
    fn new(config: LibraryConfig) -> Result<Self, Box<dyn Error>> {
        let snapshot = library::scan(&config)?;
        Ok(Self {
            config,
            snapshot,
            focus: LibraryFocus::Projects,
            project_selected: 0,
            source_selected: 0,
            job: None,
            message: None,
        })
    }

    fn refresh(&mut self) -> Result<(), Box<dyn Error>> {
        self.snapshot = library::scan(&self.config)?;
        self.project_selected = self
            .project_selected
            .min(self.snapshot.projects.len().saturating_sub(1));
        self.source_selected = self
            .source_selected
            .min(self.snapshot.sources.len().saturating_sub(1));
        Ok(())
    }

    fn start_import(&mut self) {
        if self.job.is_some() {
            self.message = Some("已有分离任务正在运行，请等待完成".into());
            return;
        }
        let Some(source) = self.snapshot.sources.get(self.source_selected) else {
            self.message = Some("音乐源目录中没有可导入的音频".into());
            return;
        };
        if source.imported {
            self.message = Some("该音频已有同名 project；可从左栏打开".into());
            return;
        }
        let path = source.path.clone();
        let worker_source = path.clone();
        let config = self.config.clone();
        let (sender, result) = mpsc::channel();
        thread::spawn(move || {
            let outcome = library::import_and_separate(&config, &worker_source)
                .map_err(|error| error.to_string());
            let _ = sender.send(outcome);
        });
        self.job = Some(ImportJob {
            source: path.clone(),
            result,
        });
        self.message = Some(format!("正在创建并分离：{}", display_name(&path)));
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TrackKind {
    Original,
    Accompaniment,
    Vocals,
    Take,
}

impl TrackKind {
    const fn label(self) -> &'static str {
        match self {
            Self::Original => "original",
            Self::Accompaniment => "accompaniment",
            Self::Vocals => "vocals",
            Self::Take => "take",
        }
    }

    const fn recording_shortcut(key: KeyCode) -> Option<Self> {
        match key {
            KeyCode::Char('1') => Some(Self::Original),
            KeyCode::Char('2') => Some(Self::Accompaniment),
            KeyCode::Char('3') => Some(Self::Vocals),
            _ => None,
        }
    }
}

struct PlaybackTrack {
    kind: TrackKind,
    path: PathBuf,
}

struct PlaybackState {
    tracks: Vec<PlaybackTrack>,
    selected: usize,
    audio: Option<AudioPlayer>,
    error: Option<String>,
    key_shift_semitones: i8,
}

impl PlaybackState {
    fn new(tracks: Vec<PlaybackTrack>, key_shift_semitones: i8) -> Self {
        let selected = tracks
            .iter()
            .position(|track| track.kind == TrackKind::Accompaniment)
            .unwrap_or(0);
        let mut error = None;
        let audio = match AudioPlayer::open(&tracks[selected].path, key_shift_semitones) {
            Ok(player) => Some(player),
            Err(open_error) => {
                error = Some(open_error.to_string());
                None
            }
        };
        Self {
            tracks,
            selected,
            audio,
            error,
            key_shift_semitones,
        }
    }

    fn position(&self) -> Duration {
        self.audio
            .as_ref()
            .map_or(Duration::ZERO, AudioPlayer::position)
    }

    fn refresh_stream_error(&mut self) {
        if let Some(error) = self.audio.as_ref().and_then(AudioPlayer::take_stream_error) {
            self.error = Some(error);
        }
    }

    fn handle_key(&mut self, key: KeyCode) -> bool {
        match key {
            KeyCode::Char('q') => return true,
            KeyCode::Char(' ') => {
                if let Some(player) = &self.audio {
                    player.toggle();
                    self.error = None;
                }
            }
            KeyCode::Left => {
                let result = self
                    .audio
                    .as_ref()
                    .map_or(Ok(()), |player| player.seek_by(-5));
                self.update_error(result);
            }
            KeyCode::Right => {
                let result = self
                    .audio
                    .as_ref()
                    .map_or(Ok(()), |player| player.seek_by(5));
                self.update_error(result);
            }
            KeyCode::Char('r') => {
                let path = &self.tracks[self.selected].path;
                let key_shift = self.selected_key_shift();
                let result = self.audio.as_mut().map_or(Ok(()), |player| {
                    player.load(path, Duration::ZERO, true, key_shift)
                });
                self.update_error(result);
            }
            key if let Some(kind) = TrackKind::recording_shortcut(key) => self.switch_track(kind),
            KeyCode::Char('-') => {
                if let Some(player) = &self.audio {
                    player.adjust_volume(-0.1);
                }
            }
            KeyCode::Char('+' | '=') => {
                if let Some(player) = &self.audio {
                    player.adjust_volume(0.1);
                }
            }
            _ => {}
        }
        false
    }

    fn switch_track(&mut self, kind: TrackKind) {
        let Some(next) = self.tracks.iter().position(|track| track.kind == kind) else {
            self.error = Some(format!("{} track is unavailable", kind.label()));
            return;
        };
        if next == self.selected {
            return;
        }

        let result = if let Some(player) = &mut self.audio {
            let position = player.position();
            let should_play = !player.is_paused() && !player.is_finished();
            let key_shift = if self.tracks[next].kind == TrackKind::Take {
                0
            } else {
                self.key_shift_semitones
            };
            player.load(&self.tracks[next].path, position, should_play, key_shift)
        } else {
            let key_shift = if self.tracks[next].kind == TrackKind::Take {
                0
            } else {
                self.key_shift_semitones
            };
            AudioPlayer::open(&self.tracks[next].path, key_shift).map(|player| {
                self.audio = Some(player);
            })
        };
        if result.is_ok() {
            self.selected = next;
        }
        self.update_error(result);
    }

    fn update_error(&mut self, result: Result<(), Box<dyn Error>>) {
        self.error = result.err().map(|error| error.to_string());
    }

    fn prepare_recording(&mut self) -> Result<(), Box<dyn Error>> {
        let key_shift = self.selected_key_shift();
        let player = self
            .audio
            .as_mut()
            .ok_or("playback is unavailable; cannot synchronize recording")?;
        player.load(
            &self.tracks[self.selected].path,
            Duration::ZERO,
            false,
            key_shift,
        )?;
        self.error = None;
        Ok(())
    }

    fn play_from_start(&mut self) -> Result<(), Box<dyn Error>> {
        let player = self
            .audio
            .as_ref()
            .ok_or("playback is unavailable; cannot synchronize recording")?;
        player.play_prepared();
        self.error = None;
        Ok(())
    }

    fn play_take(&mut self, path: &Path) -> Result<(), Box<dyn Error>> {
        let index = if let Some(index) = self
            .tracks
            .iter()
            .position(|track| track.kind == TrackKind::Take)
        {
            self.tracks[index].path = path.to_path_buf();
            index
        } else {
            self.tracks.push(PlaybackTrack {
                kind: TrackKind::Take,
                path: path.to_path_buf(),
            });
            self.tracks.len() - 1
        };
        if let Some(player) = &mut self.audio {
            player.load(path, Duration::ZERO, true, 0)?;
        } else {
            self.audio = Some(AudioPlayer::open(path, 0)?);
        }
        self.selected = index;
        self.error = None;
        Ok(())
    }

    fn selected_key_shift(&self) -> i8 {
        if self.tracks[self.selected].kind == TrackKind::Take {
            0
        } else {
            self.key_shift_semitones
        }
    }

    fn accompaniment_path(&self) -> Result<PathBuf, Box<dyn Error>> {
        self.tracks
            .iter()
            .find(|track| track.kind == TrackKind::Accompaniment)
            .map(|track| track.path.clone())
            .ok_or_else(|| "project accompaniment is unavailable".into())
    }

    fn set_key_shift(&mut self, semitones: i8) -> Result<(), Box<dyn Error>> {
        self.key_shift_semitones = semitones;
        let key_shift = self.selected_key_shift();
        let Some(player) = &mut self.audio else {
            return Ok(());
        };
        let position = player.position();
        let should_play = !player.is_paused() && !player.is_finished();
        player.load(
            &self.tracks[self.selected].path,
            position,
            should_play,
            key_shift,
        )
    }
}

struct ActiveRecording {
    recorder: AudioRecorder,
    id: String,
    dry_relative_path: String,
    dry_temporary_path: PathBuf,
    dry_final_path: PathBuf,
    mix_relative_path: String,
    mix_temporary_path: PathBuf,
    mix_final_path: PathBuf,
    backing_path: PathBuf,
    backing_key_shift_semitones: i8,
    timeline: Vec<RecordingTimelineAnchor>,
}

struct App {
    playback: PlaybackState,
    session: RecordingSession,
    active_recording: Option<ActiveRecording>,
    recording_message: Option<String>,
    project_dirty: bool,
    monitoring_enabled: bool,
    selected_take: Option<usize>,
    lyrics: Option<LyricsTimeline>,
}

impl App {
    fn new(
        project: Project,
        lyrics: Option<LyricsTimeline>,
        startup_message: Option<String>,
    ) -> Self {
        let selected_take = project.takes().len().checked_sub(1);
        let key_shift_semitones = project.key_shift_semitones();
        Self {
            playback: PlaybackState::new(playback_tracks(&project), key_shift_semitones),
            session: RecordingSession::new(project),
            active_recording: None,
            recording_message: startup_message,
            project_dirty: false,
            monitoring_enabled: false,
            selected_take,
            lyrics,
        }
    }

    fn handle_key(&mut self, key: KeyCode) -> bool {
        match (self.session.state(), key) {
            (_, KeyCode::Char('m')) => self.toggle_monitoring(),
            (RecordingState::Idle, KeyCode::Char('a')) => self.arm(),
            (RecordingState::Idle, KeyCode::Char('[')) => self.select_take(-1),
            (RecordingState::Idle, KeyCode::Char(']')) => self.select_take(1),
            (RecordingState::Idle, KeyCode::Char('e')) => self.cycle_take_effect(),
            (RecordingState::Idle, KeyCode::Char('4')) => self.play_selected_take(),
            (RecordingState::Idle, KeyCode::Char(',')) => self.adjust_key(-1),
            (RecordingState::Idle, KeyCode::Char('.')) => self.adjust_key(1),
            (RecordingState::Idle, KeyCode::Char('/')) => self.reset_key(),
            (RecordingState::Armed, KeyCode::Enter) => self.start_recording(),
            (RecordingState::Armed, KeyCode::Esc) => self.cancel_arm(),
            (RecordingState::Recording, KeyCode::Enter) => {
                self.finish_recording();
            }
            (RecordingState::Recording, KeyCode::Char('q')) => {
                return self.finish_recording();
            }
            (RecordingState::Recording, KeyCode::Char('-' | '+' | '=')) => {
                self.playback.handle_key(key);
            }
            (RecordingState::Recording, KeyCode::Left) => {
                self.seek_recording_by_lyric(-1);
            }
            (RecordingState::Recording, KeyCode::Right) => {
                self.seek_recording_by_lyric(1);
            }
            (RecordingState::Recording, key)
                if let Some(kind) = TrackKind::recording_shortcut(key) =>
            {
                self.playback.switch_track(kind);
                self.anchor_recording_at_playback_position();
                self.recording_message = self.playback.error.as_ref().map_or_else(
                    || {
                        Some(format!(
                            "录音继续 · 当前监听 {} · take 仍只混入 accompaniment",
                            self.playback.tracks[self.playback.selected].kind.label()
                        ))
                    },
                    |error| Some(format!("切换监听失败: {error}")),
                );
            }
            (RecordingState::Recording, _) => {
                self.recording_message =
                    Some("录音中可按 1/2/3 切换监听；按 Enter 停止录音".into());
            }
            (RecordingState::Armed, KeyCode::Char('q')) => {
                self.cancel_arm();
                return true;
            }
            (_, KeyCode::Char('q')) => {
                return self.save_project();
            }
            _ => return self.playback.handle_key(key),
        }
        false
    }

    fn toggle_monitoring(&mut self) {
        self.monitoring_enabled = !self.monitoring_enabled;
        if let Some(active) = &self.active_recording {
            active.recorder.set_monitoring(self.monitoring_enabled);
        }
        self.recording_message = Some(if self.monitoring_enabled {
            "麦克风监听已开启 · 请使用耳机，避免回声或啸叫".into()
        } else {
            "麦克风监听已关闭".into()
        });
    }

    fn seek_recording_by_lyric(&mut self, direction: i8) {
        let Some(timeline) = self.lyrics.as_ref() else {
            self.recording_message = Some("没有同步歌词，录音中无法按歌词跳转".into());
            return;
        };
        let Some(target) = lyric_seek_target(timeline, self.playback.position(), direction) else {
            self.recording_message = Some("已经到达歌词时间轴边界".into());
            return;
        };
        let Some(player) = &self.playback.audio else {
            self.recording_message = Some("playback is unavailable; cannot seek recording".into());
            return;
        };
        if let Err(error) = player.seek_to(target) {
            self.recording_message = Some(format!("按歌词跳转失败: {error}"));
            return;
        }
        self.anchor_recording(target);
        self.recording_message = Some(format!(
            "录音继续 · 已按歌词跳转到 {} · 此后重唱将覆盖对应时间段",
            format_duration(target)
        ));
    }

    fn anchor_recording_at_playback_position(&mut self) {
        self.anchor_recording(self.playback.position());
    }

    fn anchor_recording(&mut self, song_position: Duration) {
        if let Some(active) = &mut self.active_recording {
            active.timeline.push(RecordingTimelineAnchor {
                capture_frame: active.recorder.captured_frames(),
                song_position,
            });
        }
    }

    fn select_take(&mut self, direction: i32) {
        let count = self.session.project().takes().len();
        let Some(current) = self.selected_take else {
            self.recording_message = Some("当前 project 还没有 take".into());
            return;
        };
        let next = if direction.is_negative() {
            current.checked_sub(1).unwrap_or(count - 1)
        } else {
            (current + 1) % count
        };
        self.selected_take = Some(next);
        let take = &self.session.project().takes()[next];
        self.recording_message = Some(format!(
            "selected take {}/{}: {} · effect {}",
            next + 1,
            count,
            take.id(),
            take.effect_preset().label()
        ));
    }

    fn play_selected_take(&mut self) {
        let Some(index) = self.selected_take else {
            self.recording_message = Some("当前 project 还没有 take".into());
            return;
        };
        let take = &self.session.project().takes()[index];
        let stale = take.mix_audio().is_none()
            || take.rendered_key_semitones() != self.session.project().key_shift_semitones();
        let path = if stale {
            match self.rerender_take(index, take.effect_preset()) {
                Ok(path) => path,
                Err(error) => {
                    self.recording_message = Some(format!("重建 take mix 失败: {error}"));
                    return;
                }
            }
        } else {
            self.session
                .project()
                .root()
                .join(Path::new(take.mix_audio().expect("checked above").as_str()))
        };
        let result = self.playback.play_take(&path);
        self.recording_message = result.err().map_or_else(
            || Some("playing selected take mix".into()),
            |error| Some(error.to_string()),
        );
    }

    fn cycle_take_effect(&mut self) {
        let Some(index) = self.selected_take else {
            self.recording_message = Some("当前 project 还没有 take".into());
            return;
        };
        let preset = self.session.project().takes()[index].effect_preset().next();
        match self.rerender_take(index, preset) {
            Ok(path) => {
                let playback_warning = self.playback.play_take(&path).err();
                self.recording_message = Some(playback_warning.map_or_else(
                    || format!("take effect: {} · mix rebuilt and playing", preset.label()),
                    |error| format!("take effect: {} · playback error: {error}", preset.label()),
                ));
            }
            Err(error) => self.recording_message = Some(format!("effect render failed: {error}")),
        }
    }

    fn rerender_take(
        &mut self,
        index: usize,
        preset: VocalEffectPreset,
    ) -> Result<PathBuf, Box<dyn Error>> {
        let take_id = self.session.project().takes()[index].id().to_owned();
        let rendered = render_take_preview(self.session.project(), &take_id, preset)?;
        let path = rendered.path;
        self.session
            .set_take_render(&take_id, preset, rendered.relative_path)?;
        self.project_dirty = true;
        if !self.save_project() {
            return Err("cannot save rebuilt take".into());
        }
        Ok(path)
    }

    fn adjust_key(&mut self, delta: i8) {
        let current = self.session.project().key_shift_semitones();
        self.set_key((current + delta).clamp(-6, 6));
    }

    fn reset_key(&mut self) {
        self.set_key(0);
    }

    fn set_key(&mut self, semitones: i8) {
        if semitones == self.session.project().key_shift_semitones() {
            self.recording_message = Some(format!("Key {}", format_key(semitones)));
            return;
        }
        if let Err(error) = self.session.set_key_shift_semitones(semitones) {
            self.recording_message = Some(error.to_string());
            return;
        }
        self.project_dirty = true;
        let playback_warning = self.playback.set_key_shift(semitones).err();
        let _ = self.save_project();
        self.recording_message = Some(playback_warning.map_or_else(
            || {
                format!(
                    "Key {} · 已保存；旧 take 将在播放时自动重建",
                    format_key(semitones)
                )
            },
            |error| format!("Key {} · playback error: {error}", format_key(semitones)),
        ));
    }

    fn arm(&mut self) {
        let result = self
            .session
            .arm()
            .map_err(|error| -> Box<dyn Error> { Box::new(error) })
            .and_then(|()| self.playback.prepare_recording());
        match result {
            Ok(()) => self.recording_message = Some("armed · press Enter to record".into()),
            Err(error) => {
                if self.session.state() == RecordingState::Armed {
                    let _ = self.session.cancel();
                }
                self.recording_message = Some(error.to_string());
            }
        }
    }

    fn cancel_arm(&mut self) {
        match self.session.cancel() {
            Ok(()) => self.recording_message = Some("recording cancelled".into()),
            Err(error) => self.recording_message = Some(error.to_string()),
        }
    }

    fn start_recording(&mut self) {
        let paths = match recording_paths(self.session.project()) {
            Ok(paths) => paths,
            Err(error) => {
                self.recording_message = Some(error.to_string());
                return;
            }
        };
        let backing_path = match self.playback.accompaniment_path() {
            Ok(path) => path,
            Err(error) => {
                self.recording_message = Some(error.to_string());
                return;
            }
        };
        let backing_key_shift_semitones = self.session.project().key_shift_semitones();
        let monitor_mixer = self.playback.audio.as_ref().map(AudioPlayer::mixer);
        let recorder = match AudioRecorder::start(
            &paths.dry_temporary_path,
            monitor_mixer,
            self.monitoring_enabled,
        ) {
            Ok(recorder) => recorder,
            Err(error) => {
                self.recording_message = Some(error.to_string());
                return;
            }
        };
        let device = recorder.device().to_owned();
        if let Err(error) = self.playback.play_from_start() {
            let _ = recorder.stop();
            let _ = fs::remove_file(&paths.dry_temporary_path);
            self.recording_message = Some(error.to_string());
            return;
        }
        if let Err(error) = self.session.start() {
            let _ = recorder.stop();
            let _ = fs::remove_file(&paths.dry_temporary_path);
            self.recording_message = Some(error.to_string());
            return;
        }
        self.active_recording = Some(ActiveRecording {
            recorder,
            id: paths.id,
            dry_relative_path: paths.dry_relative_path,
            dry_temporary_path: paths.dry_temporary_path,
            dry_final_path: paths.dry_final_path,
            mix_relative_path: paths.mix_relative_path,
            mix_temporary_path: paths.mix_temporary_path,
            mix_final_path: paths.mix_final_path,
            backing_path,
            backing_key_shift_semitones,
            timeline: vec![RecordingTimelineAnchor {
                capture_frame: 0,
                song_position: Duration::ZERO,
            }],
        });
        let monitoring = if self.monitoring_enabled {
            "on · use headphones"
        } else {
            "off"
        };
        self.recording_message = Some(format!(
            "recording microphone: {device} · monitoring: {monitoring}"
        ));
    }

    fn finish_recording(&mut self) -> bool {
        let Some(active) = self.active_recording.take() else {
            self.recording_message = Some("active recorder is unavailable".into());
            let _ = self.session.abort();
            return false;
        };
        let summary = match active.recorder.stop() {
            Ok(summary) => summary,
            Err(error) => {
                let _ = fs::remove_file(&active.dry_temporary_path);
                let _ = self.session.abort();
                self.recording_message = Some(error.to_string());
                return false;
            }
        };
        if let Err(error) = place_recording_on_timeline(
            &active.dry_temporary_path,
            &active.dry_final_path,
            &active.timeline,
        ) {
            let _ = self.session.abort();
            self.recording_message = Some(format!(
                "cannot place recording on song timeline: {}; raw audio remains at {}",
                error,
                active.dry_temporary_path.display()
            ));
            return false;
        }
        let dry_project_path = match ProjectPath::new(active.dry_relative_path) {
            Ok(path) => path,
            Err(error) => {
                let _ = self.session.abort();
                self.recording_message = Some(error.to_string());
                return false;
            }
        };
        let mix_result = render_take_mix(
            &active.backing_path,
            &active.dry_final_path,
            &active.mix_temporary_path,
            self.session.project().latency_compensation_ms(),
            active.backing_key_shift_semitones,
            VocalEffectPreset::Clean,
        )
        .and_then(|()| {
            fs::rename(&active.mix_temporary_path, &active.mix_final_path).map_err(Into::into)
        });
        let mut take = Take::new(active.id, dry_project_path);
        let mix_warning = match mix_result {
            Ok(()) => match ProjectPath::new(active.mix_relative_path) {
                Ok(path) => {
                    take = take
                        .with_mix_audio_at_key(path, self.session.project().key_shift_semitones());
                    None
                }
                Err(error) => Some(error.to_string()),
            },
            Err(error) => {
                let _ = fs::remove_file(&active.mix_temporary_path);
                Some(format!("cannot render mix preview: {error}"))
            }
        };
        let mix_saved = mix_warning.is_none();
        if let Err(error) = self.session.stop(take) {
            self.recording_message = Some(error.to_string());
            return false;
        }
        self.selected_take = self.session.project().takes().len().checked_sub(1);
        self.project_dirty = true;

        let warning = summary
            .warning
            .into_iter()
            .chain(mix_warning)
            .collect::<Vec<_>>();
        let warning = if warning.is_empty() {
            String::new()
        } else {
            format!(" · warning: {}", warning.join("; "))
        };
        self.recording_message = Some(format!(
            "saved {:.1}s {} from {}{}",
            summary.duration.as_secs_f32(),
            if mix_saved {
                "dry + mix take"
            } else {
                "dry take"
            },
            summary.device,
            warning
        ));
        self.save_project()
    }

    fn save_project(&mut self) -> bool {
        if !self.project_dirty {
            return true;
        }
        match FileProjectRepository.save(self.session.project()) {
            Ok(()) => {
                self.project_dirty = false;
                true
            }
            Err(error) => {
                self.recording_message = Some(format!("cannot save project: {error}"));
                false
            }
        }
    }
}

pub fn open(project: Project, startup_message: Option<String>) -> Result<(), Box<dyn Error>> {
    let lyrics = load_lyrics(&project)?;
    let mut app = App::new(project, lyrics, startup_message);
    let mut guard = TerminalGuard::enter()?;

    loop {
        app.playback.refresh_stream_error();
        guard.terminal.draw(|frame| draw(frame, &app))?;
        if event::poll(Duration::from_millis(100))?
            && let Event::Key(key) = event::read()?
            && should_handle_key(&key)
            && app.handle_key(key.code)
        {
            break;
        }
    }
    Ok(())
}

pub fn open_library(config: LibraryConfig) -> Result<(), Box<dyn Error>> {
    let mut library = MediaLibrary::new(config)?;
    let mut current = library
        .snapshot
        .projects
        .first()
        .map(|entry| open_library_project(&library.config, &entry.path))
        .transpose()?;
    let mut guard = TerminalGuard::enter()?;

    loop {
        if let Some(app) = &mut current {
            app.playback.refresh_stream_error();
        }
        poll_import_job(&mut library, &mut current)?;
        guard
            .terminal
            .draw(|frame| draw_library(frame, &library, current.as_ref()))?;
        if event::poll(Duration::from_millis(100))?
            && let Event::Key(key) = event::read()?
            && should_handle_key(&key)
            && handle_library_key(&mut library, &mut current, key.code)?
        {
            break;
        }
    }
    Ok(())
}

fn open_library_project(config: &LibraryConfig, path: &Path) -> Result<App, Box<dyn Error>> {
    let repository = FileProjectRepository;
    let mut project = repository.open(path)?;
    let message = if config.lyrics.auto_download {
        match download_missing_lyrics(&mut project, &mut |_| {}) {
            Ok(LyricsDownload::Downloaded { track, artist }) => {
                repository.save(&project)?;
                Some(format!("已从 LRCLIB 下载歌词：{artist} - {track}"))
            }
            Ok(LyricsDownload::NotFound) => Some("未找到时长匹配的同步歌词".into()),
            Ok(LyricsDownload::AlreadyPresent) => None,
            Err(error) => Some(format!("自动下载歌词失败：{error}")),
        }
    } else {
        None
    };
    let lyrics = load_lyrics(&project)?;
    Ok(App::new(project, lyrics, message))
}

fn poll_import_job(
    library: &mut MediaLibrary,
    current: &mut Option<App>,
) -> Result<(), Box<dyn Error>> {
    let outcome = match library.job.as_ref().map(|job| job.result.try_recv()) {
        Some(Ok(result)) => Some(result),
        Some(Err(TryRecvError::Disconnected)) => Some(Err("分离任务线程意外退出".to_owned())),
        Some(Err(TryRecvError::Empty)) | None => None,
    };
    let Some(outcome) = outcome else {
        return Ok(());
    };
    library.job = None;
    library.refresh()?;
    match outcome {
        Ok(path) => {
            if let Some(index) = library
                .snapshot
                .projects
                .iter()
                .position(|entry| entry.path == path)
            {
                library.project_selected = index;
            }
            *current = Some(open_library_project(&library.config, &path)?);
            library.message = Some(format!("分离完成：{}", path.display()));
        }
        Err(error) => library.message = Some(format!("分离失败：{error}")),
    }
    Ok(())
}

fn handle_library_key(
    library: &mut MediaLibrary,
    current: &mut Option<App>,
    key: KeyCode,
) -> Result<bool, Box<dyn Error>> {
    match key {
        KeyCode::Tab => library.focus = library.focus.next(),
        KeyCode::BackTab => library.focus = library.focus.previous(),
        KeyCode::Char('r') if library.focus != LibraryFocus::Project => {
            library.refresh()?;
            library.message = Some("媒体库列表已刷新".into());
        }
        KeyCode::Char('q') if library.focus != LibraryFocus::Project => {
            if let Some(app) = current
                && !app.save_project()
            {
                return Ok(false);
            }
            return Ok(true);
        }
        _ => match library.focus {
            LibraryFocus::Projects => match key {
                KeyCode::Up => {
                    library.project_selected = library.project_selected.saturating_sub(1);
                }
                KeyCode::Down => {
                    library.project_selected = (library.project_selected + 1)
                        .min(library.snapshot.projects.len().saturating_sub(1));
                }
                KeyCode::Enter => {
                    if current
                        .as_ref()
                        .is_some_and(|app| app.session.state() != RecordingState::Idle)
                    {
                        library.message = Some("请先结束或取消当前录音，再切换 project".into());
                    } else if let Some(entry) =
                        library.snapshot.projects.get(library.project_selected)
                    {
                        if let Some(app) = current
                            && !app.save_project()
                        {
                            return Ok(false);
                        }
                        *current = Some(open_library_project(&library.config, &entry.path)?);
                        library.message = Some(format!("已打开：{}", entry.title));
                    }
                }
                _ => {}
            },
            LibraryFocus::Sources => match key {
                KeyCode::Up => {
                    library.source_selected = library.source_selected.saturating_sub(1);
                }
                KeyCode::Down => {
                    library.source_selected = (library.source_selected + 1)
                        .min(library.snapshot.sources.len().saturating_sub(1));
                }
                KeyCode::Enter | KeyCode::Char('s') => {
                    if let Some(source) = library.snapshot.sources.get(library.source_selected)
                        && source.imported
                    {
                        if current
                            .as_ref()
                            .is_some_and(|app| app.session.state() != RecordingState::Idle)
                        {
                            library.message = Some("请先结束或取消当前录音，再切换 project".into());
                        } else {
                            *current =
                                Some(open_library_project(&library.config, &source.project_path)?);
                            if let Some(index) = library
                                .snapshot
                                .projects
                                .iter()
                                .position(|entry| entry.path == source.project_path)
                            {
                                library.project_selected = index;
                            }
                            library.message =
                                Some(format!("已打开：{}", source.project_path.display()));
                        }
                    } else {
                        library.start_import();
                    }
                }
                _ => {}
            },
            LibraryFocus::Project => {
                if let Some(app) = current {
                    return Ok(app.handle_key(key));
                }
            }
        },
    }
    Ok(false)
}

fn should_handle_key(key: &KeyEvent) -> bool {
    key.kind == KeyEventKind::Press
}

fn draw(frame: &mut Frame, app: &App) {
    draw_project(frame, frame.area(), app);
}

fn draw_project(frame: &mut Frame, area: ratatui::layout::Rect, app: &App) {
    let position = app.playback.position();
    let areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(7),
            Constraint::Min(5),
            Constraint::Length(5),
        ])
        .split(area);
    let project = app.session.project();
    let playback_status = app.playback.audio.as_ref().map_or("unavailable", |player| {
        if player.is_finished() {
            "finished"
        } else if player.is_paused() {
            "paused"
        } else {
            "playing"
        }
    });
    let duration = app.playback.audio.as_ref().and_then(AudioPlayer::duration);
    let volume = app.playback.audio.as_ref().map_or(0.0, AudioPlayer::volume);
    let track = app.playback.tracks[app.playback.selected].kind.label();
    let take_status = app.selected_take.map_or_else(
        || "selected take: none".to_owned(),
        |index| {
            let take = &project.takes()[index];
            format!(
                "selected take: {}/{} · effect {}",
                index + 1,
                project.takes().len(),
                take.effect_preset().label()
            )
        },
    );
    let audio_line = if let Some(error) = &app.playback.error {
        format!("audio: {track} · error: {error}")
    } else {
        format!(
            "audio: {} · {} · {} / {} · volume {:.0}% · Key {}",
            track,
            playback_status,
            format_duration(position),
            duration.map_or_else(|| "--:--".into(), format_duration),
            volume * 100.0,
            format_key(project.key_shift_semitones()),
        )
    };
    let header = Paragraph::new(vec![
        Line::from(vec![
            Span::styled(
                project.title(),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(format!("  [{}]", project.id())),
        ]),
        Line::from(format!("source: {}", project.source().as_str())),
        Line::from(format!(
            "separation: {}  recording: {:?}  takes: {}  {}",
            project.separation(),
            app.session.state(),
            project.takes().len(),
            take_status
        )),
        Line::from(audio_line),
        Line::from(
            app.recording_message
                .as_deref()
                .unwrap_or("recording: press a to arm"),
        ),
    ])
    .block(Block::default().title(" K3 project ").borders(Borders::ALL));
    frame.render_widget(header, areas[0]);

    let visible_rows = usize::from(areas[1].height.saturating_sub(2));
    let (lyric_lines, lyric_progress) =
        lyrics_for_display(app.lyrics.as_ref(), position, visible_rows);
    let lyric_panel = Paragraph::new(lyric_lines)
        .block(
            Block::default()
                .title(format!(
                    " Lyrics · {}{} ",
                    format_duration(position),
                    lyric_progress
                ))
                .borders(Borders::ALL),
        )
        .wrap(Wrap { trim: false });
    frame.render_widget(lyric_panel, areas[1]);

    let footer = Paragraph::new(vec![
        Line::from("Space play/pause · ←/→ seek 5s（录音中按歌词跳转）· r restart · -/+ volume"),
        Line::from("1 original · 2 accompaniment · 3 vocals（录音中也可切换）· 4 take · q quit"),
        Line::from(
            "[/] select take · e next effect · / reset Key · a arm · Enter start/stop · m monitor",
        ),
    ])
    .block(Block::default().borders(Borders::ALL));
    frame.render_widget(footer, areas[2]);
}

fn draw_library(frame: &mut Frame, library: &MediaLibrary, current: Option<&App>) {
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(22),
            Constraint::Percentage(56),
            Constraint::Percentage(22),
        ])
        .split(frame.area());
    draw_library_projects(frame, columns[0], library);
    if let Some(app) = current {
        draw_project(frame, columns[1], app);
    } else {
        frame.render_widget(
            Paragraph::new("还没有 project\n\nTab 切换到右栏，选择音乐后按 Enter 创建并分离")
                .block(Block::default().title(" K3 ").borders(Borders::ALL))
                .wrap(Wrap { trim: false }),
            columns[1],
        );
    }
    draw_library_sources(frame, columns[2], library);
}

fn draw_library_projects(frame: &mut Frame, area: ratatui::layout::Rect, library: &MediaLibrary) {
    let rows = library
        .snapshot
        .projects
        .iter()
        .enumerate()
        .map(|(index, entry)| {
            library_row(
                &entry.title,
                index == library.project_selected,
                library.focus == LibraryFocus::Projects,
            )
        })
        .collect::<Vec<_>>();
    let title = if library.focus == LibraryFocus::Projects {
        " Projects · Enter 打开 "
    } else {
        " Projects "
    };
    frame.render_widget(
        Paragraph::new(if rows.is_empty() {
            vec![Line::from("没有 project")]
        } else {
            rows
        })
        .block(Block::default().title(title).borders(Borders::ALL))
        .scroll((list_scroll(library.project_selected, area.height), 0)),
        area,
    );
}

fn draw_library_sources(frame: &mut Frame, area: ratatui::layout::Rect, library: &MediaLibrary) {
    let mut rows = library
        .snapshot
        .sources
        .iter()
        .enumerate()
        .map(|(index, entry)| {
            let state = if entry.imported { "✓" } else { "+" };
            library_row(
                &format!("{state} {}", display_name(&entry.path)),
                index == library.source_selected,
                library.focus == LibraryFocus::Sources,
            )
        })
        .collect::<Vec<_>>();
    if let Some(job) = &library.job {
        rows.push(Line::from(Span::styled(
            format!("⏳ {}", display_name(&job.source)),
            Style::default().fg(Color::Yellow),
        )));
    }
    if let Some(message) = &library.message {
        rows.push(Line::from(""));
        rows.push(Line::from(Span::styled(
            message.clone(),
            Style::default().fg(Color::Cyan),
        )));
    }
    let title = if library.focus == LibraryFocus::Sources {
        " Music · Enter 分离 "
    } else {
        " Music "
    };
    frame.render_widget(
        Paragraph::new(if rows.is_empty() {
            vec![Line::from("没有音频文件")]
        } else {
            rows
        })
        .block(Block::default().title(title).borders(Borders::ALL))
        .scroll((list_scroll(library.source_selected, area.height), 0))
        .wrap(Wrap { trim: false }),
        area,
    );
}

fn library_row(text: &str, selected: bool, focused: bool) -> Line<'static> {
    let marker = if selected { "▶ " } else { "  " };
    let style = if selected && focused {
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD)
    } else if selected {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default()
    };
    Line::from(Span::styled(format!("{marker}{text}"), style))
}

fn list_scroll(selected: usize, height: u16) -> u16 {
    let visible = usize::from(height.saturating_sub(2)).max(1);
    u16::try_from(selected.saturating_sub(visible - 1)).unwrap_or(u16::MAX)
}

fn display_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |value| value.to_string_lossy().into_owned(),
    )
}

fn format_key(semitones: i8) -> String {
    format!("{semitones:+}")
}

fn lyrics_for_display(
    lyrics: Option<&LyricsTimeline>,
    position: Duration,
    visible_rows: usize,
) -> (Vec<Line<'static>>, String) {
    lyrics.map_or_else(
        || (vec![Line::from("未加载歌词")], String::new()),
        |timeline| {
            let window = lyric_window(timeline, position, visible_rows);
            let countdown = lyric_countdown(timeline, position);
            let lines = window
                .range
                .clone()
                .map(|index| {
                    let is_current = window.current == Some(index);
                    let is_past = window.current.is_some_and(|current| index < current);
                    let style = if is_current {
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD)
                    } else if is_past {
                        Style::default().fg(Color::DarkGray)
                    } else {
                        Style::default().fg(Color::White)
                    };
                    let marker = if is_current {
                        Span::styled("▶   ", style)
                    } else if countdown.is_some_and(|countdown| countdown.index == index) {
                        let seconds = countdown.expect("countdown was just matched").seconds;
                        Span::styled(
                            format!("  {seconds} "),
                            Style::default()
                                .fg(Color::Cyan)
                                .add_modifier(Modifier::BOLD),
                        )
                    } else {
                        Span::raw("    ")
                    };
                    let text = &timeline.lines()[index].text;
                    Line::from(vec![
                        marker,
                        Span::styled(if text.is_empty() { "♪" } else { text }.to_owned(), style),
                    ])
                })
                .collect::<Vec<_>>();
            let progress = window.current.map_or_else(String::new, |current| {
                format!(" · {}/{}", current + 1, timeline.lines().len())
            });
            let lines = if lines.is_empty() {
                vec![Line::from("歌词文件中没有时间轴歌词")]
            } else {
                lines
            };
            (lines, progress)
        },
    )
}

fn lyric_seek_target(
    timeline: &LyricsTimeline,
    position: Duration,
    direction: i8,
) -> Option<Duration> {
    let lines = timeline.lines();
    if direction < 0 {
        let current = lines.partition_point(|line| line.at <= position);
        let target = current.checked_sub(2)?;
        return lines.get(target).map(|line| line.at);
    }
    let next = lines.partition_point(|line| line.at <= position);
    lines.get(next).map(|line| line.at)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct LyricCountdown {
    index: usize,
    seconds: u8,
}

/// 返回下一句歌词进入前三秒内的视觉倒计时。
///
/// 相邻歌词不足一秒时不提示，避免快速歌词持续闪烁。
fn lyric_countdown(timeline: &LyricsTimeline, position: Duration) -> Option<LyricCountdown> {
    let next = timeline.lines().partition_point(|line| line.at <= position);
    let next_line = timeline.lines().get(next)?;
    let interval_start = next
        .checked_sub(1)
        .map_or(Duration::ZERO, |index| timeline.lines()[index].at);
    if next_line.at.saturating_sub(interval_start) < Duration::from_secs(1) {
        return None;
    }

    let remaining = next_line.at.saturating_sub(position);
    if remaining.is_zero() || remaining > Duration::from_secs(3) {
        return None;
    }
    let seconds = remaining.as_nanos().div_ceil(1_000_000_000);
    Some(LyricCountdown {
        index: next,
        seconds: u8::try_from(seconds).expect("three-second countdown fits in u8"),
    })
}

#[derive(Debug, PartialEq, Eq)]
struct LyricWindow {
    range: Range<usize>,
    current: Option<usize>,
}

fn lyric_window(timeline: &LyricsTimeline, position: Duration, visible_rows: usize) -> LyricWindow {
    let total = timeline.lines().len();
    let current = timeline.active_index(position, 0);
    if total == 0 || visible_rows == 0 {
        return LyricWindow {
            range: 0..0,
            current,
        };
    }

    let look_behind = (visible_rows / 4).min(2);
    let mut start = current.map_or(0, |current| current.saturating_sub(look_behind));
    let end = start.saturating_add(visible_rows).min(total);
    start = end.saturating_sub(visible_rows).min(start);
    LyricWindow {
        range: start..end,
        current,
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
    let dry_final_path = project.root().join(Path::new(&dry_relative_path));
    let dry_temporary_path = project.root().join(format!("takes/.{id}-dry.wav.partial"));
    let mix_relative_path = format!("takes/{id}-mix.wav");
    let mix_final_path = project.root().join(Path::new(&mix_relative_path));
    let mix_temporary_path = project.root().join(format!("takes/.{id}-mix.wav.partial"));
    if [
        &dry_final_path,
        &dry_temporary_path,
        &mix_final_path,
        &mix_temporary_path,
    ]
    .into_iter()
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

fn playback_tracks(project: &Project) -> Vec<PlaybackTrack> {
    let mut tracks = vec![PlaybackTrack {
        kind: TrackKind::Original,
        path: project.source_path(),
    }];
    if let SeparationState::Ready(manifest) = project.separation() {
        tracks.push(PlaybackTrack {
            kind: TrackKind::Accompaniment,
            path: project
                .root()
                .join(Path::new(manifest.accompaniment.as_str())),
        });
        tracks.push(PlaybackTrack {
            kind: TrackKind::Vocals,
            path: project.root().join(Path::new(manifest.vocals.as_str())),
        });
    }
    if let Some(mix_audio) = project.takes().last().and_then(Take::mix_audio) {
        tracks.push(PlaybackTrack {
            kind: TrackKind::Take,
            path: project.root().join(Path::new(mix_audio.as_str())),
        });
    }
    tracks
}

fn format_duration(duration: Duration) -> String {
    let seconds = duration.as_secs();
    format!("{:02}:{:02}", seconds / 60, seconds % 60)
}

fn load_lyrics(project: &Project) -> Result<Option<LyricsTimeline>, io::Error> {
    project
        .lyrics()
        .map(|relative| {
            fs::read_to_string(project.root().join(Path::new(relative.as_str())))
                .map(|text| LyricsTimeline::parse(&text))
        })
        .transpose()
}

struct TerminalGuard {
    terminal: Terminal<CrosstermBackend<io::Stdout>>,
}

impl TerminalGuard {
    fn enter() -> io::Result<Self> {
        enable_raw_mode()?;
        let mut output = stdout();
        if let Err(error) = execute!(output, EnterAlternateScreen) {
            let _ = disable_raw_mode();
            return Err(error);
        }
        match Terminal::new(CrosstermBackend::new(output)) {
            Ok(terminal) => Ok(Self { terminal }),
            Err(error) => {
                let _ = disable_raw_mode();
                let _ = execute!(stdout(), LeaveAlternateScreen);
                Err(error)
            }
        }
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(self.terminal.backend_mut(), LeaveAlternateScreen);
        let _ = self.terminal.show_cursor();
    }
}

#[cfg(test)]
mod tests {
    use super::{
        LyricCountdown, PlaybackState, PlaybackTrack, TrackKind, format_duration, lyric_countdown,
        lyric_seek_target, lyric_window, should_handle_key,
    };
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
    use k3_core::LyricsTimeline;
    use std::time::Duration;

    #[test]
    fn formats_playback_position_as_minutes_and_seconds() {
        assert_eq!(format_duration(Duration::from_secs(125)), "02:05");
    }

    #[test]
    fn lyric_window_keeps_more_upcoming_lines_visible() {
        let timeline = LyricsTimeline::parse(
            "[00:00.000]0\n[00:01.000]1\n[00:02.000]2\n[00:03.000]3\n[00:04.000]4\n[00:05.000]5\n[00:06.000]6\n[00:07.000]7",
        );

        let window = lyric_window(&timeline, Duration::from_millis(3_500), 5);

        assert_eq!(window.range, 2..7);
        assert_eq!(window.current, Some(3));
    }

    #[test]
    fn lyric_countdown_shows_at_most_three_seconds_for_the_next_line() {
        let timeline = LyricsTimeline::parse("[00:02.000]第一句\n[00:07.000]第二句");

        assert_eq!(
            lyric_countdown(&timeline, Duration::ZERO),
            Some(LyricCountdown {
                index: 0,
                seconds: 2
            })
        );
        assert_eq!(
            lyric_countdown(&timeline, Duration::from_millis(500)),
            Some(LyricCountdown {
                index: 0,
                seconds: 2
            })
        );
        assert_eq!(
            lyric_countdown(&timeline, Duration::from_millis(4_001)),
            Some(LyricCountdown {
                index: 1,
                seconds: 3
            })
        );
        assert_eq!(
            lyric_countdown(&timeline, Duration::from_millis(6_001)),
            Some(LyricCountdown {
                index: 1,
                seconds: 1
            })
        );
        assert_eq!(lyric_countdown(&timeline, Duration::from_secs(7)), None);
    }

    #[test]
    fn lyric_countdown_ignores_intervals_shorter_than_one_second() {
        let timeline = LyricsTimeline::parse("[00:02.000]快\n[00:02.900]歌词");

        assert_eq!(
            lyric_countdown(&timeline, Duration::from_millis(2_100)),
            None
        );
    }

    #[test]
    fn recording_arrows_seek_to_previous_and_next_lyric_timestamps() {
        let timeline =
            LyricsTimeline::parse("[00:01.000]第一句\n[00:04.000]第二句\n[00:08.000]第三句");

        assert_eq!(
            lyric_seek_target(&timeline, Duration::from_secs(5), -1),
            Some(Duration::from_secs(1))
        );
        assert_eq!(
            lyric_seek_target(&timeline, Duration::from_secs(5), 1),
            Some(Duration::from_secs(8))
        );
        assert_eq!(
            lyric_seek_target(&timeline, Duration::from_millis(500), 1),
            Some(Duration::from_secs(1))
        );
        assert_eq!(
            lyric_seek_target(&timeline, Duration::from_secs(1), -1),
            None
        );
        assert_eq!(
            lyric_seek_target(&timeline, Duration::from_secs(9), 1),
            None
        );
    }

    #[test]
    fn recording_shortcuts_allow_switching_between_the_three_source_tracks() {
        assert_eq!(
            TrackKind::recording_shortcut(KeyCode::Char('1')),
            Some(TrackKind::Original)
        );
        assert_eq!(
            TrackKind::recording_shortcut(KeyCode::Char('2')),
            Some(TrackKind::Accompaniment)
        );
        assert_eq!(
            TrackKind::recording_shortcut(KeyCode::Char('3')),
            Some(TrackKind::Vocals)
        );
        assert_eq!(TrackKind::recording_shortcut(KeyCode::Char('4')), None);
    }

    #[test]
    fn take_backing_is_accompaniment_even_when_original_is_selected() {
        let playback = PlaybackState {
            tracks: vec![
                PlaybackTrack {
                    kind: TrackKind::Original,
                    path: "original.flac".into(),
                },
                PlaybackTrack {
                    kind: TrackKind::Accompaniment,
                    path: "stems/accompaniment.wav".into(),
                },
                PlaybackTrack {
                    kind: TrackKind::Vocals,
                    path: "stems/vocals.wav".into(),
                },
            ],
            selected: 0,
            audio: None,
            error: None,
            key_shift_semitones: 0,
        };

        assert_eq!(
            playback.accompaniment_path().unwrap(),
            std::path::PathBuf::from("stems/accompaniment.wav")
        );
    }

    #[test]
    fn ignores_windows_key_release_events() {
        let release = KeyEvent {
            code: KeyCode::Char('m'),
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Release,
            state: KeyEventState::NONE,
        };

        assert!(!should_handle_key(&release));
    }
}
