//! UI-independent application workflows shared by K3 frontends.

use std::{fs, io, ops::Range, path::PathBuf, time::Duration};

pub use k3_core::ProjectRevision;
use k3_core::{
    FileProjectRepository, LyricsTimeline, Project, ProjectError, ProjectRepository,
    SeparationState,
};
use thiserror::Error;
use uuid::Uuid;

mod audio_config;
mod effects;
mod lyrics_download;
mod mix;
mod playback;
mod recorder;
mod recording;
mod rodio_backend;
mod rodio_player;
mod session_playback;

pub use lyrics_download::{
    LyricsChoice, LyricsProgress, LyricsSaved, LyricsSearch, default_lyrics_query,
    default_project_lyrics_query, find_lyrics_again, find_missing_lyrics, find_project_lyrics,
    save_lyrics_choice, save_project_lyrics,
};
pub use mix::{render_take_mix, render_take_preview};
pub use playback::{
    AudioCommand, AudioSnapshot, PlaybackBackend, PlaybackCommand, PlaybackEngine, PlaybackService,
    PlaybackServiceError, PlaybackSnapshot, PlaybackStatus,
};
pub use recorder::{
    AudioRecorder, RecordingSummary, RecordingTimelineAnchor, place_recording_on_timeline,
};
pub use recording::{GuiRecordingController, GuiRecordingResult, GuiRecordingStarted};
pub use rodio_backend::RodioBackend;
pub use rodio_player::{AudioPlayer, MonitorControl, MonitorTap};
pub use session_playback::{SessionPlayback, SessionTrackKind};

/// The synchronized lyric rows a frontend should present around the playback position.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LyricWindow {
    pub range: Range<usize>,
    pub current: Option<usize>,
}

/// A short cue for the next lyric line, capped at three seconds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LyricCountdown {
    pub index: usize,
    pub seconds: u8,
}

/// Keeps a small amount of lyric history while reserving most rows for upcoming lines.
#[must_use]
pub fn lyric_window(
    timeline: &LyricsTimeline,
    position: Duration,
    visible_rows: usize,
) -> LyricWindow {
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

/// Returns a visual countdown during the final three seconds before the next lyric.
///
/// Intervals shorter than one second are ignored so rapid lyrics do not flicker continuously.
#[must_use]
pub fn lyric_countdown(timeline: &LyricsTimeline, position: Duration) -> Option<LyricCountdown> {
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
        seconds: u8::try_from(seconds).ok()?,
    })
}

/// A project row ready for presentation by any K3 frontend.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectSummary {
    pub id: Uuid,
    pub title: String,
    pub path: PathBuf,
    pub source_available: bool,
    pub document_revision: Option<ProjectRevision>,
}

/// Discovers durable K3 projects without exposing repository details to frontends.
pub struct ProjectLibrary;

impl ProjectLibrary {
    /// Scans one projects root and returns valid projects in stable title order.
    ///
    /// Directories that do not contain a readable K3 project are ignored. A project whose
    /// source media has disappeared remains visible and is marked unavailable.
    ///
    /// # Errors
    ///
    /// Returns an error when the root is not a directory or cannot be read.
    pub fn scan(root: &std::path::Path) -> Result<Vec<ProjectSummary>, LibraryError> {
        if !root.is_dir() {
            return Err(LibraryError::NotDirectory(root.to_path_buf()));
        }
        let repository = FileProjectRepository;
        let mut projects = Vec::new();
        for entry in fs::read_dir(root)? {
            let path = entry?.path();
            let Ok(project) = repository.open(&path) else {
                continue;
            };
            projects.push(ProjectSummary {
                id: project.id(),
                title: project.title().to_owned(),
                source_available: project.source_path().is_file(),
                document_revision: project.document_revision().cloned(),
                path,
            });
        }
        projects.sort_by(|left, right| {
            left.title
                .to_lowercase()
                .cmp(&right.title.to_lowercase())
                .then_with(|| left.id.cmp(&right.id))
        });
        Ok(projects)
    }

    /// Filters a previously scanned library by a Unicode title substring.
    #[must_use]
    pub fn filter<'a>(projects: &'a [ProjectSummary], query: &str) -> Vec<&'a ProjectSummary> {
        let query = query.trim().to_lowercase();
        projects
            .iter()
            .filter(|project| query.is_empty() || project.title.to_lowercase().contains(&query))
            .collect()
    }
}

