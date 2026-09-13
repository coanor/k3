use std::{
    fmt,
    fs::{self, File},
    io::Write,
    path::{Component, Path, PathBuf},
};

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use thiserror::Error;
use uuid::Uuid;

const PROJECT_FILE: &str = "project.json";
const PROJECT_DIRECTORIES: [&str; 5] = ["source", "stems", "takes", "lyrics", "exports"];

/// A path stored relative to a project directory.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ProjectPath(String);

impl ProjectPath {
    /// Creates a normalized, safe project-relative path.
    ///
    /// # Errors
    ///
    /// Returns [`ProjectError::UnsafePath`] for empty, absolute, or traversing paths.
    pub fn new(path: impl Into<String>) -> Result<Self, ProjectError> {
        let path = path.into();
        let candidate = Path::new(&path);
        let is_safe = !path.is_empty()
            && !path.contains(['\\', ':'])
            && !candidate.is_absolute()
            && candidate
                .components()
                .all(|part| matches!(part, Component::Normal(_)));
        if !is_safe {
            return Err(ProjectError::UnsafePath(path));
        }
        Ok(Self(path))
    }

    /// Returns the serialized relative path.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn resolve(&self, root: &Path) -> PathBuf {
        root.join(&self.0)
    }
}

impl Serialize for ProjectPath {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for ProjectPath {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(de::Error::custom)
    }
}

/// Validated lowercase-or-uppercase hexadecimal SHA-256 digest.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct CheckpointSha256(String);

impl CheckpointSha256 {
    /// Parses a checkpoint digest.
    ///
    /// # Errors
    ///
    /// Returns [`ProjectError::Invalid`] unless the input contains exactly 64 hexadecimal bytes.
    pub fn new(value: impl Into<String>) -> Result<Self, ProjectError> {
        let value = value.into();
        if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(ProjectError::Invalid(
                "checkpoint SHA-256 must contain 64 hexadecimal characters".into(),
            ));
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Serialize for CheckpointSha256 {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for CheckpointSha256 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(de::Error::custom)
    }
}

/// Quality/cost profile requested from a separation adapter.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SeparationProfile {
    Fast,
    #[default]
    Balanced,
    Quality,
    Compatible,
}

/// Exact model identity used to produce cached stems.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct ModelProvenance {
    pub provider: String,
    pub architecture: String,
    pub checkpoint_id: String,
    pub checkpoint_sha256: CheckpointSha256,
    pub profile: SeparationProfile,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backing_vocals_model: Option<Box<BackingVocalModelProvenance>>,
}

/// Exact secondary model identity used to split lead and backing vocals.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct BackingVocalModelProvenance {
    pub provider: String,
    pub architecture: String,
    pub checkpoint_id: String,
    pub checkpoint_sha256: CheckpointSha256,
}

/// Files produced by a two-stem separation run.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct SeparationManifest {
    pub vocals: ProjectPath,
    pub accompaniment: ProjectPath,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backing_vocals: Option<ProjectPath>,
    pub provenance: ModelProvenance,
}

impl SeparationManifest {
    pub(crate) fn validate(&self) -> Result<(), String> {
        for path in [&self.vocals, &self.accompaniment]
            .into_iter()
            .chain(self.backing_vocals.iter())
        {
            if !path.as_str().starts_with("stems/") {
                return Err(format!("stem is outside stems/: {}", path.as_str()));
            }
        }
        for (name, value) in [
            ("provider", self.provenance.provider.as_str()),
            ("architecture", self.provenance.architecture.as_str()),
            ("checkpoint ID", self.provenance.checkpoint_id.as_str()),
        ] {
            if value.trim().is_empty() {
                return Err(format!("model {name} cannot be empty"));
            }
        }
        if let Some(model) = &self.provenance.backing_vocals_model {
            for (name, value) in [
                ("backing-vocal provider", model.provider.as_str()),
                ("backing-vocal architecture", model.architecture.as_str()),
                ("backing-vocal checkpoint ID", model.checkpoint_id.as_str()),
            ] {
                if value.trim().is_empty() {
                    return Err(format!("model {name} cannot be empty"));
                }
            }
        }
        if self.backing_vocals.is_some() != self.provenance.backing_vocals_model.is_some() {
            return Err("backing-vocal stem and model provenance must both be present".into());
        }
        Ok(())
    }
}

