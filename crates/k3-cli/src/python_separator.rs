use std::{
    env, fs,
    io::{self, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

use k3_core::{
    BackingVocalModelProvenance, CheckpointSha256, ModelProvenance, Project, ProjectPath,
    SeparationFailure, SeparationManifest, SeparationProfile, SeparationState, StemSeparator,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum DeviceSelection {
    #[default]
    Auto,
    Cpu,
    Gpu,
}

impl DeviceSelection {
    pub fn from_environment(default: Self) -> Result<Self, String> {
        match env::var("K3_DEVICE") {
            Ok(value) => <Self as clap::ValueEnum>::from_str(&value, false),
            Err(env::VarError::NotPresent) => Ok(default),
            Err(error) => Err(format!("cannot read K3_DEVICE: {error}")),
        }
    }
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Cpu => "cpu",
            Self::Gpu => "gpu",
        }
    }
}

#[derive(Clone, Debug)]
pub struct PythonSeparatorConfig {
    pub device: DeviceSelection,
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
    cancellation: Arc<AtomicBool>,
}

impl PythonStemSeparator {
    #[must_use]
    pub fn new(config: PythonSeparatorConfig) -> Self {
        Self {
            config,
            cancellation: Arc::new(AtomicBool::new(false)),
        }
    }

    #[must_use]
    pub(crate) fn with_cancellation(
        config: PythonSeparatorConfig,
        cancellation: Arc<AtomicBool>,
    ) -> Self {
        Self {
            config,
            cancellation,
        }
    }

    fn invoke(
        &self,
        input: &Path,
        profile: SeparationProfile,
    ) -> Result<SeparationManifest, String> {
        if self.cancellation.load(Ordering::Acquire) {
            return Err("separation cancelled".into());
        }
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
        let mut encoded_request = serde_json::to_vec(&request)
            .map_err(|error| format!("cannot encode worker request: {error}"))?;
        encoded_request.push(b'\n');

        let worker = resolve_worker(&self.config.worker);
        let mut command = Command::new(&worker);
        command.env("K3_DEVICE", self.config.device.as_str());
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

        let write_result = child.stdin.take().map_or_else(
            || Err("separation worker stdin is unavailable".to_owned()),
            |mut stdin| {
                stdin
                    .write_all(&encoded_request)
                    .map_err(|error| format!("cannot write worker request: {error}"))
            },
        );
        if let Err(error) = write_result {
            terminate_and_reap(&mut child);
            return Err(error);
        }

        let output = wait_for_output(&mut child, &self.cancellation)?;
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
        let vocals = validated_project_path(
            &result.vocals,
            &self.config.project_root.join("stems"),
            "vocals",
        )?;
        let accompaniment = validated_project_path(
            &result.accompaniment,
            &self.config.project_root.join("stems"),
            "accompaniment",
        )?;
        let backing_vocals = match (
            self.config.preserve_backing_vocals,
            result.backing_vocals,
            result.provenance.backing_vocals_model,
        ) {
            (true, Some(path), Some(model)) => {
                let project_path = validated_project_path(
                    &path,
                    &self.config.project_root.join("stems"),
                    "backing vocals",
                )?;
                Some((
                    project_path,
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
            vocals,
            accompaniment,
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

/// 默认 worker 优先使用当前发行包旁的原生入口；显式配置仍按原路径运行。
fn resolve_worker(worker: &Path) -> PathBuf {
    if worker == Path::new("k3-separator")
        && let Ok(executable) = env::current_exe()
        && let Some(root) = executable.parent()
    {
        let bundled = root.join(if cfg!(windows) {
            "k3-separator.exe"
        } else {
            "k3-separator"
        });
        let python = root.join(if cfg!(windows) {
            "runtime/python/python.exe"
        } else {
            "runtime/python/bin/python3"
        });
        if bundled.is_file() && python.is_file() {
            return bundled;
        }
    }
    worker.to_path_buf()
}

pub fn separation_output_paths(project: &Project) -> Vec<PathBuf> {
    let SeparationState::Ready(manifest) = project.separation() else {
        return Vec::new();
    };
    [&manifest.vocals, &manifest.accompaniment]
        .into_iter()
        .chain(manifest.backing_vocals.iter())
        .map(|path| path.resolve(project.root()))
        .collect()
}

pub fn cleanup_obsolete_outputs(project_root: &Path, obsolete: &[PathBuf], retained: &[PathBuf]) {
    let Ok(root) = project_root.canonicalize() else {
        return;
    };
    let stems = root.join("stems");
    for path in obsolete {
        if retained.contains(path) {
            continue;
        }
        let safe = path.strip_prefix(&stems).is_ok_and(|relative| {
            relative.components().count() > 0
                && relative
                    .components()
                    .all(|component| matches!(component, std::path::Component::Normal(_)))
        }) && path.parent().is_some_and(|parent| {
            parent
                .canonicalize()
                .is_ok_and(|parent| parent.starts_with(&stems))
        });
        if !safe {
            eprintln!(
                "Warning: skipped obsolete stem outside the project stems directory: {}",
                path.display()
            );
            continue;
        }
        if let Err(error) = fs::remove_file(path)
            && error.kind() != io::ErrorKind::NotFound
        {
            eprintln!(
                "Warning: could not remove obsolete stem {}: {error}",
                path.display()
            );
        }
    }
}

fn wait_for_output(child: &mut Child, cancellation: &AtomicBool) -> Result<Output, String> {
    let Some(mut stdout) = child.stdout.take() else {
        terminate_and_reap(child);
        return Err("separation worker stdout is unavailable".to_owned());
    };
    let stdout_reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout.read_to_end(&mut bytes).map(|_| bytes)
    });
    let status = loop {
        if cancellation.load(Ordering::Acquire) {
            terminate_and_reap(child);
            break Err("separation cancelled".into());
        }
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) => thread::sleep(Duration::from_millis(50)),
            Err(error) => {
                terminate_and_reap(child);
                break Err(format!("cannot wait for separation worker: {error}"));
            }
        }
    };
    let stdout = stdout_reader
        .join()
        .map_err(|_| "separation worker stdout reader panicked".to_owned())?
        .map_err(|error| format!("cannot read separation worker response: {error}"))?;
    let status = status?;
    Ok(Output {
        status,
        stdout,
        stderr: Vec::new(),
    })
}

fn terminate_and_reap(child: &mut Child) {
    if child.try_wait().ok().flatten().is_none() {
        let _ = child.kill();
        let _ = child.wait();
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

fn validated_project_path(
    actual: &Path,
    expected_directory: &Path,
    label: &str,
) -> Result<ProjectPath, String> {
    let actual = fs::canonicalize(actual)
        .map_err(|error| format!("cannot resolve worker {label} output: {error}"))?;
    let expected_directory = fs::canonicalize(expected_directory)
        .map_err(|error| format!("cannot resolve expected stems directory: {error}"))?;
    if actual.parent() != Some(expected_directory.as_path()) {
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
    let file_name = actual
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| format!("worker {label} output name is not valid UTF-8"))?;
    ProjectPath::new(format!("stems/{file_name}")).map_err(|error| error.to_string())
}

#[cfg(all(test, unix))]
mod tests {
    use super::{PythonSeparatorConfig, PythonStemSeparator, cleanup_obsolete_outputs};
    use k3_core::{SeparationProfile, StemSeparator};
    use std::{
        fs,
        os::unix::fs::PermissionsExt,
        process::Command,
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
        thread,
        time::{Duration, Instant},
    };

    #[test]
    fn obsolete_cleanup_keeps_files_outside_project_through_directory_links() {
        let sandbox = tempfile::tempdir().unwrap();
        let project = sandbox.path().join("project");
        let external = sandbox.path().join("external");
        fs::create_dir_all(project.join("stems")).unwrap();
        fs::create_dir(&external).unwrap();
        let sentinel = external.join("sentinel.wav");
        fs::write(&sentinel, b"must survive").unwrap();
        std::os::unix::fs::symlink(&external, project.join("stems/external")).unwrap();

        cleanup_obsolete_outputs(
            &project,
            &[project.join("stems/external/sentinel.wav")],
            &[],
        );

        assert_eq!(fs::read(&sentinel).unwrap(), b"must survive");
    }

    #[test]
    fn obsolete_cleanup_keeps_files_when_stems_directory_is_redirected() {
        let sandbox = tempfile::tempdir().unwrap();
        let project = sandbox.path().join("project");
        let external = sandbox.path().join("external");
        fs::create_dir(&project).unwrap();
        fs::create_dir(&external).unwrap();
        let sentinel = external.join("sentinel.wav");
        fs::write(&sentinel, b"must survive").unwrap();
        std::os::unix::fs::symlink(&external, project.join("stems")).unwrap();

        cleanup_obsolete_outputs(&project, &[project.join("stems/sentinel.wav")], &[]);

        assert_eq!(fs::read(&sentinel).unwrap(), b"must survive");
    }

    #[test]
    fn obsolete_cleanup_removes_only_unretained_stems_and_unlinks_file_links() {
        let sandbox = tempfile::tempdir().unwrap();
        let project = sandbox.path().join("project");
        fs::create_dir_all(project.join("stems")).unwrap();
        let obsolete = project.join("stems/obsolete.wav");
        let retained = project.join("stems/retained.wav");
        let external = sandbox.path().join("external.wav");
        let link = project.join("stems/link.wav");
        for path in [&obsolete, &retained, &external] {
            fs::write(path, b"audio").unwrap();
        }
        std::os::unix::fs::symlink(&external, &link).unwrap();

        cleanup_obsolete_outputs(
            &project,
            &[
                obsolete.clone(),
                retained.clone(),
                link.clone(),
                external.clone(),
            ],
            std::slice::from_ref(&retained),
        );

        assert!(!obsolete.exists());
        assert!(retained.exists());
        assert!(fs::symlink_metadata(link).is_err());
        assert_eq!(fs::read(external).unwrap(), b"audio");
    }

    #[test]
    fn cancellation_terminates_and_reaps_worker_process() {
        let sandbox = tempfile::tempdir().unwrap();
        let worker = sandbox.path().join("slow-worker");
        let pid_path = sandbox.path().join("worker.pid");
        fs::write(
            &worker,
            format!(
                "#!/usr/bin/env python3\nimport os\nimport pathlib\nimport sys\nimport time\nsys.stdin.readline()\npathlib.Path({pid_path:?}).write_text(str(os.getpid()), encoding='utf-8')\nwhile True:\n    time.sleep(1)\n"
            ),
        )
        .unwrap();
        let mut permissions = fs::metadata(&worker).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&worker, permissions).unwrap();
        let source = sandbox.path().join("song.wav");
        fs::write(&source, b"song").unwrap();
        let project_root = sandbox.path().join("project");
        fs::create_dir_all(&project_root).unwrap();
        let cancellation = Arc::new(AtomicBool::new(false));
        let separator_cancellation = cancellation.clone();
        let handle = thread::spawn(move || {
            let mut separator = PythonStemSeparator::with_cancellation(
                PythonSeparatorConfig {
                    device: super::DeviceSelection::Auto,
                    worker,
                    model_dir: None,
                    project_root: project_root.clone(),
                    log_path: project_root.join("separate.log"),
                    model_id: None,
                    overwrite: false,
                    segment_size: None,
                    autocast: true,
                    preserve_backing_vocals: true,
                },
                separator_cancellation,
            );
            separator.separate(&source, SeparationProfile::Quality)
        });

        let deadline = Instant::now() + Duration::from_secs(5);
        while !pid_path.exists() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        let pid = fs::read_to_string(&pid_path).expect("worker should report its pid");
        cancellation.store(true, Ordering::Release);
        let error = handle.join().unwrap().unwrap_err();

        assert!(error.to_string().contains("separation cancelled"));
        assert!(
            !Command::new("kill")
                .args(["-0", pid.trim()])
                .output()
                .unwrap()
                .status
                .success(),
            "worker process {pid} survived cancellation"
        );
    }
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
