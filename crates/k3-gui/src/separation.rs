use std::{
    env,
    fs::{self, File},
    io::{BufReader, Read},
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    sync::mpsc::{self, RecvTimeoutError},
    thread,
    time::{Duration, Instant},
};

use crate::logging::DiagnosticLog;
use k3_core::{FileProjectRepository, ProjectRepository, SeparationState};
use serde::Deserialize;
use uuid::Uuid;

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ProgressPhase {
    Preparing,
    LoadingVocals,
    SeparatingVocals,
    LoadingBackingVocals,
    SeparatingBackingVocals,
    WritingAudio,
    SavingProject,
}

impl ProgressPhase {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Preparing => "Preparing separation…",
            Self::LoadingVocals => "Loading vocal model…",
            Self::SeparatingVocals => "Separating vocals",
            Self::LoadingBackingVocals => "Loading backing vocal model…",
            Self::SeparatingBackingVocals => "Separating backing vocals",
            Self::WritingAudio => "Writing audio tracks…",
            Self::SavingProject => "Saving project…",
        }
    }
}

#[derive(Debug)]
pub(crate) struct SeparationProgress {
    pub(crate) phase: ProgressPhase,
    pub(crate) fraction: Option<f32>,
    pub(crate) elapsed: Duration,
}

#[derive(Deserialize)]
struct ProgressDocument {
    schema_version: u32,
    phase: ProgressPhase,
    fraction: Option<f32>,
}

struct ProgressFile {
    directory: PathBuf,
    path: PathBuf,
}

impl ProgressFile {
    fn create(projects_root: &Path) -> std::io::Result<Self> {
        let directory = projects_root.join(format!(".k3-progress-{}", Uuid::new_v4()));
        fs::create_dir(&directory)?;
        Ok(Self {
            path: directory.join("progress.json"),
            directory,
        })
    }

    fn read(&self) -> Option<ProgressDocument> {
        let mut bytes = Vec::new();
        File::open(&self.path)
            .ok()?
            .take(4097)
            .read_to_end(&mut bytes)
            .ok()?;
        if bytes.len() > 4096 {
            return None;
        }
        let document: ProgressDocument = serde_json::from_slice(&bytes).ok()?;
        (document.schema_version == 1
            && document
                .fraction
                .is_none_or(|value| value.is_finite() && (0.0..=1.0).contains(&value)))
        .then_some(document)
    }
}

impl Drop for ProgressFile {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.directory);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Profile {
    Fast,
    Balanced,
    Quality,
    Compatible,
}

impl Profile {
    pub(crate) fn from_index(index: i32) -> Self {
        match index {
            0 => Self::Fast,
            1 => Self::Balanced,
            3 => Self::Compatible,
            _ => Self::Quality,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Fast => "fast",
            Self::Balanced => "balanced",
            Self::Quality => "quality",
            Self::Compatible => "compatible",
        }
    }
}

pub(crate) struct SeparationRequest {
    pub(crate) source: PathBuf,
    pub(crate) projects_root: PathBuf,
    pub(crate) profile: Profile,
    pub(crate) model_id: Option<&'static str>,
    pub(crate) allow_replace: bool,
}

pub(crate) fn destination(source: &Path, projects_root: &Path) -> Result<(PathBuf, bool), String> {
    if !source.is_file() {
        return Err(format!("Audio file is not readable: {}", source.display()));
    }
    if !projects_root.is_dir() {
        return Err(format!(
            "Projects folder is not readable: {}",
            projects_root.display()
        ));
    }
    let name = source
        .file_stem()
        .or_else(|| source.file_name())
        .filter(|name| !name.is_empty())
        .ok_or("The audio file needs a name")?;
    let project = projects_root.join(name);
    if project.exists() && !project.join("project.json").is_file() {
        return Err(format!(
            "Destination already exists but is not a K3 project: {}",
            project.display()
        ));
    }
    let exists = project.join("project.json").is_file();
    Ok((project, exists))
}

/// A cached download may resume an unfinished project only when it is the same audio.
pub(crate) fn retryable_unprepared_destination(source: &Path, projects_root: &Path) -> bool {
    let Ok((project_path, true)) = destination(source, projects_root) else {
        return false;
    };
    let Ok(project) = FileProjectRepository.open(&project_path) else {
        return false;
    };
    if !matches!(
        project.separation(),
        SeparationState::Failed { .. } | SeparationState::NotRequested
    ) {
        return false;
    }
    same_file_contents(source, &project.source_path()).unwrap_or(false)
}