/// Persistent song-preparation state.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "status", content = "details", rename_all = "snake_case")]
pub enum SeparationState {
    #[default]
    NotRequested,
    Running,
    Ready(SeparationManifest),
    Failed {
        message: String,
    },
}

impl fmt::Display for SeparationState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = match self {
            Self::NotRequested => "not requested",
            Self::Running => "running",
            Self::Ready(_) => "ready",
            Self::Failed { .. } => "failed",
        };
        formatter.write_str(label)
    }
}

/// Stable user-facing vocal-effect choices persisted with each take.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum VocalEffectPreset {
    #[default]
    Clean,
    Studio,
    Ktv,
    Theater,
    Church,
}

impl VocalEffectPreset {
    #[must_use]
    pub const fn next(self) -> Self {
        match self {
            Self::Clean => Self::Studio,
            Self::Studio => Self::Ktv,
            Self::Ktv => Self::Theater,
            Self::Theater => Self::Church,
            Self::Church => Self::Clean,
        }
    }

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Clean => "clean",
            Self::Studio => "studio",
            Self::Ktv => "ktv",
            Self::Theater => "theater",
            Self::Church => "church",
        }
    }
}

/// One immutable dry vocal recording plus mutable render choices.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Take {
    id: String,
    dry_audio: ProjectPath,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    mix_audio: Option<ProjectPath>,
    #[serde(default)]
    effect_preset: VocalEffectPreset,
    #[serde(default)]
    rendered_key_semitones: i8,
}

impl Take {
    #[must_use]
    pub fn new(id: impl Into<String>, dry_audio: ProjectPath) -> Self {
        Self {
            id: id.into(),
            dry_audio,
            mix_audio: None,
            effect_preset: VocalEffectPreset::Clean,
            rendered_key_semitones: 0,
        }
    }

    #[must_use]
    pub fn with_mix_audio(mut self, mix_audio: ProjectPath) -> Self {
        self.mix_audio = Some(mix_audio);
        self
    }

    #[must_use]
    pub fn with_mix_audio_at_key(mut self, mix_audio: ProjectPath, semitones: i8) -> Self {
        self.mix_audio = Some(mix_audio);
        self.rendered_key_semitones = semitones;
        self
    }

    #[must_use]
    pub fn with_effect_preset(mut self, effect_preset: VocalEffectPreset) -> Self {
        self.effect_preset = effect_preset;
        self
    }

    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    #[must_use]
    pub fn dry_audio(&self) -> &ProjectPath {
        &self.dry_audio
    }

    #[must_use]
    pub fn mix_audio(&self) -> Option<&ProjectPath> {
        self.mix_audio.as_ref()
    }

    #[must_use]
    pub fn effect_preset(&self) -> VocalEffectPreset {
        self.effect_preset
    }

    #[must_use]
    pub fn rendered_key_semitones(&self) -> i8 {
        self.rendered_key_semitones
    }
}

/// Persisted karaoke project plus its location on disk.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Project {
    #[serde(skip)]
    root: PathBuf,
    schema_version: u32,
    id: Uuid,
    title: String,
    source: ProjectPath,
    lyrics: Option<ProjectPath>,
    separation: SeparationState,
    takes: Vec<Take>,
    latency_compensation_ms: i32,
    #[serde(default)]
    key_shift_semitones: i8,
    effects_schema_version: u32,
}

impl Project {
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    #[must_use]
    pub fn id(&self) -> Uuid {
        self.id
    }

    #[must_use]
    pub fn title(&self) -> &str {
        &self.title
    }

    #[must_use]
    pub fn source(&self) -> &ProjectPath {
        &self.source
    }

    #[must_use]
    pub fn source_path(&self) -> PathBuf {
        self.source.resolve(&self.root)
    }

    /// 更新 project 内部复制的原始音源路径。
    ///
    /// 调用方必须先把新音源放入 project 的 `source/` 目录；此方法只更新持久化引用，
    /// 不会触碰歌词、takes 或其他用户内容。
    ///
    /// # Errors
    ///
    /// 当路径不位于 `source/` 目录下时返回 [`ProjectError::Invalid`]。
    pub fn replace_source(&mut self, source: ProjectPath) -> Result<(), ProjectError> {
        if !source.as_str().starts_with("source/") {
            return Err(ProjectError::Invalid(
                "source must be stored below source/".into(),
            ));
        }
        self.source = source;
        Ok(())
    }

