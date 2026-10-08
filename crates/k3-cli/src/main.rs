mod library;
mod python_separator;
mod tui;

use k3::netease;

use std::{
    error::Error,
    io::{self, Write},
    path::{Path, PathBuf},
};

use clap::{Parser, Subcommand, ValueEnum};
use k3_core::{
    CreateProject, FileProjectRepository, Project, ProjectRepository, SeparationFailure,
    SeparationProfile, SongPreparation, VocalEffectPreset,
};

use crate::python_separator::{
    DeviceSelection, PythonSeparatorConfig, PythonStemSeparator, cleanup_obsolete_outputs,
    separation_log_path, separation_output_paths,
};
use k3_app::render_and_save_take;

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
        /// Select CPU, require GPU without CPU fallback, or choose automatically.
        #[arg(long, value_enum)]
        device: Option<DeviceSelection>,
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
        /// Disable the default second pass that keeps backing vocals in accompaniment.
        #[arg(long)]
        no_preserve_backing_vocals: bool,
    },
    /// Rebuild a recorded take preview with a vocal-effect preset.
    Effect {
        #[arg(long)]
        project: PathBuf,
        /// Take ID, or "latest" for the newest take.
        #[arg(long, default_value = "latest")]
        take: String,
        #[arg(long, value_enum)]
        preset: EffectArgument,
    },
    /// Delete a take and its dry/mix files. Confirmation defaults to keeping it.
    DeleteTake {
        #[arg(long)]
        project: PathBuf,
        /// Take ID, or "latest" for the newest take.
        #[arg(long, default_value = "latest")]
        take: String,
        /// Confirm deletion and skip the interactive prompt.
        #[arg(long)]
        yes: bool,
    },
    /// Open the terminal interface. Press q to exit.
    Tui {
        /// Open one project directly (legacy single-project mode).
        #[arg(long, required_unless_present = "config", conflicts_with = "config")]
        project: Option<PathBuf>,
        /// Open the three-panel media library using this JSON configuration.
        #[arg(long, required_unless_present = "project", conflicts_with = "project")]
        config: Option<PathBuf>,
        /// Advance recorded vocals by this many milliseconds in generated mixes.
        #[arg(long, allow_hyphen_values = true)]
        latency_ms: Option<i32>,
        /// Shift playback and backing tracks by -6 to +6 semitones without changing speed.
        #[arg(long, allow_hyphen_values = true)]
        key: Option<i8>,
        /// Do not query LRCLIB when the project has no local synced lyrics.
        #[arg(long)]
        no_lyrics_download: bool,
        /// Opt in to the unofficial `NetEase` web endpoint after LRCLIB has no usable match.
        #[arg(long)]
        netease_lyrics: bool,
    },
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum ProfileArgument {
    Fast,
    Balanced,
    Quality,
    Compatible,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum EffectArgument {
    Clean,
    Studio,
    Ktv,
    Theater,
    Church,
}

impl From<EffectArgument> for k3_core::VocalEffectPreset {
    fn from(value: EffectArgument) -> Self {
        match value {
            EffectArgument::Clean => Self::Clean,
            EffectArgument::Studio => Self::Studio,
            EffectArgument::Ktv => Self::Ktv,
            EffectArgument::Theater => Self::Theater,
            EffectArgument::Church => Self::Church,
        }
    }
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
            device,
            profile,
            model,
            worker,
            model_dir,
            overwrite,
            segment_size,
            no_autocast,
            no_preserve_backing_vocals,
        } => {
            let mut project = repository.open(&project)?;
            let previous_outputs = separation_output_paths(&project);
            let separator = PythonStemSeparator::new(PythonSeparatorConfig {
                device: match device {
                    Some(device) => device,
                    None => DeviceSelection::from_environment(DeviceSelection::Auto)?,
                },
                worker,
                model_dir,
                project_root: project.root().to_path_buf(),
                log_path: separation_log_path()?,
                model_id: model,
                overwrite,
                segment_size,
                autocast: !no_autocast,
                preserve_backing_vocals: !no_preserve_backing_vocals,
            });
            let mut preparation = SongPreparation::new(separator);
            let result = prepare_project(&mut preparation, &mut project, profile.into(), overwrite);
            persist_preparation(repository, &mut project, &previous_outputs, result)?;
            print_summary(&project);
        }
        Command::Effect {
            project,
            take,
            preset,
        } => {
            apply_take_effect(&project, &take, preset.into())?;
        }
        Command::Tui {
            project,
            config,
            latency_ms,
            key,
            no_lyrics_download,
            netease_lyrics,
        } => {
            if let Some(config) = config {
                if latency_ms.is_some() || key.is_some() {
                    return Err("--latency-ms and --key require --project mode".into());
                }
                open_library_tui(&config, no_lyrics_download, netease_lyrics)?;
            } else {
                open_tui(
                    repository,
                    project.as_deref().expect("clap requires project or config"),
                    latency_ms,
                    key,
                    no_lyrics_download,
                    netease_lyrics,
                )?;
            }
        }
        Command::DeleteTake { project, take, yes } => {
            delete_take(&project, &take, yes)?;
        }
    }
    Ok(())
}