fn same_file_contents(first: &Path, second: &Path) -> std::io::Result<bool> {
    if first.metadata()?.len() != second.metadata()?.len() {
        return Ok(false);
    }
    let mut first = BufReader::new(File::open(first)?);
    let mut second = BufReader::new(File::open(second)?);
    let mut left = vec![0_u8; 64 * 1024];
    let mut right = vec![0_u8; 64 * 1024];
    loop {
        let count = first.read(&mut left)?;
        if count == 0 {
            return Ok(true);
        }
        second.read_exact(&mut right[..count])?;
        if left[..count] != right[..count] {
            return Ok(false);
        }
    }
}

pub(crate) fn bundled_script() -> Result<PathBuf, String> {
    #[cfg(target_os = "windows")]
    let script_name = "separate.ps1";
    #[cfg(not(target_os = "windows"))]
    let script_name = "separate.sh";

    let executable = env::current_exe().map_err(|error| error.to_string())?;
    if let Some(sibling) = executable.parent().map(|parent| parent.join(script_name))
        && sibling.is_file()
    {
        return Ok(sibling);
    }
    let source_tree = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(script_name);
    if source_tree.is_file() {
        return Ok(source_tree);
    }
    Err(format!(
        "{script_name} is missing beside k3-gui. Use a complete K3 package."
    ))
}

