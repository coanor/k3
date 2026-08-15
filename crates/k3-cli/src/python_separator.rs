use std::{
    env, fs,
    io::{self, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use k3_core::{
    BackingVocalModelProvenance, CheckpointSha256, ModelProvenance, ProjectPath, SeparationFailure,
    SeparationManifest, SeparationProfile, StemSeparator,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug)]
pub struct PythonSeparatorConfig {
    pub worker: PathBuf,
    pub model_dir: Option<PathBuf>,
    pub project_root: PathBuf,
    pub log_path: PathBuf,
    pub model_id: Option<String>,
    pub overwrite: bool,
    pub segment_size: Option<u32>,
    pub autocast: bool,
    pub preserve_backing_vocals: bool,
}

#[derive(Debug)]
pub struct PythonStemSeparator {
    config: PythonSeparatorConfig,
}

impl PythonStemSeparator {
    #[must_use]
    pub fn new(config: PythonSeparatorConfig) -> Self {
        Self { config }
    }

    fn invoke(
        &self,
        input: &Path,
        profile: SeparationProfile,
    ) -> Result<SeparationManifest, String> {
        let output_dir = self.config.project_root.join("stems");
        if let Some(parent) = self.config.log_path.parent() {
            fs::create_dir_all(parent).map_err(|error| {
                format!(
                    "cannot create K3 log directory {}: {error}",
                    parent.display()
                )
            })?;
        }
        let stderr_log = fs::File::create(&self.config.log_path).map_err(|error| {
            format!(
                "cannot create separation log {}: {error}",
                self.config.log_path.display()
            )
        })?;
        let mut command = Command::new(&self.config.worker);
        if let Some(model_dir) = &self.config.model_dir {
            command.arg("--model-dir").arg(model_dir);
        }
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::from(stderr_log))
            .spawn()
            .map_err(|error| {
                format!(
                    "cannot start separation worker {}: {error}",
                    self.config.worker.display()
                )
            })?;

        let request = WorkerRequest {
            id: "k3-separate",
            method: "separate",
            params: WorkerParameters {
                input_path: input,
                output_dir: &output_dir,
                profile,
                model_id: self.config.model_id.as_deref(),
                overwrite: self.config.overwrite,
                preserve_backing_vocals: self.config.preserve_backing_vocals,
                options: WorkerOptions {
                    autocast: self.config.autocast,
                    segment_size: self.config.segment_size,
                },
            },
        };
        {
            let mut stdin = child
                .stdin
                .take()
                .ok_or_else(|| "separation worker stdin is unavailable".to_owned())?;
            serde_json::to_writer(&mut stdin, &request)
                .map_err(|error| format!("cannot encode worker request: {error}"))?;
            stdin
                .write_all(b"\n")
                .map_err(|error| format!("cannot write worker request: {error}"))?;
        }

        let output = child
            .wait_with_output()
            .map_err(|error| format!("cannot wait for separation worker: {error}"))?;
        if !output.status.success() {
            return Err(format!(
                "separation worker exited with status {}{}",
                output.status,
                stderr_diagnostics(&self.config.log_path)
            ));
        }
        let stdout = String::from_utf8(output.stdout)
            .map_err(|error| format!("worker response is not UTF-8: {error}"))?;
        let response: WorkerResponse = serde_json::from_str(stdout.trim()).map_err(|error| {
            format!(
                "invalid worker response: {error}{}",
                stderr_diagnostics(&self.config.log_path)
            )
        })?;
        if !response.ok {
            let error = response.error.unwrap_or(WorkerError {
                code: "unknown".into(),
                message: "worker failed without an error body".into(),
            });
            return Err(format!(
                "{}: {}{}",
                error.code,
                error.message,
                stderr_diagnostics(&self.config.log_path)
            ));
        }
        let result = response
            .result
            .ok_or_else(|| "worker succeeded without a result".to_owned())?;
        self.to_manifest(result, profile)
    }

    fn to_manifest(
        &self,
        result: WorkerResult,
        requested_profile: SeparationProfile,
    ) -> Result<SeparationManifest, String> {
        if result.provenance.profile != requested_profile {
            return Err("worker returned a different separation profile".into());
        }
        validate_output_path(
            &result.vocals,
            &self.config.project_root.join("stems/vocals.wav"),
            "vocals",
        )?;
        validate_output_path(
            &result.accompaniment,
            &self.config.project_root.join("stems/accompaniment.wav"),
            "accompaniment",
        )?;
        let backing_vocals = match (
            self.config.preserve_backing_vocals,
            result.backing_vocals,
            result.provenance.backing_vocals_model,
        ) {
            (true, Some(path), Some(model)) => {
                validate_output_path(
                    &path,
                    &self.config.project_root.join("stems/backing-vocals.wav"),
                    "backing vocals",
                )?;
                Some((
                    ProjectPath::new("stems/backing-vocals.wav")
                        .map_err(|error| error.to_string())?,
                    Box::new(BackingVocalModelProvenance {
                        provider: model.provider,
                        architecture: model.architecture,
                        checkpoint_id: model.checkpoint_id,
                        checkpoint_sha256: CheckpointSha256::new(model.checkpoint_sha256)
                            .map_err(|error| error.to_string())?,
                    }),
                ))
            }
            (true, _, _) => {
                return Err(
                    "worker omitted backing-vocal output or model provenance for preserve mode"
                        .into(),
                );
            }
            (false, None, None) => None,
            (false, _, _) => {
                return Err("worker returned unexpected backing-vocal data".into());
            }
        };
        let (backing_vocals, backing_vocals_model) =
            backing_vocals.map_or((None, None), |(path, model)| (Some(path), Some(model)));
        Ok(SeparationManifest {
            vocals: ProjectPath::new("stems/vocals.wav").map_err(|error| error.to_string())?,
            accompaniment: ProjectPath::new("stems/accompaniment.wav")
                .map_err(|error| error.to_string())?,
            backing_vocals,
            provenance: ModelProvenance {
                provider: result.provenance.provider,
                architecture: result.provenance.architecture,
                checkpoint_id: result.provenance.checkpoint_id,
                checkpoint_sha256: CheckpointSha256::new(result.provenance.checkpoint_sha256)
                    .map_err(|error| error.to_string())?,
                profile: requested_profile,
                backing_vocals_model,
            },
        })
    }
}

