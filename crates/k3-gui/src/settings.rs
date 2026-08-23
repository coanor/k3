use std::{
    fs::{self, File},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::mpsc::{self, Sender},
    thread::{self, JoinHandle},
};

use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use thiserror::Error;

const SETTINGS_SCHEMA_VERSION: u32 = 1;

/// Serializes GUI settings writes on one background worker.
pub struct SettingsWriter {
    sender: Sender<WriterMessage>,
    worker: Option<JoinHandle<()>>,
}

/// Cloneable handle used by UI callbacks to queue settings snapshots.
#[derive(Clone)]
pub struct SettingsWriterHandle {
    sender: Sender<WriterMessage>,
}

enum WriterMessage {
    Save(GuiSettings),
    Stop,
}

impl SettingsWriter {
    /// Starts the settings writer thread and returns it with a callback-safe handle.
    ///
    /// # Errors
    ///
    /// Returns an error if the worker thread cannot be created.
    pub fn start() -> io::Result<(Self, SettingsWriterHandle)> {
        let (sender, receiver) = mpsc::channel();
        let worker = thread::Builder::new()
            .name("k3-gui-settings".into())
            .spawn(move || {
                while let Ok(message) = receiver.recv() {
                    match message {
                        WriterMessage::Save(settings) => {
                            if let Err(error) = settings.save() {
                                eprintln!("K3 GUI settings could not be saved: {error}");
                            }
                        }
                        WriterMessage::Stop => break,
                    }
                }
            })?;
        let handle = SettingsWriterHandle {
            sender: sender.clone(),
        };
        Ok((
            Self {
                sender,
                worker: Some(worker),
            },
            handle,
        ))
    }

    /// Flushes queued snapshots in order and stops the worker.
    pub fn finish(mut self) {
        let _ = self.sender.send(WriterMessage::Stop);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl SettingsWriterHandle {
    /// Queues a complete settings snapshot without blocking the caller on file I/O.
    pub fn persist(&self, settings: GuiSettings) {
        let _ = self.sender.send(WriterMessage::Save(settings));
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GuiSettings {
    pub schema_version: u32,
    pub projects_root: Option<PathBuf>,
    pub volume: f32,
    pub window: WindowSize,
    pub last_project_id: Option<String>,
}

impl Default for GuiSettings {
    fn default() -> Self {
        Self {
            schema_version: SETTINGS_SCHEMA_VERSION,
            projects_root: None,
            volume: 1.0,
            window: WindowSize::default(),
            last_project_id: None,
        }
    }
}

impl GuiSettings {
    /// Returns the platform-standard GUI settings path.
    ///
    /// # Errors
    ///
    /// Returns an error if the platform configuration directory is unavailable.
    pub fn path() -> Result<PathBuf, SettingsError> {
        if let Some(path) = std::env::var_os("K3_GUI_SETTINGS_PATH") {
            return Ok(PathBuf::from(path));
        }
        ProjectDirs::from("", "", "k3")
            .map(|directories| directories.config_dir().join("gui.json"))
            .ok_or(SettingsError::ConfigDirectoryUnavailable)
    }

    /// Loads GUI settings, returning defaults when no file exists yet.
    ///
    /// # Errors
    ///
    /// Returns an error for unreadable, invalid, or unsupported settings.
    pub fn load() -> Result<Self, SettingsError> {
        Self::load_from(&Self::path()?)
    }

    /// Loads GUI settings from an explicit path.
    ///
    /// # Errors
    ///
    /// Returns an error for unreadable, invalid, or unsupported settings.
    pub fn load_from(path: &Path) -> Result<Self, SettingsError> {
        match fs::read(path) {
            Ok(bytes) => {
                let settings: Self = serde_json::from_slice(&bytes)?;
                settings.validate()?;
                Ok(settings)
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(error.into()),
        }
    }

    /// Atomically saves settings to the platform-standard path.
    ///
    /// # Errors
    ///
    /// Returns an error when validation or persistence fails.
    pub fn save(&self) -> Result<(), SettingsError> {
        self.save_to(&Self::path()?)
    }

    /// Atomically saves settings to an explicit path.
    ///
    /// # Errors
    ///
    /// Returns an error when validation or persistence fails.
    pub fn save_to(&self, path: &Path) -> Result<(), SettingsError> {
        self.validate()?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let temporary = path.with_extension("json.tmp");
        let encoded = serde_json::to_vec_pretty(self)?;
        let mut file = File::create(&temporary)?;
        file.write_all(&encoded)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        fs::rename(temporary, path)?;
        Ok(())
    }

    fn validate(&self) -> Result<(), SettingsError> {
        if self.schema_version != SETTINGS_SCHEMA_VERSION {
            return Err(SettingsError::UnsupportedSchema(self.schema_version));
        }
        if !(0.0..=1.0).contains(&self.volume) {
            return Err(SettingsError::InvalidVolume(self.volume));
        }
        if self.window.width < 640 || self.window.height < 480 {
            return Err(SettingsError::InvalidWindowSize);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowSize {
    pub width: u32,
    pub height: u32,
}

impl Default for WindowSize {
    fn default() -> Self {
        Self {
            width: 1280,
            height: 800,
        }
    }
}

#[derive(Debug, Error)]
pub enum SettingsError {
    #[error("the platform configuration directory is unavailable")]
    ConfigDirectoryUnavailable,
    #[error("unsupported GUI settings schema: {0}")]
    UnsupportedSchema(u32),
    #[error("GUI volume must be between 0 and 1: {0}")]
    InvalidVolume(f32),
    #[error("GUI window must be at least 640 by 480")]
    InvalidWindowSize,
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}