pub(crate) fn run(
    request: &SeparationRequest,
    script: &Path,
    progress: impl FnMut(SeparationProgress),
) -> Result<PathBuf, String> {
    let (project, exists) = destination(&request.source, &request.projects_root)?;
    if exists && !request.allow_replace {
        return Err(
            "This project already exists. Choose the file again to confirm replacement.".into(),
        );
    }
    if !script.is_file() {
        return Err(format!(
            "Separation script is missing: {}",
            script.display()
        ));
    }
    let mut command = platform_command(script);
    command
        .arg("-f")
        .arg(&request.source)
        .arg("-d")
        .arg(&request.projects_root)
        .env("K3_PROFILE", request.profile.as_str())
        .env(
            "K3_NO_OVERWRITE",
            if request.allow_replace { "0" } else { "1" },
        );
    if let Some(model_id) = request.model_id {
        command.env("K3_MODEL", model_id);
    }
    let progress_file = ProgressFile::create(&request.projects_root).ok();
    command.env_remove("K3_SEPARATION_PROGRESS_PATH");
    if let Some(file) = &progress_file {
        command.env("K3_SEPARATION_PROGRESS_PATH", &file.path);
    }
    let output = monitor_process(command, progress_file.as_ref(), progress)?;
    if output.status.success() {
        return Ok(project);
    }
    if let Ok(log) = DiagnosticLog::initialize() {
        log.record(format!(
            "Separation failed for {} ({}); stdout: {}; stderr: {}",
            request.source.display(),
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Err("Separation failed. Open About to find the diagnostics log, and check the separator runtime."
        .into())
}

fn monitor_process(
    mut command: Command,
    progress_file: Option<&ProgressFile>,
    mut progress: impl FnMut(SeparationProgress),
) -> Result<Output, String> {
    let started = Instant::now();
    let child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("Could not start the separation script: {error}"))?;
    let (sender, receiver) = mpsc::channel();
    // 持续排空 stdout/stderr，避免日志塞满管道阻塞分离。
    let waiter = thread::spawn(move || {
        let _ = sender.send(child.wait_with_output());
    });
    let mut phase = ProgressPhase::Preparing;
    let mut fraction = None;
    let mut previous = None;
    let result = loop {
        if let Some(document) = progress_file.and_then(ProgressFile::read) {
            phase = document.phase;
            fraction = document.fraction;
        }
        let elapsed = started.elapsed();
        let current = (phase, fraction, elapsed.as_secs());
        if previous != Some(current) {
            progress(SeparationProgress {
                phase,
                fraction,
                elapsed,
            });
            previous = Some(current);
        }
        match receiver.recv_timeout(Duration::from_millis(200)) {
            Ok(output) => break output.map_err(|error| error.to_string()),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                break Err("Could not collect the separation script result".into());
            }
        }
    };
    let _ = waiter.join();
    result
}

fn platform_command(script: &Path) -> Command {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;

        let mut command = Command::new("powershell.exe");
        command
            .arg("-NoProfile")
            .arg("-NonInteractive")
            .arg("-ExecutionPolicy")
            .arg("Bypass")
            .arg("-File")
            .arg(script);
        // GUI 分离任务通过管道收集诊断输出，不需要 Windows 控制台窗口。
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        command
    }
    #[cfg(not(target_os = "windows"))]
    {
        let mut command = Command::new("bash");
        command.arg(script);
        command
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Profile, ProgressFile, ProgressPhase, SeparationRequest, destination,
        retryable_unprepared_destination, run,
    };
    use k3_core::{CreateProject, FileProjectRepository, ProjectRepository};
    use std::{fs, path::Path};

    #[test]
    fn destination_identifies_existing_project_without_overwriting_it() {
        let sandbox = tempfile::tempdir().unwrap();
        let source = sandbox.path().join("A song.flac");
        fs::write(&source, b"audio").unwrap();
        let projects = sandbox.path().join("projects");
        fs::create_dir(&projects).unwrap();
        let project = projects.join("A song");
        assert_eq!(
            destination(&source, &projects).unwrap(),
            (project.clone(), false)
        );
        fs::create_dir(&project).unwrap();
        fs::write(project.join("project.json"), b"{}").unwrap();
        assert_eq!(destination(&source, &projects).unwrap(), (project, true));
    }

    #[test]
    fn existing_non_project_is_rejected() {
        let sandbox = tempfile::tempdir().unwrap();
        let source = sandbox.path().join("song.wav");
        fs::write(&source, b"audio").unwrap();
        let projects = sandbox.path().join("projects");
        fs::create_dir(&projects).unwrap();
        fs::create_dir(projects.join("song")).unwrap();
        assert!(
            destination(&source, &projects)
                .unwrap_err()
                .contains("not a K3 project")
        );
    }

    #[test]
    fn failed_project_retries_only_with_the_same_cached_audio() {
        let sandbox = tempfile::tempdir().unwrap();
        let projects = sandbox.path().join("projects");
        let source = sandbox.path().join("song.wav");
        fs::write(&source, b"audio").unwrap();
        let project = projects.join("song");
        FileProjectRepository
            .create(CreateProject {
                root: project.clone(),
                song: source.clone(),
                lyrics: None,
                title: None,
            })
            .unwrap();
        let document_path = project.join("project.json");
        let mut document: serde_json::Value =
            serde_json::from_slice(&fs::read(&document_path).unwrap()).unwrap();
        document["separation"] = serde_json::json!({
            "status": "failed", "details": { "message": "model_not_allowed" }
        });
        fs::write(&document_path, serde_json::to_vec(&document).unwrap()).unwrap();

        assert!(retryable_unprepared_destination(&source, &projects));
        fs::write(&source, b"audix").unwrap();
        assert!(!retryable_unprepared_destination(&source, &projects));
        fs::write(&source, b"audio").unwrap();
        document["separation"] = serde_json::json!({ "status": "running" });
        fs::write(&document_path, serde_json::to_vec(&document).unwrap()).unwrap();
        assert!(!retryable_unprepared_destination(&source, &projects));
    }

    #[test]
    fn replacing_existing_project_requires_confirmation() {
        let sandbox = tempfile::tempdir().unwrap();
        let source = sandbox.path().join("song.wav");
        fs::write(&source, b"audio").unwrap();
        let projects = sandbox.path().join("projects");
        fs::create_dir(&projects).unwrap();
        let project = projects.join("song");
        fs::create_dir(&project).unwrap();
        fs::write(project.join("project.json"), b"{}").unwrap();
        let request = SeparationRequest {
            source,
            projects_root: projects,
            profile: Profile::Balanced,
            model_id: None,
            allow_replace: false,
        };
        assert!(
            run(&request, Path::new("missing.sh"), |_| {})
                .unwrap_err()
                .contains("already exists")
        );
    }

    #[cfg(unix)]
    #[test]
    fn runs_wrapper_with_selected_profile_and_no_overwrite_guard() {
        let sandbox = tempfile::tempdir().unwrap();
        let source = sandbox.path().join("song with space.wav");
        fs::write(&source, b"audio").unwrap();
        let projects = sandbox.path().join("projects");
        fs::create_dir(&projects).unwrap();
        let script = sandbox.path().join("separate.sh");
        fs::write(
            &script,
            "printf '%s\\n' \"$K3_PROFILE\" \"$K3_MODEL\" \"$K3_NO_OVERWRITE\" \"$1\" \"$2\" \"$3\" \"$4\" > \"$4/invocation.txt\"\n",
        )
        .unwrap();
        let request = SeparationRequest {
            source: source.clone(),
            projects_root: projects.clone(),
            profile: Profile::Quality,
            model_id: Some("bs-roformer-viperx-1297"),
            allow_replace: false,
        };
        assert_eq!(
            run(&request, &script, |_| {}).unwrap(),
            projects.join("song with space")
        );
        let invocation = fs::read_to_string(projects.join("invocation.txt")).unwrap();
        assert_eq!(
            invocation.lines().collect::<Vec<_>>(),
            [
                "quality",
                "bs-roformer-viperx-1297",
                "1",
                "-f",
                source.to_str().unwrap(),
                "-d",
                projects.to_str().unwrap(),
            ]
        );
    }

    #[test]
    fn ignores_incomplete_unknown_and_invalid_progress_documents() {
        let sandbox = tempfile::tempdir().unwrap();
        let file = ProgressFile::create(sandbox.path()).unwrap();
        for document in [
            r#"{"schema_version":1,"phase":"separating_vocals""#,
            r#"{"schema_version":2,"phase":"separating_vocals","fraction":0.5}"#,
            r#"{"schema_version":1,"phase":"unknown","fraction":0.5}"#,
            r#"{"schema_version":1,"phase":"separating_vocals","fraction":1.5}"#,
        ] {
            fs::write(&file.path, document).unwrap();
            assert!(file.read().is_none());
        }
        fs::write(
            &file.path,
            r#"{"schema_version":1,"phase":"separating_vocals","fraction":0.37}"#,
        )
        .unwrap();
        let document = file.read().unwrap();
        assert_eq!(document.phase, ProgressPhase::SeparatingVocals);
        assert_eq!(document.fraction, Some(0.37));
    }

    #[cfg(unix)]
    #[test]
    fn reports_live_progress_drains_output_and_cleans_telemetry() {
        let sandbox = tempfile::tempdir().unwrap();
        let projects = sandbox.path().join("projects");
        fs::create_dir(&projects).unwrap();
        let source = sandbox.path().join("song.wav");
        fs::write(&source, b"audio").unwrap();
        let script = sandbox.path().join("separate.sh");
        fs::write(
            &script,
            r#"python3 - <<'PY'
import json, os, pathlib, sys, time
path = pathlib.Path(os.environ["K3_SEPARATION_PROGRESS_PATH"])
path.write_text(json.dumps({"schema_version":1,"phase":"loading_vocals","fraction":None}))
sys.stdout.write("x" * 500000)
sys.stderr.write("y" * 500000)
sys.stdout.flush()
sys.stderr.flush()
time.sleep(0.4)
temporary = path.with_suffix(".tmp")
temporary.write_text(json.dumps({"schema_version":1,"phase":"separating_vocals","fraction":0.37}))
os.replace(temporary, path)
time.sleep(0.5)
(path.parent.parent / "finished").write_text("done")
PY
"#,
        )
        .unwrap();
        let request = SeparationRequest {
            source,
            projects_root: projects.clone(),
            profile: Profile::Quality,
            model_id: None,
            allow_replace: false,
        };
        let mut saw_loading = false;
        let mut saw_live_fraction = false;
        run(&request, &script, |progress| {
            saw_loading |= progress.phase == ProgressPhase::LoadingVocals;
            if progress.fraction == Some(0.37) {
                assert!(!projects.join("finished").exists());
                saw_live_fraction = true;
            }
        })
        .unwrap();
        assert!(saw_loading && saw_live_fraction);
        assert!(fs::read_dir(&projects).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".k3-progress-")
        }));
    }

    #[cfg(unix)]
    #[test]
    fn legacy_worker_keeps_elapsed_indicator_without_inventing_percentage() {
        let sandbox = tempfile::tempdir().unwrap();
        let source = sandbox.path().join("song.wav");
        fs::write(&source, b"audio").unwrap();
        let script = sandbox.path().join("legacy.sh");
        fs::write(&script, "sleep 1.1\n").unwrap();
        let request = SeparationRequest {
            source,
            projects_root: sandbox.path().into(),
            profile: Profile::Quality,
            model_id: None,
            allow_replace: false,
        };
        let mut elapsed = 0;
        run(&request, &script, |progress| {
            assert_eq!(progress.fraction, None);
            elapsed = progress.elapsed.as_secs();
        })
        .unwrap();
        assert!(elapsed >= 1);
    }
}
