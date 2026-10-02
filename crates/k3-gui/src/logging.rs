use std::{
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
    time::{SystemTime, UNIX_EPOCH},
};

use directories::ProjectDirs;

const MAX_LOG_BYTES: u64 = 1_048_576;
static LOG: OnceLock<DiagnosticLog> = OnceLock::new();

pub struct DiagnosticLog {
    path: PathBuf,
    writer: Mutex<File>,
}

impl DiagnosticLog {
    /// Initializes the process-wide rolling diagnostic log.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when the platform state directory or log file is unavailable.
    pub fn initialize() -> io::Result<&'static Self> {
        if let Some(log) = LOG.get() {
            return Ok(log);
        }
        let path = if let Some(path) = std::env::var_os("K3_GUI_LOG_PATH") {
            PathBuf::from(path)
        } else {
            let directories = ProjectDirs::from("", "", "k3").ok_or_else(|| {
                io::Error::new(io::ErrorKind::NotFound, "state directory unavailable")
            })?;
            directories
                .state_dir()
                .unwrap_or_else(|| directories.data_local_dir())
                .join("k3-gui.log")
        };
        let log = Self::open_at(path)?;
        let _ = LOG.set(log);
        LOG.get()
            .ok_or_else(|| io::Error::other("diagnostic log initialization failed"))
    }

    /// Opens a log at an explicit path, rotating a full previous log once.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when directories, rotation, or file opening fails.
    pub fn open_at(path: PathBuf) -> io::Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        if fs::metadata(&path).is_ok_and(|metadata| metadata.len() >= MAX_LOG_BYTES) {
            let previous = rotated_path(&path);
            if previous.exists() {
                fs::remove_file(&previous)?;
            }
            fs::rename(&path, previous)?;
        }
        let writer = OpenOptions::new().create(true).append(true).open(&path)?;
        Ok(Self {
            path,
            writer: Mutex::new(writer),
        })
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn record(&self, message: impl AsRef<str>) {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_secs());
        if let Ok(mut writer) = self.writer.lock() {
            let _ = writeln!(writer, "{timestamp} {}", message.as_ref());
            let _ = writer.flush();
        }
    }
}

fn rotated_path(path: &Path) -> PathBuf {
    PathBuf::from(format!("{}.1", path.display()))
}
