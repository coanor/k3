use std::{
    env,
    fs::File,
    io::{BufReader, Read},
    path::{Path, PathBuf},
    process::Command,
};

use crate::logging::DiagnosticLog;
use k3_core::{FileProjectRepository, ProjectRepository, SeparationState};

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
            2 => Self::Quality,
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
    let mut left = [0_u8; 64 * 1024];
    let mut right = [0_u8; 64 * 1024];
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

pub(crate) fn run(request: &SeparationRequest, script: &Path) -> Result<PathBuf, String> {
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
    let output = command
        .output()
        .map_err(|error| format!("Could not start the separation script: {error}"))?;
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

fn platform_command(script: &Path) -> Command {
    #[cfg(target_os = "windows")]
    {
        let mut command = Command::new("powershell.exe");
        command
            .arg("-NoProfile")
            .arg("-NonInteractive")
            .arg("-ExecutionPolicy")
            .arg("Bypass")
            .arg("-File")
            .arg(script);
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
    use super::{Profile, SeparationRequest, destination, retryable_unprepared_destination, run};
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
            allow_replace: false,
        };
        assert!(
            run(&request, Path::new("missing.sh"))
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
            "printf '%s\\n' \"$K3_PROFILE\" \"$K3_NO_OVERWRITE\" \"$1\" \"$2\" \"$3\" \"$4\" > \"$4/invocation.txt\"\n",
        )
        .unwrap();
        let request = SeparationRequest {
            source: source.clone(),
            projects_root: projects.clone(),
            profile: Profile::Quality,
            allow_replace: false,
        };
        assert_eq!(
            run(&request, &script).unwrap(),
            projects.join("song with space")
        );
        let invocation = fs::read_to_string(projects.join("invocation.txt")).unwrap();
        assert_eq!(
            invocation.lines().collect::<Vec<_>>(),
            [
                "quality",
                "1",
                "-f",
                source.to_str().unwrap(),
                "-d",
                projects.to_str().unwrap(),
            ]
        );
    }
}
