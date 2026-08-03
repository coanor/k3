use std::{
    error::Error,
    fs,
    io::{self, stdout},
    path::Path,
    time::{Duration, Instant},
};

use crossterm::{
    event::{self, Event, KeyCode},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use k3_core::{LyricsTimeline, Project, RecordingSession};
use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
};

pub fn open(project: Project) -> Result<(), Box<dyn Error>> {
    let lyrics = load_lyrics(&project)?;
    let session = RecordingSession::new(project);
    let preview_started = Instant::now();
    let mut guard = TerminalGuard::enter()?;

    loop {
        let position = preview_started.elapsed();
        guard.terminal.draw(|frame| {
            let areas = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(5),
                    Constraint::Min(5),
                    Constraint::Length(3),
                ])
                .split(frame.area());
            let project = session.project();
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
            ])
            .block(Block::default().title(" K3 project ").borders(Borders::ALL));
            frame.render_widget(header, areas[0]);

            let lyric = lyrics
                .as_ref()
                .and_then(|timeline| timeline.line_at(position, 0))
                .map_or("No active lyric", |line| line.text.as_str());
            let lyric_panel = Paragraph::new(lyric)
                .style(Style::default().fg(Color::Yellow))
                .block(
                    Block::default()
                        .title(format!(" Lyrics preview · {:.1}s ", position.as_secs_f32()))
                        .borders(Borders::ALL),
                )
                .wrap(Wrap { trim: true });
            frame.render_widget(lyric_panel, areas[1]);

            let footer = Paragraph::new("q quit · preview clock starts when this screen opens")
                .block(Block::default().borders(Borders::ALL));
            frame.render_widget(footer, areas[2]);
        })?;

        if event::poll(Duration::from_millis(250))?
            && matches!(
                event::read()?,
                Event::Key(key) if key.code == KeyCode::Char('q')
            )
        {
            break;
        }
    }
    Ok(())
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
