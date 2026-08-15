mod audio;
mod audio_config;
mod effects;
mod library;
mod lyrics_download;
mod mix;
mod pitch;
mod python_separator;
mod recorder;
mod tui;

use std::{error::Error, path::PathBuf};

use clap::{Parser, Subcommand, ValueEnum};
use k3_core::{
    CreateProject, FileProjectRepository, Project, ProjectRepository, SeparationProfile,
    SongPreparation,
};

use crate::lyrics_download::{LyricsDownload, download_missing_lyrics};
use crate::mix::render_take_preview;
use crate::python_separator::{PythonSeparatorConfig, PythonStemSeparator, separation_log_path};

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
        /// project 没有本地同步歌词时，不查询 LRCLIB。
        #[arg(long)]
        no_lyrics_download: bool,
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
                log_path: separation_log_path()?,
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
        Command::Effect {
            project,
            take,
            preset,
        } => {
            let mut project = repository.open(&project)?;
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
        }
        Command::Tui {
            project,
            config,
            latency_ms,
            key,
            no_lyrics_download,
        } => {
            if let Some(config) = config {
                if latency_ms.is_some() || key.is_some() {
                    return Err("--latency-ms and --key require --project mode".into());
                }
                tui::open_library(library::LibraryConfig::load(&config)?)?;
            } else {
                open_tui(
                    repository,
                    project.as_deref().expect("clap requires project or config"),
                    latency_ms,
                    key,
                    no_lyrics_download,
                )?;
            }
        }
    }
    Ok(())
}

fn open_tui(
    repository: FileProjectRepository,
    project_path: &std::path::Path,
    latency_ms: Option<i32>,
    key: Option<i8>,
    no_lyrics_download: bool,
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
    let lyrics_message = if no_lyrics_download {
        None
    } else {
        match download_missing_lyrics(&mut project, &mut |progress| {
            eprintln!("{progress}");
        }) {
            Ok(LyricsDownload::AlreadyPresent) => None,
            Ok(LyricsDownload::Downloaded { track, artist }) => {
                repository.save(&project)?;
                Some(format!("已从 LRCLIB 下载歌词：{artist} - {track}"))
            }
            Ok(LyricsDownload::NotFound) => {
                Some("LRCLIB 未找到时长匹配的同步歌词；录音仍可继续".into())
            }
            Err(error) => Some(format!("自动下载歌词失败：{error}；录音仍可继续")),
        }
    };
    tui::open(project, lyrics_message)
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
