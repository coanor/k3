use std::{
    error::Error,
    fs,
    io::{self, stdout},
    ops::Range,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use crossterm::{
    event::{self, Event, KeyCode, KeyEvent, KeyEventKind},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use k3_core::{
    FileProjectRepository, LyricsTimeline, Project, ProjectPath, ProjectRepository,
    RecordingSession, RecordingState, SeparationState, Take,
};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
};

use crate::{audio::AudioPlayer, mix::render_take_mix, recorder::AudioRecorder};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TrackKind {
    Original,
    Accompaniment,
    Vocals,
}

impl TrackKind {
    const fn label(self) -> &'static str {
        match self {
            Self::Original => "original",
            Self::Accompaniment => "accompaniment",
            Self::Vocals => "vocals",
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
}

impl PlaybackState {
    fn new(tracks: Vec<PlaybackTrack>) -> Self {
        let selected = tracks
            .iter()
            .position(|track| track.kind == TrackKind::Accompaniment)
            .unwrap_or(0);
        let mut error = None;
        let audio = match AudioPlayer::open(&tracks[selected].path) {
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
                let result = self
                    .audio
                    .as_mut()
                    .map_or(Ok(()), |player| player.load(path, Duration::ZERO, true));
                self.update_error(result);
            }
            KeyCode::Char('1') => self.switch_track(TrackKind::Original),
            KeyCode::Char('2') => self.switch_track(TrackKind::Accompaniment),
            KeyCode::Char('3') => self.switch_track(TrackKind::Vocals),
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
            player.load(&self.tracks[next].path, position, should_play)
        } else {
            AudioPlayer::open(&self.tracks[next].path).map(|player| {
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
        let player = self
            .audio
            .as_mut()
            .ok_or("playback is unavailable; cannot synchronize recording")?;
        player.load(&self.tracks[self.selected].path, Duration::ZERO, false)?;
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
}

struct App {
    playback: PlaybackState,
    session: RecordingSession,
    active_recording: Option<ActiveRecording>,
    recording_message: Option<String>,
    project_dirty: bool,
    monitoring_enabled: bool,
}

impl App {
    fn new(project: Project) -> Self {
        Self {
            playback: PlaybackState::new(playback_tracks(&project)),
            session: RecordingSession::new(project),
            active_recording: None,
            recording_message: None,
            project_dirty: false,
            monitoring_enabled: false,
        }
    }

    fn handle_key(&mut self, key: KeyCode) -> bool {
        match (self.session.state(), key) {
            (_, KeyCode::Char('m')) => self.toggle_monitoring(),
            (RecordingState::Idle, KeyCode::Char('a')) => self.arm(),
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
            (RecordingState::Recording, _) => {
                self.recording_message =
                    Some("press Enter to stop recording before changing playback".into());
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
            backing_path: self.playback.tracks[self.playback.selected].path.clone(),
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
        if let Err(error) = fs::rename(&active.dry_temporary_path, &active.dry_final_path) {
            let _ = self.session.abort();
            self.recording_message = Some(format!(
                "cannot commit recording {}; audio remains at {}",
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
        )
        .and_then(|()| {
            fs::rename(&active.mix_temporary_path, &active.mix_final_path).map_err(Into::into)
        });
        let mut take = Take::new(active.id, dry_project_path);
        let mix_warning = match mix_result {
            Ok(()) => match ProjectPath::new(active.mix_relative_path) {
                Ok(path) => {
                    take = take.with_mix_audio(path);
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

pub fn open(project: Project) -> Result<(), Box<dyn Error>> {
    let lyrics = load_lyrics(&project)?;
    let mut app = App::new(project);
    let mut guard = TerminalGuard::enter()?;

    loop {
        app.playback.refresh_stream_error();
        guard
            .terminal
            .draw(|frame| draw(frame, lyrics.as_ref(), &app))?;
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

fn should_handle_key(key: &KeyEvent) -> bool {
    key.kind == KeyEventKind::Press
}

fn draw(frame: &mut Frame, lyrics: Option<&LyricsTimeline>, app: &App) {
    let position = app.playback.position();
    let areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(7),
            Constraint::Min(5),
            Constraint::Length(5),
        ])
        .split(frame.area());
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
    let audio_line = if let Some(error) = &app.playback.error {
        format!("audio: {track} · error: {error}")
    } else {
        format!(
            "audio: {} · {} · {} / {} · volume {:.0}%",
            track,
            playback_status,
            format_duration(position),
            duration.map_or_else(|| "--:--".into(), format_duration),
            volume * 100.0,
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
            "separation: {}  recording: {:?}  takes: {}",
            project.separation(),
            app.session.state(),
            project.takes().len()
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
    let (lyric_lines, lyric_progress) = lyrics_for_display(lyrics, position, visible_rows);
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
        Line::from("Space play/pause · ←/→ seek 5s · r restart · -/+ volume"),
        Line::from("1 original · 2 accompaniment · 3 vocals · q quit"),
        Line::from("a arm recording · Enter start/stop · m monitor · Esc cancel arm"),
    ])
    .block(Block::default().borders(Borders::ALL));
    frame.render_widget(footer, areas[2]);
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
                    let marker = if is_current { "▶ " } else { "  " };
                    let text = &timeline.lines()[index].text;
                    Line::from(Span::styled(
                        format!("{marker}{}", if text.is_empty() { "♪" } else { text }),
                        style,
                    ))
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
    use super::{format_duration, lyric_window, should_handle_key};
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