    #[must_use]
    pub fn lyrics(&self) -> Option<&ProjectPath> {
        self.lyrics.as_ref()
    }

    /// Attaches a synchronized lyric file stored inside the project `lyrics/` directory.
    ///
    /// # Errors
    ///
    /// Returns [`ProjectError::Invalid`] when the path is outside `lyrics/`.
    pub fn set_lyrics(&mut self, lyrics: ProjectPath) -> Result<(), ProjectError> {
        if !lyrics.as_str().starts_with("lyrics/") {
            return Err(ProjectError::Invalid(
                "lyrics must be stored below lyrics/".into(),
            ));
        }
        self.lyrics = Some(lyrics);
        Ok(())
    }

    #[must_use]
    pub fn separation(&self) -> &SeparationState {
        &self.separation
    }

    #[must_use]
    pub fn takes(&self) -> &[Take] {
        &self.takes
    }

    #[must_use]
    pub fn take(&self, take_id: &str) -> Option<&Take> {
        self.takes.iter().find(|take| take.id == take_id)
    }

    #[must_use]
    pub fn latency_compensation_ms(&self) -> i32 {
        self.latency_compensation_ms
    }

    /// Sets how far a recorded voice is advanced when rendering a take mix.
    pub fn set_latency_compensation_ms(&mut self, milliseconds: i32) {
        self.latency_compensation_ms = milliseconds;
    }

    /// Returns the project-wide backing-track pitch shift in semitones.
    #[must_use]
    pub fn key_shift_semitones(&self) -> i8 {
        self.key_shift_semitones
    }

    /// Sets the project-wide pitch shift used for playback and rendered backing tracks.
    ///
    /// # Errors
    ///
    /// Returns [`ProjectError::Invalid`] unless the shift is between -6 and +6 semitones.
    pub fn set_key_shift_semitones(&mut self, semitones: i8) -> Result<(), ProjectError> {
        if !(-6..=6).contains(&semitones) {
            return Err(ProjectError::Invalid(
                "key shift must be between -6 and +6 semitones".into(),
            ));
        }
        self.key_shift_semitones = semitones;
        Ok(())
    }

    /// Records a successfully rendered mix and the preset used to create it.
    ///
    /// # Errors
    ///
    /// Returns `ProjectError::TakeNotFound` when the ID is not part of the project.
    pub fn set_take_render(
        &mut self,
        take_id: &str,
        preset: VocalEffectPreset,
        mix_audio: ProjectPath,
    ) -> Result<(), ProjectError> {
        let rendered_key_semitones = self.key_shift_semitones;
        let take = self
            .takes
            .iter_mut()
            .find(|take| take.id == take_id)
            .ok_or_else(|| ProjectError::TakeNotFound(take_id.to_owned()))?;
        take.effect_preset = preset;
        take.mix_audio = Some(mix_audio);
        take.rendered_key_semitones = rendered_key_semitones;
        Ok(())
    }

    pub(crate) fn set_separation(&mut self, state: SeparationState) {
        self.separation = state;
    }

    pub(crate) fn add_take(&mut self, take: Take) {
        self.takes.push(take);
    }

    fn validate(&self) -> Result<(), ProjectError> {
        if self.schema_version != 1 {
            return Err(ProjectError::UnsupportedSchema(self.schema_version));
        }
        if self.title.trim().is_empty() {
            return Err(ProjectError::Invalid(
                "project title cannot be empty".into(),
            ));
        }
        if !(-6..=6).contains(&self.key_shift_semitones) {
            return Err(ProjectError::Invalid(
                "key shift must be between -6 and +6 semitones".into(),
            ));
        }
        for path in self
            .takes
            .iter()
            .map(Take::dry_audio)
            .chain(self.takes.iter().filter_map(Take::mix_audio))
            .chain(self.lyrics.iter())
            .chain(std::iter::once(&self.source))
        {
            ProjectPath::new(path.as_str())?;
        }
        if let SeparationState::Ready(manifest) = &self.separation {
            manifest.validate().map_err(ProjectError::Invalid)?;
        }
        Ok(())
    }
}

/// Inputs needed to create a new local project.
#[derive(Clone, Debug)]
pub struct CreateProject {
    pub root: PathBuf,
    pub song: PathBuf,
    pub lyrics: Option<PathBuf>,
    pub title: Option<String>,
}