/// Audio sources a frontend may select for project playback.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrackKind {
    Original,
    Accompaniment,
    Vocals,
    Take,
}

impl TrackKind {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Original => "Original",
            Self::Accompaniment => "Accompaniment",
            Self::Vocals => "Vocals",
            Self::Take => "Take",
        }
    }
}

/// One selectable project track, including unavailable tracks so a UI can explain them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectTrack {
    pub kind: TrackKind,
    pub path: Option<PathBuf>,
}

impl ProjectTrack {
    #[must_use]
    pub fn available(&self) -> bool {
        self.path.as_ref().is_some_and(|path| path.is_file())
    }
}

/// Project data prepared for playback without exposing JSON or repository details.
#[derive(Clone, Debug)]
pub struct LoadedProject {
    pub id: Uuid,
    pub title: String,
    pub root: PathBuf,
    pub tracks: Vec<ProjectTrack>,
    pub lyrics: Option<LyricsTimeline>,
    pub key_shift_semitones: i8,
    pub document_revision: Option<ProjectRevision>,
}

impl LoadedProject {
    /// Opens one durable project and resolves the media needed for presentation and playback.
    ///
    /// # Errors
    ///
    /// Returns an error when the project, lyrics, or referenced paths cannot be read.
    pub fn open(root: &std::path::Path) -> Result<Self, LoadProjectError> {
        let project = FileProjectRepository.open(root)?;
        Ok(Self::from_project_with_lyrics(&project)?)
    }

    pub(crate) fn from_project_with_lyrics(project: &Project) -> Result<Self, io::Error> {
        let mut loaded = Self::from_project(project);
        loaded.lyrics = project
            .lyrics()
            .map(|lyrics| lyrics.resolve(project.root()))
            .filter(|path| path.is_file())
            .map(fs::read_to_string)
            .transpose()?
            .map(|text| LyricsTimeline::parse(&text));
        Ok(loaded)
    }

    /// Builds playback data from an already loaded project without another repository read.
    #[must_use]
    pub fn from_project(project: &Project) -> Self {
        let mut tracks = vec![ProjectTrack {
            kind: TrackKind::Original,
            path: Some(project.source_path()),
        }];
        let (accompaniment, vocals) = match project.separation() {
            SeparationState::Ready(manifest) => (
                Some(manifest.accompaniment.resolve(project.root())),
                Some(manifest.vocals.resolve(project.root())),
            ),
            _ => (None, None),
        };
        tracks.push(ProjectTrack {
            kind: TrackKind::Accompaniment,
            path: accompaniment,
        });
        tracks.push(ProjectTrack {
            kind: TrackKind::Vocals,
            path: vocals,
        });
        if let Some(path) = project
            .takes()
            .last()
            .map(|take| take.mix_audio().unwrap_or_else(|| take.dry_audio()))
            .map(|path| path.resolve(project.root()))
        {
            tracks.push(ProjectTrack {
                kind: TrackKind::Take,
                path: Some(path),
            });
        }
        Self {
            id: project.id(),
            title: project.title().to_owned(),
            root: project.root().to_path_buf(),
            tracks,
            lyrics: None,
            key_shift_semitones: project.key_shift_semitones(),
            document_revision: project.document_revision().cloned(),
        }
    }

    #[must_use]
    pub fn track(&self, kind: TrackKind) -> Option<&ProjectTrack> {
        self.tracks.iter().find(|track| track.kind == kind)
    }

    #[must_use]
    pub fn default_track(&self) -> Option<TrackKind> {
        [TrackKind::Accompaniment, TrackKind::Original]
            .into_iter()
            .find(|kind| self.track(*kind).is_some_and(ProjectTrack::available))
    }
}

#[derive(Debug, Error)]
pub enum LibraryError {
    #[error("projects root is not a readable directory: {0}")]
    NotDirectory(PathBuf),
    #[error(transparent)]
    Io(#[from] io::Error),
}

#[derive(Debug, Error)]
pub enum LoadProjectError {
    #[error(transparent)]
    Project(#[from] ProjectError),
    #[error(transparent)]
    Io(#[from] io::Error),
}
