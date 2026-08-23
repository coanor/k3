//! Domain modules for K3.

mod lyrics;
mod project;
mod recording;
mod separation;

pub use lyrics::{LyricsLine, LyricsTimeline};
pub use project::{
    BackingVocalModelProvenance, CheckpointSha256, CreateProject, FileProjectRepository,
    ModelProvenance, Project, ProjectError, ProjectMutation, ProjectPath, ProjectRepository,
    SeparationManifest, SeparationProfile, SeparationState, Take, VocalEffectPreset,
};
pub use recording::{RecordingError, RecordingSession, RecordingState};
pub use separation::{SeparationFailure, SongPreparation, StemSeparator};
