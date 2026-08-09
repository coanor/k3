mod audio;
mod python_separator;
mod tui;

use std::{error::Error, path::PathBuf};

use clap::{Parser, Subcommand, ValueEnum};
use k3_core::{
    CreateProject, FileProjectRepository, Project, ProjectRepository, SeparationProfile,
    SongPreparation,
};

use crate::python_separator::{PythonSeparatorConfig, PythonStemSeparator};

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
    /// Separate the project song into local vocals and accompaniment WAV files.
    Separate {
        #[arg(long)]
        project: PathBuf,
        #[arg(long, value_enum, default_value = "balanced")]
        profile: ProfileArgument,
        /// Explicit worker model ID; otherwise the profile default is used.
        #[arg(long)]
        model: Option<String>,
        /// Path to the k3-separator Python executable.
        #[arg(long, default_value = "k3-separator")]
        worker: PathBuf,
        /// Optional checkpoint cache directory passed to the worker.
        #[arg(long)]
        model_dir: Option<PathBuf>,
        /// Override existing vocals.wav and accompaniment.wav.
        #[arg(long)]
        overwrite: bool,
        /// Override the model segment size to trade quality/speed for memory.
        #[arg(long)]
        segment_size: Option<u32>,
        /// Disable mixed-precision CUDA inference.
        #[arg(long)]
        no_autocast: bool,
    },
    /// Open the terminal interface. Press q to exit.
    Tui {
        #[arg(long)]
        project: PathBuf,
    },
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum ProfileArgument {
    Fast,
    Balanced,
    Quality,
    Compatible,
}

impl From<ProfileArgument> for SeparationProfile {
    fn from(value: ProfileArgument) -> Self {
        match value {
            ProfileArgument::Fast => Self::Fast,
            ProfileArgument::Balanced => Self::Balanced,
            ProfileArgument::Quality => Self::Quality,
            ProfileArgument::Compatible => Self::Compatible,
        }
    }
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
        Command::Separate {
            project,
            profile,
            model,
            worker,
            model_dir,
            overwrite,
            segment_size,
            no_autocast,
        } => {
            let mut project = repository.open(&project)?;
            let separator = PythonStemSeparator::new(PythonSeparatorConfig {
                worker,
                model_dir,
                project_root: project.root().to_path_buf(),
                model_id: model,
                overwrite,
                segment_size,
                autocast: !no_autocast,
            });
            let mut preparation = SongPreparation::new(separator);
            let result = preparation.prepare(&mut project, profile.into());
            repository.save(&project)?;
            result?;
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