fn apply_take_effect(
    root: &Path,
    take: &str,
    preset: VocalEffectPreset,
) -> Result<(), Box<dyn Error>> {
    let mut project = FileProjectRepository.open(root)?;
    let take_id = if take == "latest" {
        project
            .takes()
            .last()
            .ok_or("project has no recorded takes")?
            .id()
            .to_owned()
    } else {
        take.to_owned()
    };
    let rendered = render_and_save_take(&mut project, &take_id, preset)?;
    if let Some(warning) = rendered.cleanup_warning {
        eprintln!("k3: warning: {warning}");
    }
    println!("{}", rendered.path.display());
    Ok(())
}

fn delete_take(root: &Path, take: &str, yes: bool) -> Result<(), Box<dyn Error>> {
    let loaded = FileProjectRepository.open(root)?;
    let take_id = if take == "latest" {
        loaded
            .takes()
            .last()
            .ok_or("project has no recorded takes")?
            .id()
            .to_owned()
    } else {
        loaded
            .take(take)
            .ok_or_else(|| format!("take is not part of this project: {take}"))?;
        take.to_owned()
    };
    if !yes {
        print!("Delete take {take_id} and its dry/mix audio? [y/N] ");
        io::stdout().flush()?;
        let mut answer = String::new();
        io::stdin().read_line(&mut answer)?;
        if !matches!(answer.trim(), "y" | "Y" | "yes" | "YES") {
            println!("Take kept");
            return Ok(());
        }
    }
    let deleted = FileProjectRepository.delete_take(root, &take_id, loaded.document_revision())?;
    println!("Deleted take {take_id}");
    if let Some(warning) = deleted.cleanup_warning {
        eprintln!("k3: warning: {warning}");
    }
    Ok(())
}

fn persist_preparation(
    repository: FileProjectRepository,
    project: &mut Project,
    previous_outputs: &[PathBuf],
    result: Result<(), SeparationFailure>,
) -> Result<(), Box<dyn Error>> {
    let produced_outputs = separation_output_paths(project);
    if let Err(error) = repository.save(project) {
        cleanup_obsolete_outputs(project.root(), &produced_outputs, previous_outputs);
        return Err(error.into());
    }
    cleanup_obsolete_outputs(project.root(), previous_outputs, &produced_outputs);
    result.map_err(Into::into)
}

fn prepare_project(
    preparation: &mut SongPreparation<PythonStemSeparator>,
    project: &mut Project,
    profile: SeparationProfile,
    overwrite: bool,
) -> Result<(), SeparationFailure> {
    if overwrite {
        preparation.reprepare(project, profile)
    } else {
        preparation.prepare(project, profile)
    }
}

fn open_library_tui(
    config_path: &std::path::Path,
    no_lyrics_download: bool,
    netease_lyrics: bool,
) -> Result<(), Box<dyn Error>> {
    let mut config = library::LibraryConfig::load(config_path)?;
    if no_lyrics_download {
        config.lyrics.auto_download = false;
    }
    if netease_lyrics {
        config.lyrics.netease_fallback = true;
    }
    tui::open_library(config)
}

fn open_tui(
    repository: FileProjectRepository,
    project_path: &std::path::Path,
    latency_ms: Option<i32>,
    key: Option<i8>,
    no_lyrics_download: bool,
    netease_lyrics: bool,
) -> Result<(), Box<dyn Error>> {
    let mut project = repository.open(project_path)?;
    if let Some(latency_ms) = latency_ms {
        if !(-1_000..=1_000).contains(&latency_ms) {
            return Err("--latency-ms must be between -1000 and 1000".into());
        }
        project.set_latency_compensation_ms(latency_ms);
        repository.save(&mut project)?;
    }
    if let Some(key) = key {
        project.set_key_shift_semitones(key)?;
        repository.save(&mut project)?;
    }
    tui::open(project, None, !no_lyrics_download, netease_lyrics)
}

fn print_summary(project: &Project) {
    println!("title: {}", project.title());
    println!("id: {}", project.id());
    println!("source: {}", project.source().as_str());
    println!("separation: {}", project.separation());
    println!("takes: {}", project.takes().len());
    println!("key shift: {:+} semitones", project.key_shift_semitones());
    println!(
        "latency compensation: {} ms",
        project.latency_compensation_ms()
    );
}
