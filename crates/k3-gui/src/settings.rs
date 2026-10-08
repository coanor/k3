use std::{
    fs::{self, File},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::mpsc::{self, Sender},
    thread::{self, JoinHandle},
};

use directories::ProjectDirs;
use k3_core::VocalEffectPreset;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::ui_text;

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
                                eprintln!("{}", ui_text::settings_save_failed(&error));
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
    #[serde(default)]
    pub recording: RecordingSettings,
    #[serde(default)]
    pub separation_profile: GuiSeparationProfile,
    #[serde(default)]
    pub separation_quality_model: GuiQualityModel,
    #[serde(default)]
    pub separation_device: GuiSeparationDevice,
    #[serde(default)]
    pub language: GuiLanguage,
    pub window: WindowSize,
    pub last_project_id: Option<String>,
}

impl Default for GuiSettings {
    fn default() -> Self {
        Self {
            schema_version: SETTINGS_SCHEMA_VERSION,
            projects_root: None,
            volume: 1.0,
            recording: RecordingSettings::default(),
            separation_profile: GuiSeparationProfile::default(),
            separation_quality_model: GuiQualityModel::default(),
            separation_device: GuiSeparationDevice::default(),
            language: GuiLanguage::default(),
            window: WindowSize::default(),
            last_project_id: None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GuiSeparationProfile {
    Fast,
    Balanced,
    #[default]
    Quality,
    Compatible,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GuiSeparationDevice {
    #[default]
    Auto,
    Cpu,
    Gpu,
}

impl GuiSeparationDevice {
    #[must_use]
    pub const fn index(self) -> i32 {
        match self {
            Self::Auto => 0,
            Self::Cpu => 1,
            Self::Gpu => 2,
        }
    }

    #[must_use]
    pub const fn from_index(index: i32) -> Option<Self> {
        match index {
            0 => Some(Self::Auto),
            1 => Some(Self::Cpu),
            2 => Some(Self::Gpu),
            _ => None,
        }
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Cpu => "cpu",
            Self::Gpu => "gpu",
        }
    }
}

impl GuiSeparationProfile {
    #[must_use]
    pub fn index(self) -> i32 {
        match self {
            Self::Fast => 0,
            Self::Balanced => 1,
            Self::Quality => 2,
            Self::Compatible => 3,
        }
    }

    #[must_use]
    pub fn from_index(index: i32) -> Option<Self> {
        Some(match index {
            0 => Self::Fast,
            1 => Self::Balanced,
            2 => Self::Quality,
            3 => Self::Compatible,
            _ => return None,
        })
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GuiQualityModel {
    #[default]
    RuntimeDefault,
    KimVocal2,
    BsRoformer1297,
}

impl GuiQualityModel {
    #[must_use]
    pub fn index(self) -> i32 {
        match self {
            Self::RuntimeDefault => 0,
            Self::KimVocal2 => 1,
            Self::BsRoformer1297 => 2,
        }
    }

    #[must_use]
    pub fn from_index(index: i32) -> Option<Self> {
        Some(match index {
            0 => Self::RuntimeDefault,
            1 => Self::KimVocal2,
            2 => Self::BsRoformer1297,
            _ => return None,
        })
    }

    #[must_use]
    pub fn model_id(self) -> Option<&'static str> {
        match self {
            Self::RuntimeDefault => None,
            Self::KimVocal2 => Some("mel-band-roformer-kim-vocal-2"),
            Self::BsRoformer1297 => Some("bs-roformer-viperx-1297"),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum GuiLanguage {
    #[default]
    #[serde(rename = "en")]
    English,
    #[serde(rename = "zh-Hans")]
    SimplifiedChinese,
    #[serde(rename = "zh-Hant")]
    TraditionalChinese,
}

impl GuiLanguage {
    #[must_use]
    pub fn index(self) -> i32 {
        match self {
            Self::SimplifiedChinese => 0,
            Self::English => 1,
            Self::TraditionalChinese => 2,
        }
    }

    #[must_use]
    pub fn from_index(index: i32) -> Option<Self> {
        Some(match index {
            0 => Self::SimplifiedChinese,
            1 => Self::English,
            2 => Self::TraditionalChinese,
            _ => return None,
        })
    }

    #[must_use]
    pub fn locale(self) -> &'static str {
        match self {
            Self::SimplifiedChinese => "zh-Hans",
            Self::English => "en",
            Self::TraditionalChinese => "zh-Hant",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordingSettings {
    #[serde(default)]
    pub default_effect: VocalEffectPreset,
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
        let path = Self::path()?;
        let first_use = !path.exists();
        let mut settings = Self::load_from(&path)?;
        if first_use
            && let Ok(script) = crate::separation::bundled_script()
            && let Some(root) = script.parent()
        {
            let candidates = [
                (root.join("config.json"), false),
                (root.join("runtime/python/k3-hardware.json"), true),
                (root.join(".venv-separator/k3-hardware.json"), true),
            ];
            for (candidate, requires_schema) in candidates {
                if let Ok(bytes) = fs::read(candidate)
                    && let Ok(plan) = serde_json::from_slice::<serde_json::Value>(&bytes)
                    && (!requires_schema || plan["schema_version"] == 1)
                {
                    if !requires_schema
                        && let Ok(device) =
                            serde_json::from_value(plan["separation"]["device"].clone())
                    {
                        settings.separation_device = device;
                    }
                    if let Ok(profile) =
                        serde_json::from_value(plan["separation"]["profile"].clone())
                    {
                        settings.separation_profile = if requires_schema
                            && settings.separation_device == GuiSeparationDevice::Cpu
                        {
                            GuiSeparationProfile::Fast
                        } else {
                            profile
                        };
                        break;
                    }
                }
            }
        }
        Ok(settings)
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
