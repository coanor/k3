mod audio;
mod audio_config;
mod effects;
mod library;
mod lyrics_download;
mod mix;
mod pitch;
mod python_separator;
mod recorder;
mod remote_separator;
mod tui;

use std::{env, error::Error, path::PathBuf, time::Duration};

use clap::{Parser, Subcommand, ValueEnum};
use k3_core::{
    CreateProject, FileProjectRepository, Project, ProjectRepository, SeparationFailure,
    SeparationOutputLayout, SeparationProfile, SongPreparation,
};

use crate::mix::render_take_preview;
use crate::python_separator::{PythonSeparatorConfig, PythonStemSeparator, separation_log_path};
use crate::remote_separator::{RemoteJobError, RemoteSeparationCoordinator, RemoteSeparatorConfig};

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
        /// Disable the default second pass that keeps backing vocals in accompaniment.
        #[arg(long)]
        no_preserve_backing_vocals: bool,
        /// Remote separator base URL. When set, the local Python worker is not used.
        #[arg(long)]
        server_url: Option<String>,
        /// Environment variable containing the remote bearer token.
        #[arg(long, requires = "server_url")]
        token_env: Option<String>,
        /// Stable name persisted with a remote job for later resume.
        #[arg(long, default_value = "default")]
        server_profile: String,
        /// Stem contract requested from a remote separator.
        #[arg(long, value_enum, default_value = "karaoke")]
        output_layout: OutputLayoutArgument,
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

#[derive(Clone, Copy, Debug, ValueEnum)]
enum OutputLayoutArgument {
    TwoStem,
    Karaoke,
}

impl From<OutputLayoutArgument> for SeparationOutputLayout {
    fn from(value: OutputLayoutArgument) -> Self {
        match value {
            OutputLayoutArgument::TwoStem => Self::TwoStem,
            OutputLayoutArgument::Karaoke => Self::Karaoke,
        }
    }
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
        } => create_project(repository, root, song, lyrics, title)?,
        Command::Show { project } => show_project(repository, &project)?,
        Command::Separate {
            project,
            profile,
            model,
            worker,
            model_dir,
            overwrite,
            segment_size,
            no_autocast,
            no_preserve_backing_vocals,
            server_url,
            token_env,
            server_profile,
            output_layout,
        } => {
            let mut project = repository.open(&project)?;
            if let Some(server_url) = server_url {
                let token_env = token_env.ok_or("--token-env is required with --server-url")?;
                let token = env::var(&token_env).map_err(|_| {
                    format!("separator token environment variable is not set: {token_env}")
                })?;
                let remote = RemoteSeparatorConfig {
                    server_profile,
                    server_url,
                    token,
                    model_id: model
                        .or_else(|| {
                            project
                                .separation_operation()
                                .map(|operation| operation.model_id().to_owned())
                        })
                        .ok_or("--model is required when creating a remote job")?,
                    output_layout: output_layout.into(),
                    poll_interval: Duration::from_millis(500),
                };
                return run_remote_separation(
                    repository,
                    &mut project,
                    remote,
                    profile.into(),
                    overwrite,
                );
            }
            let separator = PythonStemSeparator::new(PythonSeparatorConfig {
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
            repository.save(&project)?;
            result?;
            print_summary(&project);
        }
        Command::Effect {
            project,
            take,
            preset,
        } => run_effect(repository, &project, take, preset)?,
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
    }
    Ok(())
}

fn create_project(
    repository: FileProjectRepository,
    root: PathBuf,
    song: PathBuf,
    lyrics: Option<PathBuf>,
    title: Option<String>,
) -> Result<(), Box<dyn Error>> {
    let project = repository.create(CreateProject {
        root,
        song,
        lyrics,
        title,
    })?;
    println!("{}", project.root().display());
    Ok(())
}

fn show_project(
    repository: FileProjectRepository,
    project_path: &std::path::Path,
) -> Result<(), Box<dyn Error>> {
    let project = repository.open(project_path)?;
    print_summary(&project);
    Ok(())
}

fn run_remote_separation(
    _repository: FileProjectRepository,
    project: &mut Project,
    config: RemoteSeparatorConfig,
    profile: SeparationProfile,
    overwrite: bool,
) -> Result<(), Box<dyn Error>> {
    if let Err(error) = RemoteSeparationCoordinator::new(config).run(project, profile, overwrite) {
        let recovery = match error {
            RemoteJobError::Terminal(_) => "remote job ended and cannot be resumed",
            RemoteJobError::Retryable(_) => "remote job remains recorded and can be resumed",
        };
        return Err(format!("{error}; {recovery}").into());
    }
    print_summary(project);
    Ok(())
}

fn run_effect(
    repository: FileProjectRepository,
    project_path: &std::path::Path,
    take: String,
    preset: EffectArgument,
) -> Result<(), Box<dyn Error>> {
    let mut project = repository.open(project_path)?;
    let take_id = if take == "latest" {
        project
            .takes()
            .last()
            .ok_or("project has no recorded takes")?
            .id()
            .to_owned()
    } else {
        take
    };
    let preset = preset.into();
    let rendered = render_take_preview(&project, &take_id, preset)?;
    project.set_take_render(&take_id, preset, rendered.relative_path)?;
    repository.save(&project)?;
    println!("{}", rendered.path.display());
    Ok(())
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
        repository.save(&project)?;
    }
    if let Some(key) = key {
        project.set_key_shift_semitones(key)?;
        repository.save(&project)?;
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
