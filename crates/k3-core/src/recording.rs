use thiserror::Error;

use crate::{Project, Take};

/// Observable state of a dry-vocal recording session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecordingState {
    Idle,
    Armed,
    Recording,
}

/// Coordinates legal recording transitions and owns the evolving project.
#[derive(Debug)]
pub struct RecordingSession {
    project: Project,
    state: RecordingState,
}

impl RecordingSession {
    #[must_use]
    pub fn new(project: Project) -> Self {
        Self {
            project,
            state: RecordingState::Idle,
        }
    }

    #[must_use]
    pub fn project(&self) -> &Project {
        &self.project
    }

    #[must_use]
    pub fn state(&self) -> RecordingState {
        self.state
    }

    /// Arms an idle recording session.
    ///
    /// # Errors
    ///
    /// Returns [`RecordingError::InvalidTransition`] unless the session is idle.
    pub fn arm(&mut self) -> Result<(), RecordingError> {
        self.transition(RecordingState::Idle, RecordingState::Armed, "arm")
    }

    /// Starts an armed recording session.
    ///
    /// # Errors
    ///
    /// Returns [`RecordingError::InvalidTransition`] unless the session is armed.
    pub fn start(&mut self) -> Result<(), RecordingError> {
        self.transition(RecordingState::Armed, RecordingState::Recording, "start")
    }

    /// Cancels an armed recording session.
    ///
    /// # Errors
    ///
    /// Returns [`RecordingError::InvalidTransition`] unless the session is armed.
    pub fn cancel(&mut self) -> Result<(), RecordingError> {
        self.transition(RecordingState::Armed, RecordingState::Idle, "cancel")
    }

    /// Stops recording and attaches an immutable dry take to the project.
    ///
    /// # Errors
    ///
    /// Returns an error unless the session is recording and the take is below `takes/`.
    pub fn stop(&mut self, take: Take) -> Result<(), RecordingError> {
        if self.state != RecordingState::Recording {
            return Err(RecordingError::InvalidTransition {
                operation: "stop",
                state: self.state,
            });
        }
        if !take.dry_audio().as_str().starts_with("takes/") {
            return Err(RecordingError::InvalidTakePath(
                take.dry_audio().as_str().to_owned(),
            ));
        }
        if let Some(mix_audio) = take.mix_audio()
            && !mix_audio.as_str().starts_with("takes/")
        {
            return Err(RecordingError::InvalidTakePath(
                mix_audio.as_str().to_owned(),
            ));
        }
        self.project.add_take(take);
        self.state = RecordingState::Idle;
        Ok(())
    }

    /// Aborts an active recording without attaching a take.
    ///
    /// # Errors
    ///
    /// Returns [`RecordingError::InvalidTransition`] unless the session is recording.
    pub fn abort(&mut self) -> Result<(), RecordingError> {
        self.transition(RecordingState::Recording, RecordingState::Idle, "abort")
    }

    fn transition(
        &mut self,
        expected: RecordingState,
        next: RecordingState,
        operation: &'static str,
    ) -> Result<(), RecordingError> {
        if self.state != expected {
            return Err(RecordingError::InvalidTransition {
                operation,
                state: self.state,
            });
        }
        self.state = next;
        Ok(())
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum RecordingError {
    #[error("cannot {operation} while recording session is {state:?}")]
    InvalidTransition {
        operation: &'static str,
        state: RecordingState,
    },
    #[error("dry take must be stored below takes/: {0}")]
    InvalidTakePath(String),
}
