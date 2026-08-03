mod tui;

use std::{error::Error, path::PathBuf};

use clap::{Parser, Subcommand};
use k3_core::{CreateProject, FileProjectRepository, Project, ProjectRepository};

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
            tui::open(project)?;
        }
    }
    Ok(())
}

fn print_summary(project: &Project) {
    println!("title: {}", project.title());
    println!("id: {}", project.id());
    println!("source: {}", project.source().as_str());
    println!("separation: {}", project.separation());
    println!("takes: {}", project.takes().len());
}
