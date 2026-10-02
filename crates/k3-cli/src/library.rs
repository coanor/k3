use std::{
    collections::HashMap,
    error::Error,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, atomic::AtomicBool},
    time::{SystemTime, UNIX_EPOCH},
};

use k3_core::{
    CreateProject, FileProjectRepository, ProjectPath, ProjectRepository, SeparationProfile,
    SeparationState, SongPreparation, VocalEffectPreset,
};
use serde::{Deserialize, Serialize};

use crate::python_separator::{
    PythonSeparatorConfig, PythonStemSeparator, cleanup_obsolete_outputs, separation_log_path,
    separation_output_paths,
};

const DEFAULT_EXTENSIONS: [&str; 6] = ["mp3", "flac", "wav", "m4a", "aac", "ogg"];

/// 媒体库模式所需的全部持久配置。
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct LibraryConfig {
    pub music_root: PathBuf,
    pub projects_root: PathBuf,
    #[serde(default)]
    pub scan: ScanConfig,
    pub separation: SeparationConfig,
    #[serde(default)]
    pub recording: RecordingConfig,
    #[serde(default)]
    pub lyrics: LyricsConfig,
    #[serde(default)]
    pub netease: NeteaseConfig,
}

impl LibraryConfig {
    pub fn load(path: &Path) -> Result<Self, Box<dyn Error>> {
        let bytes = fs::read(path)?;
        let config: Self = serde_json::from_slice(&bytes)?;
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<(), Box<dyn Error>> {
        if !self.music_root.is_dir() {
            return Err(format!(
                "music_root is not a readable directory: {}",
                self.music_root.display()
            )
            .into());
        }
        if !self.projects_root.is_dir() {
            return Err(format!(
                "projects_root is not a readable directory: {}",
                self.projects_root.display()
            )
            .into());
        }
        if self.scan.extensions.is_empty() {
            return Err("scan.extensions must contain at least one extension".into());
        }
        if let Some(size) = self.separation.segment_size
            && !(1..=4_096).contains(&size)
        {
            return Err("separation.segment_size must be between 1 and 4096".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ScanConfig {
    #[serde(default = "default_recursive")]
    pub recursive: bool,
    #[serde(default = "default_extensions")]
    pub extensions: Vec<String>,
}

impl Default for ScanConfig {
    fn default() -> Self {
        Self {
            recursive: true,
            extensions: default_extensions(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SeparationConfig {
    pub worker: PathBuf,
    pub model_dir: Option<PathBuf>,
    pub log_dir: Option<PathBuf>,
    #[serde(default)]
    pub profile: SeparationProfile,
    pub model: Option<String>,
    pub segment_size: Option<u32>,
    #[serde(default = "default_autocast")]
    pub autocast: bool,
    #[serde(default = "default_preserve_backing_vocals")]
    pub preserve_backing_vocals: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct LyricsConfig {
    #[serde(default = "default_auto_download")]
    pub auto_download: bool,
    #[serde(default)]
    pub netease_fallback: bool,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct RecordingConfig {
    #[serde(default)]
    pub default_effect: VocalEffectPreset,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct NeteaseConfig {
    #[serde(default)]
    pub enabled: bool,
}

impl Default for LyricsConfig {
    fn default() -> Self {
        Self {
            auto_download: true,
            netease_fallback: false,
        }
    }
}

fn default_preserve_backing_vocals() -> bool {
    true
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectEntry {
    pub path: PathBuf,
    pub title: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceEntry {
    pub path: PathBuf,
    pub project_path: PathBuf,
    pub project_state: SourceProjectState,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceProjectState {
    New,
    Current,
    ReplaceSource,
}

pub(crate) fn source_entry_for_path(config: &LibraryConfig, source: &Path) -> SourceEntry {
    let repository = FileProjectRepository;
    let project_path = project_path_for_source(&config.projects_root, source);
    let project_state = source_project_state(repository, &project_path, source);
    SourceEntry {
        path: source.to_path_buf(),
        project_path,
        project_state,
    }
}

fn source_project_state(
    repository: FileProjectRepository,
    project_path: &Path,
    source: &Path,
) -> SourceProjectState {
    let Ok(project) = repository.open(project_path) else {
        return SourceProjectState::New;
    };
    if matches!(project.separation(), SeparationState::Failed { .. }) {
        return SourceProjectState::New;
    }
    if source_identity(source) == source_identity(&project.source_path()) {
        SourceProjectState::Current
    } else {
        SourceProjectState::ReplaceSource
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LibrarySnapshot {
    pub projects: Vec<ProjectEntry>,
    pub sources: Vec<SourceEntry>,
}

/// 扫描两个根目录，并隐藏扩展名过滤、排序和 project 映射规则。
pub fn scan(config: &LibraryConfig) -> Result<LibrarySnapshot, Box<dyn Error>> {
    let repository = FileProjectRepository;
    let mut projects = Vec::new();
    let mut imported_sources = HashMap::new();
    for entry in fs::read_dir(&config.projects_root)? {
        let path = entry?.path();
        if !path.join("project.json").is_file() {
            continue;
        }
        if let Ok(project) = repository.open(&path) {
            if matches!(project.separation(), SeparationState::Failed { .. }) {
                continue;
            }
            if let Some(identity) = source_identity(&project.source_path()) {
                imported_sources.insert(identity, path.clone());
            }
            projects.push(ProjectEntry {
                path,
                title: project.title().to_owned(),
            });
        }
    }
    projects.sort_by(|left, right| left.title.cmp(&right.title));

    let extensions = config
        .scan
        .extensions
        .iter()
        .map(|extension| extension.trim_start_matches('.').to_ascii_lowercase())
        .collect::<Vec<_>>();
    let mut paths = Vec::new();
    scan_music_paths(
        &config.music_root,
        config.scan.recursive,
        &extensions,
        &mut paths,
    )?;
    paths.sort();
    let sources = paths
        .into_iter()
        .map(|path| {
            let existing_project = source_identity(&path)
                .and_then(|identity| imported_sources.get(&identity))
                .cloned();
            let (project_path, project_state) = existing_project.map_or_else(
                || {
                    let project_path = project_path_for_source(&config.projects_root, &path);
                    let project_state = source_project_state(repository, &project_path, &path);
                    (project_path, project_state)
                },
                |project_path| (project_path, SourceProjectState::Current),
            );
            SourceEntry {
                path,
                project_path,
                project_state,
            }
        })
        .collect();
    Ok(LibrarySnapshot { projects, sources })
}

fn source_identity(path: &Path) -> Option<(std::ffi::OsString, u64)> {
    Some((
        path.file_name()?.to_os_string(),
        fs::metadata(path).ok()?.len(),
    ))
}

/// 创建一个 project，并通过配置好的 worker 完成分离。
///
/// 新歌曲创建 project；同名已有 project 则保留其他内容并覆盖分离结果。
#[cfg(test)]
pub fn import_and_separate(
    config: &LibraryConfig,
    source: &Path,
) -> Result<PathBuf, Box<dyn Error>> {
    let project_root = project_path_for_source(&config.projects_root, source);
    separate_into_project(config, source, &project_root, None)
}

pub fn import_and_separate_with_cancellation(
    config: &LibraryConfig,
    source: &Path,
    cancellation: Arc<AtomicBool>,
) -> Result<PathBuf, Box<dyn Error>> {
    let project_root = project_path_for_source(&config.projects_root, source);
    separate_into_project(config, source, &project_root, Some(cancellation))
}

/// 使用当前配置重新分离已有 project，同时保留其歌词、take 和其他文件。
#[cfg(test)]
pub fn reseparate(
    config: &LibraryConfig,
    source: &Path,
    project_root: &Path,
) -> Result<PathBuf, Box<dyn Error>> {
    if !project_root.exists() {
        return Err(format!("project no longer exists: {}", project_root.display()).into());
    }
    separate_into_project(config, source, project_root, None)
}

pub fn reseparate_with_cancellation(
    config: &LibraryConfig,
    source: &Path,
    project_root: &Path,
    cancellation: Arc<AtomicBool>,
) -> Result<PathBuf, Box<dyn Error>> {
    if !project_root.exists() {
        return Err(format!("project no longer exists: {}", project_root.display()).into());
    }
    separate_into_project(config, source, project_root, Some(cancellation))
}

/// 使用新音源重新生成已有 project，并保留歌词、takes 和其他用户内容。
#[cfg(test)]
pub fn replace_source_and_reseparate(
    config: &LibraryConfig,
    source: &Path,
    project_root: &Path,
) -> Result<PathBuf, Box<dyn Error>> {
    replace_source_and_reseparate_inner(config, source, project_root, None)
}

pub fn replace_source_and_reseparate_with_cancellation(
    config: &LibraryConfig,
    source: &Path,
    project_root: &Path,
    cancellation: Arc<AtomicBool>,
) -> Result<PathBuf, Box<dyn Error>> {
    replace_source_and_reseparate_inner(config, source, project_root, Some(cancellation))
}

fn replace_source_and_reseparate_inner(
    config: &LibraryConfig,
    source: &Path,
    project_root: &Path,
    cancellation: Option<Arc<AtomicBool>>,
) -> Result<PathBuf, Box<dyn Error>> {
    if !source.is_file() {
        return Err(format!("replacement source is not readable: {}", source.display()).into());
    }
    if !project_root.exists() {
        return Err(format!("project no longer exists: {}", project_root.display()).into());
    }
    let expected_project = project_path_for_source(&config.projects_root, source);
    if expected_project.canonicalize()? != project_root.canonicalize()? {
        return Err(format!(
            "replacement source does not map to project: {}",
            project_root.display()
        )
        .into());
    }

    let project_name = project_root
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("project");
    let staging = create_transaction_directory(
        &config.projects_root,
        &format!(".{project_name}-source-upgrade"),
    )?;
    let prepare_result = (|| -> Result<(), Box<dyn Error>> {
        fs::create_dir_all(staging.join("source"))?;
        fs::create_dir_all(staging.join("stems"))?;
        fs::copy(
            project_root.join("project.json"),
            staging.join("project.json"),
        )?;

        let repository = FileProjectRepository;
        let mut project = repository.open(&staging)?;
        let filename = source
            .file_name()
            .and_then(|value| value.to_str())
            .ok_or_else(|| {
                format!(
                    "replacement source path is not valid UTF-8: {}",
                    source.display()
                )
            })?;
        let relative_source = ProjectPath::new(format!("source/{filename}"))?;
        fs::copy(source, staging.join(relative_source.as_str()))?;
        project.replace_source(relative_source)?;

        let separator_config = PythonSeparatorConfig {
            worker: config.separation.worker.clone(),
            model_dir: config.separation.model_dir.clone(),
            project_root: staging.clone(),
            log_path: config
                .separation
                .log_dir
                .as_ref()
                .map_or_else(separation_log_path, |directory| {
                    Ok(directory.join("separate.log"))
                })?,
            model_id: config.separation.model.clone(),
            overwrite: true,
            segment_size: config.separation.segment_size,
            autocast: config.separation.autocast,
            preserve_backing_vocals: config.separation.preserve_backing_vocals,
        };
        let separator = match cancellation {
            Some(cancellation) => {
                PythonStemSeparator::with_cancellation(separator_config, cancellation)
            }
            None => PythonStemSeparator::new(separator_config),
        };
        SongPreparation::new(separator)
            .reprepare(&mut project, config.separation.profile)
            .map_err(|error| format!("{error}; existing project was preserved"))?;
        repository.save(&mut project)?;
        Ok(())
    })();
    if let Err(error) = prepare_result {
        let _ = fs::remove_dir_all(&staging);
        return Err(error);
    }

    commit_source_replacement(project_root, &staging)?;
    Ok(project_root.to_path_buf())
}

fn create_transaction_directory(root: &Path, prefix: &str) -> Result<PathBuf, Box<dyn Error>> {
    let timestamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis();
    for attempt in 0..1_000_u16 {
        let candidate = root.join(format!("{prefix}-{timestamp}-{attempt}"));
        match fs::create_dir(&candidate) {
            Ok(()) => return Ok(candidate),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
    }
    Err(format!(
        "cannot create transaction directory below {}",
        root.display()
    )
    .into())
}

fn commit_source_replacement(target_project: &Path, staging: &Path) -> Result<(), Box<dyn Error>> {
    const REPLACED_ENTRIES: [&str; 3] = ["source", "stems", "project.json"];

    let backup = create_transaction_directory(target_project, ".source-backup")?;
    let mut backed_up = Vec::new();
    for entry in REPLACED_ENTRIES {
        if let Err(error) = fs::rename(target_project.join(entry), backup.join(entry)) {
            let rollback_errors =
                rollback_source_replacement(target_project, staging, &backup, &[], &backed_up);
            return Err(replacement_commit_error(
                &format!("cannot stage existing project {entry}: {error}"),
                &rollback_errors,
                staging,
                &backup,
            ));
        }
        backed_up.push(entry);
    }

    let mut installed = Vec::new();
    for entry in REPLACED_ENTRIES {
        if let Err(error) = fs::rename(staging.join(entry), target_project.join(entry)) {
            let rollback_errors = rollback_source_replacement(
                target_project,
                staging,
                &backup,
                &installed,
                &backed_up,
            );
            return Err(replacement_commit_error(
                &format!("cannot commit replacement project {entry}: {error}"),
                &rollback_errors,
                staging,
                &backup,
            ));
        }
        installed.push(entry);
    }

    let _ = fs::remove_dir_all(&backup);
    let _ = fs::remove_dir_all(staging);
    Ok(())
}

fn rollback_source_replacement(
    project_root: &Path,
    staging: &Path,
    backup: &Path,
    installed: &[&str],
    backed_up: &[&str],
) -> Vec<String> {
    let mut errors = Vec::new();
    for entry in installed.iter().rev() {
        if let Err(error) = fs::rename(project_root.join(entry), staging.join(entry)) {
            errors.push(format!(
                "cannot move replacement {entry} out of project: {error}"
            ));
        }
    }
    for entry in backed_up.iter().rev() {
        if let Err(error) = fs::rename(backup.join(entry), project_root.join(entry)) {
            errors.push(format!("cannot restore original {entry}: {error}"));
        }
    }
    errors
}

fn replacement_commit_error(
    cause: &str,
    rollback_errors: &[String],
    staging: &Path,
    backup: &Path,
) -> Box<dyn Error> {
    if rollback_errors.is_empty() {
        let _ = fs::remove_dir_all(backup);
        let _ = fs::remove_dir_all(staging);
        return format!("{cause}; existing project was restored").into();
    }

    format!(
        "{cause}; rollback was incomplete: {}; recovery data was kept at {} and {}",
        rollback_errors.join("; "),
        backup.display(),
        staging.display()
    )
    .into()
}

fn separate_into_project(
    config: &LibraryConfig,
    source: &Path,
    project_root: &Path,
    cancellation: Option<Arc<AtomicBool>>,
) -> Result<PathBuf, Box<dyn Error>> {
    let repository = FileProjectRepository;
    let replacing = project_root.exists();
    let title = source
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("Untitled")
        .to_owned();
    let mut project = if replacing {
        let project = repository.open(project_root)?;
        if source_identity(source) != source_identity(&project.source_path()) {
            return Err(format!(
                "source does not match existing project: {}",
                project_root.display()
            )
            .into());
        }
        project
    } else {
        repository.create(CreateProject {
            root: project_root.to_path_buf(),
            song: source.to_path_buf(),
            lyrics: None,
            title: Some(title),
        })?
    };
    let previous_outputs = separation_output_paths(&project);
    let separator_config = PythonSeparatorConfig {
        worker: config.separation.worker.clone(),
        model_dir: config.separation.model_dir.clone(),
        project_root: project.root().to_path_buf(),
        log_path: config
            .separation
            .log_dir
            .as_ref()
            .map_or_else(separation_log_path, |directory| {
                Ok(directory.join("separate.log"))
            })?,
        model_id: config.separation.model.clone(),
        overwrite: replacing,
        segment_size: config.separation.segment_size,
        autocast: config.separation.autocast,
        preserve_backing_vocals: config.separation.preserve_backing_vocals,
    };
    let separator = match cancellation {
        Some(cancellation) => {
            PythonStemSeparator::with_cancellation(separator_config, cancellation)
        }
        None => PythonStemSeparator::new(separator_config),
    };
    let mut preparation = SongPreparation::new(separator);
    let result = if replacing {
        preparation.reprepare(&mut project, config.separation.profile)
    } else {
        preparation.prepare(&mut project, config.separation.profile)
    };
    if result.is_ok() {
        let produced_outputs = separation_output_paths(&project);
        if let Err(error) = repository.save(&mut project) {
            cleanup_obsolete_outputs(project.root(), &produced_outputs, &previous_outputs);
            return Err(error.into());
        }
        cleanup_obsolete_outputs(project.root(), &previous_outputs, &produced_outputs);
        return Ok(project_root.to_path_buf());
    }
    let error = result.unwrap_err();
    if replacing {
        return Err(format!(
            "{error}; existing project and stems were preserved: {}",
            project_root.display()
        )
        .into());
    }
    repository.save(&mut project)?;
    match archive_failed_project(&config.projects_root, project_root) {
        Ok(archived) => Err(format!(
            "{error}; failed project archived at {}; fix the configuration and retry",
            archived.display()
        )
        .into()),
        Err(archive_error) => Err(format!(
            "{error}; failed to archive project {}: {archive_error}",
            project_root.display()
        )
        .into()),
    }
}

fn archive_failed_project(
    library_root: &Path,
    failed_project: &Path,
) -> Result<PathBuf, Box<dyn Error>> {
    let archive_root = library_root.join(".failed");
    fs::create_dir_all(&archive_root)?;
    let name = failed_project
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("project");
    let timestamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis();
    for attempt in 0..1_000_u16 {
        let destination = archive_root.join(format!("{name}-{timestamp}-{attempt}"));
        if !destination.exists() {
            fs::rename(failed_project, &destination)?;
            return Ok(destination);
        }
    }
    Err(format!(
        "cannot create an archive directory for failed project {}",
        failed_project.display()
    )
    .into())
}

fn scan_music_paths(
    root: &Path,
    recursive: bool,
    extensions: &[String],
    output: &mut Vec<PathBuf>,
) -> Result<(), Box<dyn Error>> {
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        if entry.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        let path = entry.path();
        if path.is_dir() && recursive {
            scan_music_paths(&path, true, extensions, output)?;
        } else if path.is_file()
            && path
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| extensions.contains(&extension.to_ascii_lowercase()))
        {
            output.push(path);
        }
    }
    Ok(())
}

fn project_path_for_source(projects_root: &Path, source: &Path) -> PathBuf {
    let name = source
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("Untitled");
    projects_root.join(name)
}

const fn default_recursive() -> bool {
    true
}

fn default_extensions() -> Vec<String> {
    DEFAULT_EXTENSIONS.map(str::to_owned).to_vec()
}

const fn default_autocast() -> bool {
    true
}

const fn default_auto_download() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::{
        LibraryConfig, SourceProjectState, import_and_separate, replace_source_and_reseparate,
        replace_source_and_reseparate_with_cancellation, reseparate, scan, source_entry_for_path,
    };
    use k3_core::{FileProjectRepository, ProjectRepository, SeparationState, VocalEffectPreset};
    use std::{
        fs,
        sync::{Arc, atomic::AtomicBool},
    };

    #[test]
    fn scans_projects_and_supported_music_files() {
        let sandbox = tempfile::tempdir().unwrap();
        let music = sandbox.path().join("music");
        let projects = sandbox.path().join("projects");
        fs::create_dir_all(music.join("album")).unwrap();
        fs::create_dir_all(projects.join("existing")).unwrap();
        fs::write(music.join("album/song.flac"), b"audio").unwrap();
        fs::write(music.join("album/.download.part.flac"), b"partial").unwrap();
        fs::write(music.join("ignore.txt"), b"text").unwrap();
        fs::write(
            projects.join("existing/project.json"),
            br#"{
              "schema_version": 1,
              "id": "550e8400-e29b-41d4-a716-446655440000",
              "title": "Existing",
              "source": "source/song.flac",
              "separation": {"status":"not_requested"},
              "takes": [],
              "latency_compensation_ms": 0,
              "key_shift_semitones": 0,
              "effects_schema_version": 1
            }"#,
        )
        .unwrap();
        fs::create_dir_all(projects.join("existing/source")).unwrap();
        fs::write(projects.join("existing/source/song.flac"), b"audio").unwrap();
        for dir in ["stems", "takes", "lyrics", "exports"] {
            fs::create_dir_all(projects.join("existing").join(dir)).unwrap();
        }
        let config: LibraryConfig = serde_json::from_value(serde_json::json!({
            "music_root": music,
            "projects_root": projects,
            "separation": {
                "worker": "/bin/false",
                "profile": "fast"
            },
            "recording": {"default_effect": "ktv"},
            "lyrics": {"netease_fallback": true}
        }))
        .unwrap();

        let snapshot = scan(&config).unwrap();

        assert_eq!(snapshot.projects.len(), 1);
        assert_eq!(snapshot.projects[0].title, "Existing");
        assert_eq!(snapshot.sources.len(), 1);
        assert_eq!(snapshot.sources[0].path.file_name().unwrap(), "song.flac");
        assert_eq!(
            snapshot.sources[0].project_state,
            SourceProjectState::Current
        );
        assert_eq!(snapshot.sources[0].project_path, projects.join("existing"));
        assert_eq!(config.recording.default_effect, VocalEffectPreset::Ktv);
        assert!(config.lyrics.netease_fallback);
        assert!(!config.netease.enabled);
        assert!(config.separation.preserve_backing_vocals);
    }

    #[test]
    fn netease_source_is_only_enabled_explicitly() {
        let sandbox = tempfile::tempdir().unwrap();
        let music = sandbox.path().join("music");
        let projects = sandbox.path().join("projects");
        fs::create_dir_all(&music).unwrap();
        fs::create_dir_all(&projects).unwrap();

        let disabled: LibraryConfig = serde_json::from_value(serde_json::json!({
            "music_root": music,
            "projects_root": projects,
            "separation": {"worker": "/bin/false", "profile": "fast"}
        }))
        .unwrap();
        assert!(!disabled.netease.enabled);

        let enabled: LibraryConfig = serde_json::from_value(serde_json::json!({
            "music_root": disabled.music_root,
            "projects_root": disabled.projects_root,
            "separation": {"worker": "/bin/false", "profile": "fast"},
            "netease": {"enabled": true}
        }))
        .unwrap();
        assert!(enabled.netease.enabled);
    }

    #[cfg(unix)]
    #[test]
    fn imports_one_source_and_persists_ready_separation() {
        use std::os::unix::fs::PermissionsExt;

        let sandbox = tempfile::tempdir().unwrap();
        let music = sandbox.path().join("music");
        let projects = sandbox.path().join("projects");
        fs::create_dir_all(&music).unwrap();
        fs::create_dir_all(&projects).unwrap();
        let song = music.join("新歌.wav");
        fs::write(&song, b"audio").unwrap();
        let worker = sandbox.path().join("worker");
        fs::write(
            &worker,
            r#"#!/usr/bin/env python3
import json, pathlib, sys
r = json.loads(sys.stdin.readline())
out = pathlib.Path(r["params"]["output_dir"])
out.mkdir(parents=True, exist_ok=True)
model = r["params"].get("model_id") or "default-model"
v = out / ("vocals-" + model + ".wav")
b = out / ("backing-vocals-" + model + ".wav")
a = out / ("accompaniment-" + model + ".wav")
v.write_bytes(b"voice")
b.write_bytes(b"backing voice")
a.write_bytes(b"music")
print(json.dumps({"id": r["id"], "ok": True, "result": {
  "vocals": str(v), "backing_vocals": str(b), "accompaniment": str(a), "provenance": {
    "provider": "fake", "architecture": "mdx-net",
    "checkpoint_id": "fake-model", "checkpoint_sha256": "a" * 64,
    "profile": r["params"]["profile"], "backing_vocals_model": {
      "provider": "fake", "architecture": "mdx-net",
      "checkpoint_id": "uvr-mdx-karaoke-2", "checkpoint_sha256": "b" * 64}}}}))
"#,
        )
        .unwrap();
        let mut permissions = fs::metadata(&worker).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&worker, permissions).unwrap();
        let config: LibraryConfig = serde_json::from_value(serde_json::json!({
            "music_root": music,
            "projects_root": projects,
            "separation": {
                "worker": worker,
                "log_dir": sandbox.path().join("logs"),
                "profile": "fast",
                "model": "fake-model",
                "autocast": false,
                "preserve_backing_vocals": true
            }
        }))
        .unwrap();

        assert_eq!(config.recording.default_effect, VocalEffectPreset::Clean);
        assert!(!config.lyrics.netease_fallback);

        let project_path = import_and_separate(&config, &song).unwrap();
        let project = FileProjectRepository.open(&project_path).unwrap();

        assert!(matches!(project.separation(), SeparationState::Ready(_)));
        let SeparationState::Ready(manifest) = project.separation() else {
            unreachable!()
        };
        assert!(manifest.vocals.resolve(&project_path).is_file());
        assert!(
            manifest
                .backing_vocals
                .as_ref()
                .unwrap()
                .resolve(&project_path)
                .is_file()
        );
        assert_eq!(
            manifest.backing_vocals.as_ref().unwrap().as_str(),
            "stems/backing-vocals-fake-model.wav"
        );
        assert_eq!(
            scan(&config).unwrap().sources[0].project_state,
            SourceProjectState::Current
        );
    }

    #[cfg(unix)]
    #[test]
    fn reseparation_overwrites_stems_but_preserves_the_existing_project() {
        use std::os::unix::fs::PermissionsExt;

        let sandbox = tempfile::tempdir().unwrap();
        let music = sandbox.path().join("music");
        let projects = sandbox.path().join("projects");
        fs::create_dir_all(&music).unwrap();
        fs::create_dir_all(&projects).unwrap();
        let song = music.join("song.wav");
        fs::write(&song, b"audio").unwrap();
        let worker = sandbox.path().join("worker");
        let write_worker = |vocals: &str, fail: bool| {
            let response = if fail {
                r#"print(json.dumps({"id": r["id"], "ok": False, "error": {
  "code": "forced_failure", "message": "new configuration failed"}}))"#
                    .to_owned()
            } else {
                format!(
                    r#"out = pathlib.Path(r["params"]["output_dir"])
out.mkdir(parents=True, exist_ok=True)
model = r["params"].get("model_id") or "default-model"
v = out / ("vocals-" + model + ".wav")
b = out / ("backing-vocals-" + model + ".wav")
a = out / ("accompaniment-" + model + ".wav")
v.write_bytes({vocals:?}.encode())
b.write_bytes(b"backing")
a.write_bytes(b"music")
print(json.dumps({{"id": r["id"], "ok": True, "result": {{
  "vocals": str(v), "backing_vocals": str(b), "accompaniment": str(a), "provenance": {{
    "provider": "fake", "architecture": "mdx-net",
    "checkpoint_id": model, "checkpoint_sha256": "a" * 64,
    "profile": r["params"]["profile"], "backing_vocals_model": {{
      "provider": "fake", "architecture": "mdx-net",
      "checkpoint_id": "uvr-mdx-karaoke-2", "checkpoint_sha256": "b" * 64}}}}}}}}))"#
                )
            };
            fs::write(
                &worker,
                format!("#!/usr/bin/env python3\nimport json, pathlib, sys\nr = json.loads(sys.stdin.readline())\n{response}\n"),
            )
            .unwrap();
            let mut permissions = fs::metadata(&worker).unwrap().permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(&worker, permissions).unwrap();
        };
        write_worker("voice-v1", false);
        let mut config: LibraryConfig = serde_json::from_value(serde_json::json!({
            "music_root": music,
            "projects_root": projects,
            "separation": {
                "worker": worker,
                "log_dir": sandbox.path().join("logs"),
                "profile": "fast",
                "model": "model-v1"
            }
        }))
        .unwrap();
        let project_path = import_and_separate(&config, &song).unwrap();
        let original = FileProjectRepository.open(&project_path).unwrap();
        let SeparationState::Ready(original_manifest) = original.separation() else {
            panic!("expected ready separation")
        };
        let original_vocals = original_manifest.vocals.resolve(&project_path);
        fs::write(project_path.join("takes/keep.wav"), b"keep").unwrap();

        write_worker("voice-v2", false);
        config.separation.model = Some("model-v2".into());
        reseparate(&config, &song, &project_path).unwrap();
        let replaced = FileProjectRepository.open(&project_path).unwrap();

        assert_eq!(original.id(), replaced.id());
        assert_eq!(
            fs::read(project_path.join("takes/keep.wav")).unwrap(),
            b"keep"
        );
        let SeparationState::Ready(manifest) = replaced.separation() else {
            panic!("expected ready separation")
        };
        assert_eq!(manifest.provenance.checkpoint_id, "model-v2");
        let replaced_vocals = manifest.vocals.resolve(&project_path);
        assert_eq!(fs::read(&replaced_vocals).unwrap(), b"voice-v2");
        assert!(!original_vocals.exists());

        write_worker("unused", true);
        let error = reseparate(&config, &song, &project_path)
            .unwrap_err()
            .to_string();
        let preserved = FileProjectRepository.open(&project_path).unwrap();
        assert!(error.contains("existing project and stems were preserved"));
        assert!(matches!(preserved.separation(), SeparationState::Ready(_)));
        assert_eq!(fs::read(replaced_vocals).unwrap(), b"voice-v2");
        assert!(project_path.join("takes/keep.wav").is_file());
    }

    #[cfg(unix)]
    #[test]
    fn source_upgrade_replaces_project_media_and_stems_without_losing_user_content() {
        for upgraded_filename in ["song.wav", "song.flac"] {
            run_source_upgrade_case(upgraded_filename);
        }
    }

    #[cfg(unix)]
    fn write_source_echo_worker(worker: &std::path::Path) {
        use std::os::unix::fs::PermissionsExt;

        fs::write(
            worker,
            r#"#!/usr/bin/env python3
import json, pathlib, sys
r = json.loads(sys.stdin.readline())
source = pathlib.Path(r["params"]["input_path"]).read_bytes()
out = pathlib.Path(r["params"]["output_dir"])
out.mkdir(parents=True, exist_ok=True)
v, b, a = out / "vocals.wav", out / "backing-vocals.wav", out / "accompaniment.wav"
v.write_bytes(b"stem:" + source)
b.write_bytes(b"backing")
a.write_bytes(b"music")
result = {"id": r["id"], "ok": True, "result": {
  "vocals": str(v), "backing_vocals": str(b), "accompaniment": str(a), "provenance": {
    "provider": "fake", "architecture": "mdx-net",
    "checkpoint_id": "fake-model", "checkpoint_sha256": "a" * 64,
    "profile": r["params"]["profile"], "backing_vocals_model": {
      "provider": "fake", "architecture": "mdx-net",
      "checkpoint_id": "uvr-mdx-karaoke-2", "checkpoint_sha256": "b" * 64}}}}
print(json.dumps(result))
"#,
        )
        .unwrap();
        let mut permissions = fs::metadata(worker).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(worker, permissions).unwrap();
    }

    #[cfg(unix)]
    fn attach_user_content(project_path: &std::path::Path) {
        fs::write(project_path.join("lyrics/keep.lrc"), b"lyrics").unwrap();
        fs::write(project_path.join("takes/keep-dry.wav"), b"take").unwrap();
        let project_file = project_path.join("project.json");
        let mut document: serde_json::Value =
            serde_json::from_slice(&fs::read(&project_file).unwrap()).unwrap();
        document["lyrics"] = serde_json::json!("lyrics/keep.lrc");
        document["takes"] = serde_json::json!([{
            "id": "keep",
            "dry_audio": "takes/keep-dry.wav"
        }]);
        fs::write(project_file, serde_json::to_vec_pretty(&document).unwrap()).unwrap();
    }

    #[cfg(unix)]
    fn assert_user_content_preserved(project_path: &std::path::Path) {
        assert_eq!(
            fs::read(project_path.join("lyrics/keep.lrc")).unwrap(),
            b"lyrics"
        );
        assert_eq!(
            fs::read(project_path.join("takes/keep-dry.wav")).unwrap(),
            b"take"
        );
    }

    #[cfg(unix)]
    fn run_source_upgrade_case(upgraded_filename: &str) {
        let sandbox = tempfile::tempdir().unwrap();
        let music = sandbox.path().join("music");
        let projects = sandbox.path().join("projects");
        fs::create_dir_all(&music).unwrap();
        fs::create_dir_all(&projects).unwrap();
        let original_source = music.join("song.wav");
        fs::write(&original_source, b"old").unwrap();
        let worker = sandbox.path().join("worker");
        write_source_echo_worker(&worker);
        let config: LibraryConfig = serde_json::from_value(serde_json::json!({
            "music_root": music,
            "projects_root": projects,
            "separation": {"worker": worker, "profile": "fast"}
        }))
        .unwrap();
        let project_path = import_and_separate(&config, &original_source).unwrap();
        let original = FileProjectRepository.open(&project_path).unwrap();
        attach_user_content(&project_path);

        let upgraded_source = music.join(upgraded_filename);
        fs::write(&upgraded_source, b"new-audio-with-a-different-size").unwrap();
        if upgraded_source != original_source {
            fs::remove_file(&original_source).unwrap();
        }
        assert_eq!(
            source_entry_for_path(&config, &upgraded_source).project_state,
            SourceProjectState::ReplaceSource
        );
        let error = replace_source_and_reseparate_with_cancellation(
            &config,
            &upgraded_source,
            &project_path,
            Arc::new(AtomicBool::new(true)),
        )
        .unwrap_err()
        .to_string();
        let preserved = FileProjectRepository.open(&project_path).unwrap();
        assert!(error.contains("existing project was preserved"));
        assert_eq!(preserved.source_path().file_name().unwrap(), "song.wav");
        assert_eq!(fs::read(preserved.source_path()).unwrap(), b"old");
        assert_eq!(
            fs::read(project_path.join("stems/vocals.wav")).unwrap(),
            b"stem:old"
        );
        assert_user_content_preserved(&project_path);

        replace_source_and_reseparate(&config, &upgraded_source, &project_path).unwrap();
        let upgraded = FileProjectRepository.open(&project_path).unwrap();
        assert_eq!(upgraded.id(), original.id());
        assert_eq!(
            upgraded.source_path().file_name().unwrap(),
            upgraded_filename
        );
        assert_eq!(
            fs::read(upgraded.source_path()).unwrap(),
            b"new-audio-with-a-different-size"
        );
        assert_eq!(
            fs::read(project_path.join("stems/vocals.wav")).unwrap(),
            b"stem:new-audio-with-a-different-size"
        );
        assert_user_content_preserved(&project_path);
        assert_eq!(upgraded.lyrics().unwrap().as_str(), "lyrics/keep.lrc");
        assert_eq!(upgraded.takes()[0].id(), "keep");
        if upgraded_filename != "song.wav" {
            assert!(!project_path.join("source/song.wav").exists());
        }
    }

    #[cfg(unix)]
    #[test]
    fn failed_import_is_archived_hidden_and_retryable() {
        use std::os::unix::fs::PermissionsExt;

        let sandbox = tempfile::tempdir().unwrap();
        let music = sandbox.path().join("music");
        let projects = sandbox.path().join("projects");
        fs::create_dir_all(&music).unwrap();
        fs::create_dir_all(&projects).unwrap();
        let song = music.join("失败歌曲.wav");
        fs::write(&song, b"audio").unwrap();
        let worker = sandbox.path().join("failing-worker");
        fs::write(
            &worker,
            r#"#!/usr/bin/env python3
import json, sys
r = json.loads(sys.stdin.readline())
print("x" * 10000, file=sys.stderr)
print("模型诊断尾部", file=sys.stderr)
print(json.dumps({"id": r["id"], "ok": False, "error": {
  "code": "forced_failure", "message": "测试分离失败"}}))
"#,
        )
        .unwrap();
        let mut permissions = fs::metadata(&worker).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&worker, permissions).unwrap();
        let config: LibraryConfig = serde_json::from_value(serde_json::json!({
            "music_root": music,
            "projects_root": projects,
            "separation": {
                "worker": worker,
                "log_dir": sandbox.path().join("logs"),
                "profile": "fast"
            }
        }))
        .unwrap();

        let error = import_and_separate(&config, &song).unwrap_err().to_string();
        let snapshot = scan(&config).unwrap();
        let archived = fs::read_dir(projects.join(".failed"))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();

        assert!(error.contains("测试分离失败"));
        assert!(error.contains("模型诊断尾部"));
        assert!(error.len() < 5_000);
        assert!(error.contains(".failed"));
        assert!(!projects.join("失败歌曲").exists());
        assert!(snapshot.projects.is_empty());
        assert_eq!(snapshot.sources.len(), 1);
        assert_eq!(snapshot.sources[0].project_state, SourceProjectState::New);
        assert_eq!(archived.len(), 1);
        assert!(archived[0].path().join("project.json").is_file());

        // 模拟旧版本遗留在项目根目录中的失败 project。
        fs::rename(archived[0].path(), projects.join("失败歌曲")).unwrap();
        let legacy_snapshot = scan(&config).unwrap();
        assert!(legacy_snapshot.projects.is_empty());
        assert_eq!(
            legacy_snapshot.sources[0].project_state,
            SourceProjectState::New
        );

        let retry_error = import_and_separate(&config, &song).unwrap_err().to_string();
        let retry_archived = fs::read_dir(projects.join(".failed"))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert!(retry_error.contains("测试分离失败"));
        assert!(retry_error.contains("existing project and stems were preserved"));
        assert!(projects.join("失败歌曲/project.json").is_file());
        assert!(retry_archived.is_empty());
    }
}