pub fn separation_log_path() -> io::Result<PathBuf> {
    if let Some(directory) = env::var_os("K3_LOG_DIR").filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(directory).join("separate.log"));
    }
    if let Some(state_home) = env::var_os("XDG_STATE_HOME").filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(state_home)
            .join("k3")
            .join("logs")
            .join("separate.log"));
    }
    #[cfg(windows)]
    if let Some(local_app_data) = env::var_os("LOCALAPPDATA").filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(local_app_data)
            .join("k3")
            .join("logs")
            .join("separate.log"));
    }
    if let Some(home) = env::var_os("HOME").filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(home)
            .join(".local")
            .join("state")
            .join("k3")
            .join("logs")
            .join("separate.log"));
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        "cannot locate the K3 log directory; set K3_LOG_DIR",
    ))
}

fn stderr_diagnostics(log_path: &Path) -> String {
    const LIMIT: usize = 2_048;
    let mut file = match fs::File::open(log_path) {
        Ok(file) => file,
        Err(error) => return format!("; cannot read worker log {}: {error}", log_path.display()),
    };
    let length = file.metadata().map_or(0, |metadata| metadata.len());
    let start = length.saturating_sub(LIMIT as u64);
    if file.seek(SeekFrom::Start(start)).is_err() {
        return format!("; full log: {}", log_path.display());
    }
    let mut stderr = Vec::with_capacity(LIMIT);
    if file.read_to_end(&mut stderr).is_err() {
        return format!("; full log: {}", log_path.display());
    }
    let text = String::from_utf8_lossy(&stderr);
    let sanitized = text
        .chars()
        .filter(|character| !character.is_control() || matches!(character, '\n' | '\r' | '\t'))
        .collect::<String>();
    let diagnostics = sanitized.trim();
    if diagnostics.is_empty() {
        format!("; full log: {}", log_path.display())
    } else if start == 0_u64 {
        format!(
            "; full log: {}; worker stderr: {diagnostics}",
            log_path.display(),
        )
    } else {
        format!(
            "; full log: {}; worker stderr (tail): {diagnostics}",
            log_path.display(),
        )
    }
}

impl StemSeparator for PythonStemSeparator {
    fn separate(
        &mut self,
        input: &Path,
        profile: SeparationProfile,
    ) -> Result<SeparationManifest, SeparationFailure> {
        self.invoke(input, profile)
            .map_err(SeparationFailure::Worker)
    }
}

fn validate_output_path(actual: &Path, expected: &Path, label: &str) -> Result<(), String> {
    let actual = fs::canonicalize(actual)
        .map_err(|error| format!("cannot resolve worker {label} output: {error}"))?;
    let expected = fs::canonicalize(expected)
        .map_err(|error| format!("cannot resolve expected {label} output: {error}"))?;
    if actual != expected {
        return Err(format!(
            "worker {label} output is outside the project stems directory: {}",
            actual.display()
        ));
    }
    if fs::metadata(&actual)
        .map_err(|error| format!("cannot inspect worker {label} output: {error}"))?
        .len()
        == 0
    {
        return Err(format!("worker {label} output is empty"));
    }
    Ok(())
}

#[derive(Debug, Serialize)]
struct WorkerRequest<'a> {
    id: &'static str,
    method: &'static str,
    params: WorkerParameters<'a>,
}

#[derive(Debug, Serialize)]
struct WorkerParameters<'a> {
    input_path: &'a Path,
    output_dir: &'a Path,
    profile: SeparationProfile,
    #[serde(skip_serializing_if = "Option::is_none")]
    model_id: Option<&'a str>,
    overwrite: bool,
    preserve_backing_vocals: bool,
    options: WorkerOptions,
}

#[derive(Debug, Serialize)]
struct WorkerOptions {
    autocast: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    segment_size: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct WorkerResponse {
    ok: bool,
    result: Option<WorkerResult>,
    error: Option<WorkerError>,
}

#[derive(Debug, Deserialize)]
struct WorkerResult {
    vocals: PathBuf,
    accompaniment: PathBuf,
    backing_vocals: Option<PathBuf>,
    provenance: WorkerProvenance,
}

#[derive(Debug, Deserialize)]
struct WorkerProvenance {
    provider: String,
    architecture: String,
    checkpoint_id: String,
    checkpoint_sha256: String,
    profile: SeparationProfile,
    backing_vocals_model: Option<WorkerBackingVocalProvenance>,
}

#[derive(Debug, Deserialize)]
struct WorkerBackingVocalProvenance {
    provider: String,
    architecture: String,
    checkpoint_id: String,
    checkpoint_sha256: String,
}

#[derive(Debug, Deserialize)]
struct WorkerError {
    code: String,
    message: String,
}
