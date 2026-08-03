use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use k3_core::{
    CheckpointSha256, ModelProvenance, ProjectPath, SeparationFailure, SeparationManifest,
    SeparationProfile, StemSeparator,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug)]
pub struct PythonSeparatorConfig {
    pub worker: PathBuf,
    pub model_dir: Option<PathBuf>,
    pub project_root: PathBuf,
    pub model_id: Option<String>,
    pub overwrite: bool,
    pub segment_size: Option<u32>,
    pub autocast: bool,
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
        let mut command = Command::new(&self.config.worker);
        if let Some(model_dir) = &self.config.model_dir {
            command.arg("--model-dir").arg(model_dir);
        }
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
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
                "separation worker exited with status {}",
                output.status
            ));
        }
        let stdout = String::from_utf8(output.stdout)
            .map_err(|error| format!("worker response is not UTF-8: {error}"))?;
        let response: WorkerResponse = serde_json::from_str(stdout.trim())
            .map_err(|error| format!("invalid worker response: {error}"))?;
        if !response.ok {
            let error = response.error.unwrap_or(WorkerError {
                code: "unknown".into(),
                message: "worker failed without an error body".into(),
            });
            return Err(format!("{}: {}", error.code, error.message));
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
        Ok(SeparationManifest {
            vocals: ProjectPath::new("stems/vocals.wav").map_err(|error| error.to_string())?,
            accompaniment: ProjectPath::new("stems/accompaniment.wav")
                .map_err(|error| error.to_string())?,
            provenance: ModelProvenance {
                provider: result.provenance.provider,
                architecture: result.provenance.architecture,
                checkpoint_id: result.provenance.checkpoint_id,
                checkpoint_sha256: CheckpointSha256::new(result.provenance.checkpoint_sha256)
                    .map_err(|error| error.to_string())?,
                profile: requested_profile,
            },
        })
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
    provenance: WorkerProvenance,
}

#[derive(Debug, Deserialize)]
struct WorkerProvenance {
    provider: String,
    architecture: String,
    checkpoint_id: String,
    checkpoint_sha256: String,
    profile: SeparationProfile,
}

#[derive(Debug, Deserialize)]
struct WorkerError {
    code: String,
    message: String,
}
