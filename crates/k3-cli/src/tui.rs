use std::{
    error::Error,
    fs,
    io::{self, stdout},
    path::{Path, PathBuf},
    time::Duration,
};

use crossterm::{
    event::{self, Event, KeyCode},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use k3_core::{LyricsTimeline, Project, RecordingSession, SeparationState};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
};

use crate::audio::AudioPlayer;

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
}

pub fn open(project: Project) -> Result<(), Box<dyn Error>> {
    let lyrics = load_lyrics(&project)?;
    let mut playback = PlaybackState::new(playback_tracks(&project));
    let session = RecordingSession::new(project);
    let mut guard = TerminalGuard::enter()?;

    loop {
        guard
            .terminal
            .draw(|frame| draw(frame, &session, lyrics.as_ref(), &playback))?;
        if event::poll(Duration::from_millis(100))?
            && let Event::Key(key) = event::read()?
            && playback.handle_key(key.code)
        {
            break;
        }
    }
    Ok(())
}

fn draw(
    frame: &mut Frame,
    session: &RecordingSession,
    lyrics: Option<&LyricsTimeline>,
    playback: &PlaybackState,
) {
    let position = playback.position();
    let areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(6),
            Constraint::Min(5),
            Constraint::Length(4),
        ])
        .split(frame.area());
    let project = session.project();
    let playback_status = playback.audio.as_ref().map_or("unavailable", |player| {
        if player.is_finished() {
            "finished"
        } else if player.is_paused() {
            "paused"
        } else {
            "playing"
        }
    });
    let duration = playback.audio.as_ref().and_then(AudioPlayer::duration);
    let volume = playback.audio.as_ref().map_or(0.0, AudioPlayer::volume);
    let track = playback.tracks[playback.selected].kind.label();
    let audio_line = if let Some(error) = &playback.error {
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
            session.state(),
            project.takes().len()
        )),
        Line::from(audio_line),
    ])
    .block(Block::default().title(" K3 project ").borders(Borders::ALL));
    frame.render_widget(header, areas[0]);

    let lyric = lyrics
        .and_then(|timeline| timeline.line_at(position, 0))
        .map_or("No active lyric", |line| line.text.as_str());
    let lyric_panel = Paragraph::new(lyric)
        .style(Style::default().fg(Color::Yellow))
        .block(
            Block::default()
                .title(format!(" Lyrics · {} ", format_duration(position)))
                .borders(Borders::ALL),
        )
        .wrap(Wrap { trim: true });
    frame.render_widget(lyric_panel, areas[1]);

    let footer = Paragraph::new(vec![
        Line::from("Space play/pause · ←/→ seek 5s · r restart · -/+ volume"),
        Line::from("1 original · 2 accompaniment · 3 vocals · q quit"),
    ])
    .block(Block::default().borders(Borders::ALL));
    frame.render_widget(footer, areas[2]);
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
    use super::format_duration;
    use std::time::Duration;

    #[test]
    fn formats_playback_position_as_minutes_and_seconds() {
        assert_eq!(format_duration(Duration::from_secs(125)), "02:05");
    }
}