/// Persistence interface used by the application and its tests.
pub trait ProjectRepository {
    /// Creates a project without overwriting an existing directory.
    ///
    /// # Errors
    ///
    /// Returns an error when inputs are unreadable, the destination exists, or persistence fails.
    fn create(&self, request: CreateProject) -> Result<Project, ProjectError>;
    /// Opens and validates an existing project.
    ///
    /// # Errors
    ///
    /// Returns an error when the project cannot be read, decoded, or validated.
    fn open(&self, project_dir: &Path) -> Result<Project, ProjectError>;
    /// Atomically saves a valid project document.
    ///
    /// # Errors
    ///
    /// Returns an error when validation, serialization, or filesystem operations fail.
    fn save(&self, project: &Project) -> Result<(), ProjectError>;
}

/// JSON and filesystem implementation of [`ProjectRepository`].
#[derive(Clone, Copy, Debug, Default)]
pub struct FileProjectRepository;

impl ProjectRepository for FileProjectRepository {
    fn create(&self, request: CreateProject) -> Result<Project, ProjectError> {
        if request.root.exists() {
            return Err(ProjectError::AlreadyExists(request.root));
        }
        ensure_readable_file(&request.song)?;
        if let Some(lyrics) = &request.lyrics {
            ensure_readable_file(lyrics)?;
        }

        for directory in PROJECT_DIRECTORIES {
            fs::create_dir_all(request.root.join(directory))?;
        }
        let root = request.root.canonicalize()?;
        let source = copy_media(&request.song, &root, "source")?;
        let lyrics = request
            .lyrics
            .as_deref()
            .map(|path| copy_media(path, &root, "lyrics"))
            .transpose()?;
        let inferred_title = request
            .song
            .file_stem()
            .and_then(|name| name.to_str())
            .unwrap_or("Untitled")
            .to_owned();
        let project = Project {
            root,
            schema_version: 1,
            id: Uuid::new_v4(),
            title: request.title.unwrap_or(inferred_title),
            source,
            lyrics,
            separation: SeparationState::NotRequested,
            takes: Vec::new(),
            latency_compensation_ms: 0,
            key_shift_semitones: 0,
            effects_schema_version: 1,
        };
        project.validate()?;
        self.save(&project)?;
        Ok(project)
    }

    fn open(&self, project_dir: &Path) -> Result<Project, ProjectError> {
        let root = project_dir.canonicalize()?;
        let input = fs::read(root.join(PROJECT_FILE))?;
        let mut project: Project = serde_json::from_slice(&input)?;
        project.root = root;
        project.validate()?;
        Ok(project)
    }

    fn save(&self, project: &Project) -> Result<(), ProjectError> {
        project.validate()?;
        let destination = project.root.join(PROJECT_FILE);
        let temporary = project.root.join("project.json.tmp");
        let encoded = serde_json::to_vec_pretty(project)?;
        let mut file = File::create(&temporary)?;
        file.write_all(&encoded)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        fs::rename(temporary, destination)?;
        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum ProjectError {
    #[error("project directory already exists: {0}")]
    AlreadyExists(PathBuf),
    #[error("unsafe project-relative path: {0}")]
    UnsafePath(String),
    #[error("unsupported project schema version: {0}")]
    UnsupportedSchema(u32),
    #[error("invalid project: {0}")]
    Invalid(String),
    #[error("take is not part of this project: {0}")]
    TakeNotFound(String),
    #[error("media file is not readable: {0}")]
    MissingMedia(PathBuf),
    #[error("path is not valid UTF-8: {0}")]
    NonUtf8Path(PathBuf),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

fn ensure_readable_file(path: &Path) -> Result<(), ProjectError> {
    if !path.is_file() {
        return Err(ProjectError::MissingMedia(path.to_path_buf()));
    }
    File::open(path)?;
    Ok(())
}

fn copy_media(source: &Path, root: &Path, directory: &str) -> Result<ProjectPath, ProjectError> {
    let filename = source
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| ProjectError::NonUtf8Path(source.to_path_buf()))?;
    let relative = ProjectPath::new(format!("{directory}/{filename}"))?;
    let destination = relative.resolve(root);
    if destination.exists() {
        return Err(ProjectError::AlreadyExists(destination));
    }
    fs::copy(source, destination)?;
    Ok(relative)
}
