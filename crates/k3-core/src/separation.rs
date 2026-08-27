use std::path::Path;

use thiserror::Error;

use crate::{Project, SeparationManifest, SeparationProfile, SeparationState};

/// Adapter seam for a local model worker.
pub trait StemSeparator {
    /// Separates a local song using the requested quality profile.
    ///
    /// # Errors
    ///
    /// Returns a worker or model-specific failure without mutating the project.
    fn separate(
        &mut self,
        input: &Path,
        profile: SeparationProfile,
    ) -> Result<SeparationManifest, SeparationFailure>;
}

/// Applies separation results to a project without knowing the model runtime.
#[derive(Debug)]
pub struct SongPreparation<S> {
    separator: S,
}

impl<S: StemSeparator> SongPreparation<S> {
    #[must_use]
    pub fn new(separator: S) -> Self {
        Self { separator }
    }

    /// Runs separation and records either a validated manifest or a failure.
    ///
    /// # Errors
    ///
    /// Returns the worker failure or an invalid-manifest error after updating project state.
    pub fn prepare(
        &mut self,
        project: &mut Project,
        profile: SeparationProfile,
    ) -> Result<(), SeparationFailure> {
        if !matches!(project.separation(), SeparationState::NotRequested) {
            return Err(SeparationFailure::InvalidState(format!(
                "cannot prepare project while separation is {:?}",
                project.separation()
            )));
        }
        let result = self
            .separator
            .separate(&project.source_path(), profile)
            .and_then(|manifest| {
                validate_manifest(&manifest)?;
                Ok(manifest)
            });
        match result {
            Ok(manifest) => {
                project.set_separation(SeparationState::Ready(manifest));
                Ok(())
            }
            Err(error) => {
                project.set_separation(SeparationState::Failed {
                    message: error.detail().to_owned(),
                });
                Err(error)
            }
        }
    }

    /// Runs separation again while preserving the previous ready state on failure.
    ///
    /// # Errors
    ///
    /// Returns the worker failure or an invalid-manifest error. A running project
    /// cannot be restarted, and a failed retry restores the state observed before
    /// the retry began.
    pub fn reprepare(
        &mut self,
        project: &mut Project,
        profile: SeparationProfile,
    ) -> Result<(), SeparationFailure> {
        if matches!(project.separation(), SeparationState::NotRequested) {
            return self.prepare(project, profile);
        }
        let previous = project.separation().clone();
        let result = self
            .separator
            .separate(&project.source_path(), profile)
            .and_then(|manifest| {
                validate_manifest(&manifest)?;
                Ok(manifest)
            });
        match result {
            Ok(manifest) => {
                project.set_separation(SeparationState::Ready(manifest));
                Ok(())
            }
            Err(error) => {
                project.set_separation(previous);
                Err(error)
            }
        }
    }
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum SeparationFailure {
    #[error("separation worker failed: {0}")]
    Worker(String),
    #[error("invalid separation manifest: {0}")]
    InvalidManifest(String),
    #[error("invalid separation state: {0}")]
    InvalidState(String),
}

impl SeparationFailure {
    fn detail(&self) -> &str {
        match self {
            Self::Worker(message)
            | Self::InvalidManifest(message)
            | Self::InvalidState(message) => message,
        }
    }
}

fn validate_manifest(manifest: &SeparationManifest) -> Result<(), SeparationFailure> {
    manifest
        .validate()
        .map_err(SeparationFailure::InvalidManifest)
}
