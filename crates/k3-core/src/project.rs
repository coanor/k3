use std::{
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
    pub checkpoint_sha256: String,
    pub profile: SeparationProfile,
}

/// Files produced by a two-stem separation run.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct SeparationManifest {
    pub vocals: ProjectPath,
    pub accompaniment: ProjectPath,
    pub provenance: ModelProvenance,
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

/// One immutable dry vocal recording.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Take {
    id: String,
    dry_audio: ProjectPath,
}

impl Take {
    #[must_use]
    pub fn new(id: impl Into<String>, dry_audio: ProjectPath) -> Self {
        Self {
            id: id.into(),
            dry_audio,
        }
    }

    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    #[must_use]
    pub fn dry_audio(&self) -> &ProjectPath {
        &self.dry_audio
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

    #[must_use]
    pub fn lyrics(&self) -> Option<&ProjectPath> {
        self.lyrics.as_ref()
    }

    #[must_use]
    pub fn separation(&self) -> &SeparationState {
        &self.separation
    }

    #[must_use]
    pub fn takes(&self) -> &[Take] {
        &self.takes
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
        for path in self
            .takes
            .iter()
            .map(Take::dry_audio)
            .chain(self.lyrics.iter())
            .chain(std::iter::once(&self.source))
        {
            ProjectPath::new(path.as_str())?;
        }
        if let SeparationState::Ready(manifest) = &self.separation {
            ProjectPath::new(manifest.vocals.as_str())?;
            ProjectPath::new(manifest.accompaniment.as_str())?;
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
