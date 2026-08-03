use std::{
    error::Error,
    fs,
    io::{self, stdout},
    path::{Path, PathBuf},
    time::Duration,
};

use clap::{Parser, Subcommand};
use crossterm::{
    event::{self, Event, KeyCode},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use k3_core::{
    CreateProject, FileProjectRepository, LyricsTimeline, Project, ProjectRepository,
    RecordingSession, SeparationState,
};
use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
};

#[derive(Debug, Parser)]
#[command(name = "k3", version, about = "Local terminal karaoke workspace")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Create a project and copy local song assets into it.
    New {
        #[arg(long)]
        root: PathBuf,
        #[arg(long)]
        song: PathBuf,
        #[arg(long)]
        lyrics: Option<PathBuf>,
        #[arg(long)]
        title: Option<String>,
    },
    /// Print a project summary without entering the TUI.
    Show {
        #[arg(long)]
        project: PathBuf,
    },
    /// Open the terminal interface. Press q to exit.
    Tui {
        #[arg(long)]
        project: PathBuf,
    },
}

fn main() {
    if let Err(error) = run(Cli::parse()) {
        eprintln!("k3: {error}");
        std::process::exit(1);
    }
}

fn run(cli: Cli) -> Result<(), Box<dyn Error>> {
    let repository = FileProjectRepository;
    match cli.command {
        Command::New {
            root,
            song,
            lyrics,
            title,
        } => {
            let project = repository.create(CreateProject {
                root,
                song,
                lyrics,
                title,
            })?;
            println!("{}", project.root().display());
        }
        Command::Show { project } => {
            let project = repository.open(&project)?;
            print_summary(&project);
        }
        Command::Tui { project } => {
            let project = repository.open(&project)?;
            open_tui(project)?;
        }
    }
    Ok(())
}

fn print_summary(project: &Project) {
    println!("title: {}", project.title());
    println!("id: {}", project.id());
    println!("source: {}", project.source().as_str());
    println!("separation: {}", separation_label(project.separation()));
    println!("takes: {}", project.takes().len());
}

fn open_tui(project: Project) -> Result<(), Box<dyn Error>> {
    let lyrics = load_lyrics(&project)?;
    let session = RecordingSession::new(project);
    let mut guard = TerminalGuard::enter()?;

    loop {
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
                    separation_label(project.separation()),
                    session.state(),
                    project.takes().len()
                )),
            ])
            .block(Block::default().title(" K3 project ").borders(Borders::ALL));
            frame.render_widget(header, areas[0]);

            let lyric = lyrics
                .as_ref()
                .and_then(|timeline| timeline.line_at(Duration::ZERO, 0))
                .map_or("No active lyric", |line| line.text.as_str());
            let lyric_panel = Paragraph::new(lyric)
                .style(Style::default().fg(Color::Yellow))
                .block(Block::default().title(" Lyrics ").borders(Borders::ALL))
                .wrap(Wrap { trim: true });
            frame.render_widget(lyric_panel, areas[1]);

            let footer = Paragraph::new("q quit · playback and audio-device adapters arrive next")
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

fn separation_label(state: &SeparationState) -> &'static str {
    match state {
        SeparationState::NotRequested => "not requested",
        SeparationState::Running => "running",
        SeparationState::Ready(_) => "ready",
        SeparationState::Failed { .. } => "failed",
    }
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
