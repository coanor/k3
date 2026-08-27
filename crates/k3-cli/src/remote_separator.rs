use std::{
    error::Error,
    fmt,
    fs::{self, File},
    io::{Read, Write},
    path::Path,
    thread,
    time::Duration,
};

use k3_core::{
    BackingVocalModelProvenance, CheckpointSha256, FileProjectRepository, ModelProvenance, Project,
    ProjectPath, ProjectRepository, SeparationManifest, SeparationOperation,
    SeparationOutputLayout, SeparationProfile, SeparationState,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

const PROTOCOL_MAJOR: u16 = 1;

pub(crate) enum RemoteJobError {
    Terminal(String),
    Retryable(Box<dyn Error>),
}

impl fmt::Debug for RemoteJobError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, formatter)
    }
}

impl fmt::Display for RemoteJobError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Terminal(message) => formatter.write_str(message),
            Self::Retryable(error) => fmt::Display::fmt(error, formatter),
        }
    }
}

impl Error for RemoteJobError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Terminal(_) => None,
            Self::Retryable(error) => Some(error.as_ref()),
        }
    }
}

pub(crate) fn validate_server_url(url: &str) -> Result<(), Box<dyn Error>> {
    if let Some(authority) = url.strip_prefix("https://").and_then(authority)
        && !authority.is_empty()
        && !authority.contains('@')
    {
        return Ok(());
    }
    if let Some(authority) = url.strip_prefix("http://").and_then(authority) {
        let loopback = authority == "localhost"
            || authority.starts_with("localhost:")
            || authority == "127.0.0.1"
            || authority.starts_with("127.0.0.1:")
            || authority == "[::1]"
            || authority.starts_with("[::1]:");
        if loopback {
            return Ok(());
        }
    }
    Err("remote separator URL must use HTTPS; HTTP is allowed only for loopback".into())
}

fn authority(value: &str) -> Option<&str> {
    value
        .split('/')
        .next()
        .filter(|authority| !authority.is_empty())
}

#[derive(Clone, Debug)]
pub(crate) struct RemoteSeparatorConfig {
    pub(crate) server_profile: String,
    pub(crate) server_url: String,
    pub(crate) token: String,
    pub(crate) model_id: String,
    pub(crate) output_layout: SeparationOutputLayout,
    pub(crate) poll_interval: Duration,
}

pub(crate) struct RemoteSeparator {
    config: RemoteSeparatorConfig,
    api: Box<dyn SeparatorApi>,
}

/// Owns the complete durable remote-separation transition: resume-or-submit,
/// checkpoint the operation, wait, publish, and checkpoint the final state.
pub(crate) struct RemoteSeparationCoordinator {
    separator: RemoteSeparator,
    repository: FileProjectRepository,
}

impl RemoteSeparationCoordinator {
    pub(crate) fn new(config: RemoteSeparatorConfig) -> Self {
        Self {
            separator: RemoteSeparator::new(config),
            repository: FileProjectRepository,
        }
    }

    pub(crate) fn run(
        &self,
        project: &mut Project,
        profile: SeparationProfile,
        overwrite: bool,
    ) -> Result<(), RemoteJobError> {
        if matches!(project.separation(), SeparationState::Ready(_))
            && !overwrite
            && project.separation_operation().is_none()
        {
            return Err(RemoteJobError::retryable_message(
                "project already has stems; pass --overwrite to replace them",
            ));
        }
        let operation = if let Some(operation) = project.separation_operation() {
            if operation.server_profile() != self.separator.config.server_profile {
                return Err(RemoteJobError::retryable_message(
                    "pending separation uses a different server profile",
                ));
            }
            operation.clone()
        } else {
            let operation = self
                .separator
                .submit(&project.source_path(), profile)
                .map_err(RemoteJobError::Retryable)?;
            project
                .start_separation_operation(operation.clone())
                .map_err(|error| RemoteJobError::Retryable(Box::new(error)))?;
            self.repository
                .save(project)
                .map_err(|error| RemoteJobError::Retryable(Box::new(error)))?;
            operation
        };
        match self.separator.wait_and_download(project.root(), &operation) {
            Ok(manifest) => {
                project
                    .finish_separation_operation(manifest)
                    .map_err(|error| RemoteJobError::Retryable(Box::new(error)))?;
                self.repository
                    .save(project)
                    .map_err(|error| RemoteJobError::Retryable(Box::new(error)))?;
                Ok(())
            }
            Err(error @ RemoteJobError::Terminal(_)) => {
                project.fail_separation_operation(error.to_string());
                self.repository
                    .save(project)
                    .map_err(|error| RemoteJobError::Retryable(Box::new(error)))?;
                Err(error)
            }
            Err(error) => Err(error),
        }
    }
}

impl RemoteJobError {
    fn retryable_message(message: impl Into<String>) -> Self {
        Self::Retryable(Box::new(std::io::Error::other(message.into())))
    }
}

impl RemoteSeparator {
    pub(crate) fn new(config: RemoteSeparatorConfig) -> Self {
        let api = Box::new(HttpSeparatorApi::new(&config));
        Self { config, api }
    }

    #[cfg(test)]
    fn with_api(config: RemoteSeparatorConfig, api: Box<dyn SeparatorApi>) -> Self {
        Self { config, api }
    }

    pub(crate) fn submit(
        &self,
        input: &Path,
        profile: SeparationProfile,
    ) -> Result<SeparationOperation, Box<dyn Error>> {
        validate_server_url(&self.config.server_url)?;
        if profile == SeparationProfile::Compatible {
            return Err("remote separator does not support the compatible profile".into());
        }
        let capabilities = self.api.capabilities()?;
        if capabilities.protocol.major != PROTOCOL_MAJOR {
            return Err(format!(
                "separator protocol major {} is incompatible with K3 major {PROTOCOL_MAJOR}",
                capabilities.protocol.major
            )
            .into());
        }
        let metadata = input.metadata()?;
        let input_digest = hash_file(input)?;
        let created = self.api.create_input(&CreateInputRequest {
            filename: input
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("music")
                .to_owned(),
            size_bytes: metadata.len(),
            sha256: input_digest,
        })?;
        if created.upload_required {
            self.api.upload_input(&created.input_id, input)?;
        }
        let job = self.api.create_job(
            &Uuid::new_v4().to_string(),
            &CreateJobRequest {
                input_id: created.input_id.clone(),
                model_id: self.config.model_id.clone(),
                preset: profile_name(profile).to_owned(),
                output_layout: layout_name(self.config.output_layout).to_owned(),
                force: false,
            },
        )?;
        Ok(SeparationOperation::remote(
            self.config.server_profile.clone(),
            created.input_id,
            job.job_id,
            self.config.model_id.clone(),
            profile,
            self.config.output_layout,
        ))
    }

    pub(crate) fn wait_and_download(
        &self,
        project_root: &Path,
        operation: &SeparationOperation,
    ) -> Result<SeparationManifest, RemoteJobError> {
        validate_server_url(&self.config.server_url).map_err(RemoteJobError::Retryable)?;
        loop {
            let job = self
                .api
                .get_job(operation.job_id())
                .map_err(RemoteJobError::Retryable)?;
            match job {
                JobResponse::Queued | JobResponse::Running | JobResponse::Cancelling => {
                    thread::sleep(self.config.poll_interval);
                }
                JobResponse::Completed { result } => {
                    return self
                        .download_result(project_root, operation, &result)
                        .map_err(RemoteJobError::Retryable);
                }
                JobResponse::Cancelled => {
                    return Err(RemoteJobError::Terminal(
                        "separator job was cancelled".into(),
                    ));
                }
                JobResponse::Failed { error } => {
                    let detail = error.map_or_else(
                        || "separator job failed without an error".to_owned(),
                        |error| format!("{}: {}", error.code, error.message),
                    );
                    return Err(RemoteJobError::Terminal(detail));
                }
            }
        }
    }

    fn download_result(
        &self,
        project_root: &Path,
        operation: &SeparationOperation,
        result: &JobResult,
    ) -> Result<SeparationManifest, Box<dyn Error>> {
        let version = format!("stems/{}", operation.job_id());
        let manifest = manifest_from_provenance(operation, result.provenance.clone(), &version)?;
        let stems_dir = project_root.join("stems");
        fs::create_dir_all(&stems_dir)?;
        let staging = stems_dir.join(format!(".remote-{}", operation.job_id()));
        if staging.exists() {
            fs::remove_dir_all(&staging)?;
        }
        fs::create_dir(&staging)?;
        let outcome = (|| {
            let expected_roles: &[(&str, &str)] = match operation.output_layout() {
                SeparationOutputLayout::TwoStem => &[
                    ("vocals", "vocals.wav"),
                    ("accompaniment", "accompaniment.wav"),
                ],
                SeparationOutputLayout::Karaoke => &[
                    ("lead_vocals", "vocals.wav"),
                    ("backing_vocals", "backing-vocals.wav"),
                    ("accompaniment", "accompaniment.wav"),
                ],
            };
            let mut expected_frames = None;
            for (role, filename) in expected_roles {
                let artifact = result
                    .artifacts
                    .iter()
                    .find(|artifact| artifact.role == *role)
                    .ok_or_else(|| format!("separator result omitted {role}"))?;
                if artifact.media_type != "audio/wav" {
                    return Err(format!("separator returned non-WAV {role}").into());
                }
                let temporary = staging.join(filename);
                let mut file = File::create(&temporary)?;
                self.api.download_artifact(&artifact.id, &mut file)?;
                file.flush()?;
                file.sync_all()?;
                if temporary.metadata()?.len() != artifact.size_bytes {
                    return Err(format!("downloaded {role} has the wrong size").into());
                }
                if hash_file(&temporary)? != artifact.sha256 {
                    return Err(format!("downloaded {role} failed SHA-256 validation").into());
                }
                let frames = validate_wav_contract(&temporary, role)?;
                if expected_frames.is_some_and(|expected| expected != frames) {
                    return Err("separator returned stems with different frame counts".into());
                }
                expected_frames = Some(frames);
            }
            publish_stem_set(&staging, &project_root.join(&version), expected_roles)?;
            Ok::<(), Box<dyn Error>>(())
        })();
        if outcome.is_err() {
            fs::remove_dir_all(&staging).ok();
        }
        outcome?;
        Ok(manifest)
    }
}

trait SeparatorApi {
    fn capabilities(&self) -> Result<Capabilities, Box<dyn Error>>;
    fn create_input(&self, request: &CreateInputRequest) -> Result<CreatedInput, Box<dyn Error>>;
    fn upload_input(&self, input_id: &str, path: &Path) -> Result<(), Box<dyn Error>>;
    fn create_job(
        &self,
        idempotency_key: &str,
        request: &CreateJobRequest,
    ) -> Result<CreatedJob, Box<dyn Error>>;
    fn get_job(&self, job_id: &str) -> Result<JobResponse, Box<dyn Error>>;
    fn download_artifact(
        &self,
        artifact_id: &str,
        destination: &mut dyn Write,
    ) -> Result<(), Box<dyn Error>>;
}

struct HttpSeparatorApi {
    base_url: String,
    authorization: String,
    agent: ureq::Agent,
}

impl HttpSeparatorApi {
    fn new(config: &RemoteSeparatorConfig) -> Self {
        let agent_config = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_mins(1)))
            .user_agent(format!("k3/{}", env!("CARGO_PKG_VERSION")))
            .build();
        Self {
            base_url: config.server_url.trim_end_matches('/').to_owned(),
            authorization: format!("Bearer {}", config.token),
            agent: ureq::Agent::new_with_config(agent_config),
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base_url)
    }
}

impl SeparatorApi for HttpSeparatorApi {
    fn capabilities(&self) -> Result<Capabilities, Box<dyn Error>> {
        let mut response = self
            .agent
            .get(self.url("/v1/capabilities"))
            .header("Authorization", &self.authorization)
            .call()?;
        Ok(response.body_mut().read_json()?)
    }

    fn create_input(&self, request: &CreateInputRequest) -> Result<CreatedInput, Box<dyn Error>> {
        let mut response = self
            .agent
            .post(self.url("/v1/inputs"))
            .header("Authorization", &self.authorization)
            .send_json(request)?;
        Ok(response.body_mut().read_json()?)
    }

    fn upload_input(&self, input_id: &str, path: &Path) -> Result<(), Box<dyn Error>> {
        self.agent
            .put(self.url(&format!("/v1/inputs/{input_id}/content")))
            .header("Authorization", &self.authorization)
            .header("Content-Type", "application/octet-stream")
            .send(File::open(path)?)?;
        Ok(())
    }

    fn create_job(
        &self,
        idempotency_key: &str,
        request: &CreateJobRequest,
    ) -> Result<CreatedJob, Box<dyn Error>> {
        let mut response = self
            .agent
            .post(self.url("/v1/jobs"))
            .header("Authorization", &self.authorization)
            .header("Idempotency-Key", idempotency_key)
            .send_json(request)?;
        Ok(response.body_mut().read_json()?)
    }

    fn get_job(&self, job_id: &str) -> Result<JobResponse, Box<dyn Error>> {
        let mut response = self
            .agent
            .get(self.url(&format!("/v1/jobs/{job_id}")))
            .header("Authorization", &self.authorization)
            .call()?;
        Ok(response.body_mut().read_json()?)
    }

    fn download_artifact(
        &self,
        artifact_id: &str,
        destination: &mut dyn Write,
    ) -> Result<(), Box<dyn Error>> {
        let mut response = self
            .agent
            .get(self.url(&format!("/v1/artifacts/{artifact_id}")))
            .header("Authorization", &self.authorization)
            .call()?;
        std::io::copy(&mut response.body_mut().as_reader(), destination)?;
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize)]
struct Capabilities {
    protocol: ProtocolVersion,
}

#[derive(Clone, Debug, Deserialize)]
struct ProtocolVersion {
    major: u16,
}

#[derive(Clone, Debug, Serialize)]
struct CreateInputRequest {
    filename: String,
    size_bytes: u64,
    sha256: String,
}

#[derive(Clone, Debug, Deserialize)]
struct CreatedInput {
    input_id: String,
    upload_required: bool,
}

#[derive(Clone, Debug, Serialize)]
struct CreateJobRequest {
    input_id: String,
    model_id: String,
    preset: String,
    output_layout: String,
    force: bool,
}

#[derive(Clone, Debug, Deserialize)]
struct CreatedJob {
    job_id: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum JobResponse {
    Queued,
    Running,
    Cancelling,
    Completed { result: JobResult },
    Cancelled,
    Failed { error: Option<ApiError> },
}

#[derive(Clone, Debug, Deserialize)]
struct JobResult {
    artifacts: Vec<Artifact>,
    provenance: Provenance,
}

#[derive(Clone, Debug, Deserialize)]
struct Artifact {
    role: String,
    #[serde(rename = "artifact_id")]
    id: String,
    media_type: String,
    size_bytes: u64,
    sha256: String,
}

#[derive(Clone, Debug, Deserialize)]
struct Provenance {
    provider: String,
    architecture: String,
    checkpoint_id: String,
    checkpoint_sha256: String,
    #[serde(default)]
    backing_vocals_model: Option<SecondaryProvenance>,
}

#[derive(Clone, Debug, Deserialize)]
struct SecondaryProvenance {
    provider: String,
    architecture: String,
    checkpoint_id: String,
    checkpoint_sha256: String,
}

#[derive(Clone, Debug, Deserialize)]
struct ApiError {
    code: String,
    message: String,
}

fn profile_name(profile: SeparationProfile) -> &'static str {
    match profile {
        SeparationProfile::Fast => "fast",
        SeparationProfile::Balanced => "balanced",
        SeparationProfile::Quality => "quality",
        SeparationProfile::Compatible => "compatible",
    }
}

fn layout_name(layout: SeparationOutputLayout) -> &'static str {
    match layout {
        SeparationOutputLayout::TwoStem => "two_stem",
        SeparationOutputLayout::Karaoke => "karaoke",
    }
}

fn manifest_from_provenance(
    operation: &SeparationOperation,
    provenance: Provenance,
    stem_directory: &str,
) -> Result<SeparationManifest, Box<dyn Error>> {
    for (name, value) in [
        ("provider", provenance.provider.as_str()),
        ("architecture", provenance.architecture.as_str()),
        ("checkpoint ID", provenance.checkpoint_id.as_str()),
    ] {
        if value.trim().is_empty() {
            return Err(format!("separator returned an empty {name}").into());
        }
    }
    let backing_vocals_model = provenance
        .backing_vocals_model
        .map(|model| {
            Ok::<_, Box<dyn Error>>(Box::new(BackingVocalModelProvenance {
                provider: model.provider,
                architecture: model.architecture,
                checkpoint_id: model.checkpoint_id,
                checkpoint_sha256: CheckpointSha256::new(model.checkpoint_sha256)?,
            }))
        })
        .transpose()?;
    match (operation.output_layout(), backing_vocals_model.is_some()) {
        (SeparationOutputLayout::Karaoke, false) => {
            return Err("karaoke result omitted backing-vocal provenance".into());
        }
        (SeparationOutputLayout::TwoStem, true) => {
            return Err("two-stem result included unexpected backing-vocal provenance".into());
        }
        _ => {}
    }
    Ok(SeparationManifest {
        vocals: ProjectPath::new(format!("{stem_directory}/vocals.wav"))?,
        accompaniment: ProjectPath::new(format!("{stem_directory}/accompaniment.wav"))?,
        backing_vocals: if operation.output_layout() == SeparationOutputLayout::Karaoke {
            Some(ProjectPath::new(format!(
                "{stem_directory}/backing-vocals.wav"
            ))?)
        } else {
            None
        },
        provenance: ModelProvenance {
            provider: provenance.provider,
            architecture: provenance.architecture,
            checkpoint_id: provenance.checkpoint_id,
            checkpoint_sha256: CheckpointSha256::new(provenance.checkpoint_sha256)?,
            profile: operation.profile(),
            backing_vocals_model,
        },
    })
}

fn hash_file(path: &Path) -> Result<String, Box<dyn Error>> {
    let mut file = File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = vec![0_u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn validate_wav_contract(path: &Path, role: &str) -> Result<u32, Box<dyn Error>> {
    let mut reader = hound::WavReader::open(path)
        .map_err(|error| format!("downloaded {role} is an invalid WAV: {error}"))?;
    let spec = reader.spec();
    if spec.sample_format != hound::SampleFormat::Float
        || spec.channels != 2
        || spec.sample_rate != 44_100
        || spec.bits_per_sample != 32
        || reader.duration() == 0
    {
        return Err(format!(
            "downloaded {role} is an invalid WAV; expected 44.1 kHz stereo 32-bit float"
        )
        .into());
    }
    let frames = reader.duration();
    for sample in reader.samples::<f32>() {
        let sample =
            sample.map_err(|error| format!("downloaded {role} has invalid samples: {error}"))?;
        if !sample.is_finite() {
            return Err(format!("downloaded {role} contains non-finite samples").into());
        }
    }
    Ok(frames)
}

fn publish_stem_set(
    staging: &Path,
    destination: &Path,
    roles: &[(&str, &str)],
) -> Result<(), Box<dyn Error>> {
    if destination.exists() {
        for (_, filename) in roles {
            let published = destination.join(filename);
            if !published.is_file() || hash_file(&published)? != hash_file(&staging.join(filename))?
            {
                return Err(format!(
                    "remote stem version already exists with different content: {}",
                    destination.display()
                )
                .into());
            }
        }
        fs::remove_dir_all(staging)?;
        return Ok(());
    }
    fs::rename(staging, destination)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        Artifact, Capabilities, CreateInputRequest, CreateJobRequest, CreatedInput, CreatedJob,
        JobResponse, JobResult, ProtocolVersion, Provenance, RemoteSeparator,
        RemoteSeparatorConfig, SeparatorApi, validate_wav_contract,
    };
    use k3_core::{SeparationOperation, SeparationOutputLayout, SeparationProfile};
    use sha2::{Digest, Sha256};
    use std::{cell::Cell, error::Error, fs, io::Write, path::Path, time::Duration};

    struct FakeApi {
        uploaded: Cell<bool>,
    }

    impl SeparatorApi for FakeApi {
        fn capabilities(&self) -> Result<Capabilities, Box<dyn Error>> {
            Ok(Capabilities {
                protocol: ProtocolVersion { major: 1 },
            })
        }

        fn create_input(
            &self,
            request: &CreateInputRequest,
        ) -> Result<CreatedInput, Box<dyn Error>> {
            assert_eq!(request.filename, "song.flac");
            assert_eq!(request.size_bytes, 9);
            assert_eq!(request.sha256.len(), 64);
            Ok(CreatedInput {
                input_id: "input_test".into(),
                upload_required: true,
            })
        }

        fn upload_input(&self, input_id: &str, path: &Path) -> Result<(), Box<dyn Error>> {
            assert_eq!(input_id, "input_test");
            assert_eq!(fs::read(path)?, b"raw music");
            self.uploaded.set(true);
            Ok(())
        }

        fn create_job(
            &self,
            idempotency_key: &str,
            request: &CreateJobRequest,
        ) -> Result<CreatedJob, Box<dyn Error>> {
            assert!(self.uploaded.get());
            assert!(uuid::Uuid::parse_str(idempotency_key).is_ok());
            assert_eq!(request.input_id, "input_test");
            assert_eq!(request.model_id, "fake-separator");
            assert_eq!(request.preset, "balanced");
            assert_eq!(request.output_layout, "two_stem");
            Ok(CreatedJob {
                job_id: "job_test".into(),
            })
        }

        fn get_job(&self, _job_id: &str) -> Result<JobResponse, Box<dyn Error>> {
            unreachable!()
        }

        fn download_artifact(
            &self,
            _artifact_id: &str,
            _destination: &mut dyn Write,
        ) -> Result<(), Box<dyn Error>> {
            unreachable!()
        }
    }

    #[test]
    fn remote_separator_uploads_a_source_and_returns_durable_job_identity() {
        let sandbox = tempfile::tempdir().unwrap();
        let source = sandbox.path().join("song.flac");
        fs::write(&source, b"raw music").unwrap();
        let config = RemoteSeparatorConfig {
            server_profile: "test-server".into(),
            server_url: "http://127.0.0.1:1".into(),
            token: "test-token".into(),
            model_id: "fake-separator".into(),
            output_layout: SeparationOutputLayout::TwoStem,
            poll_interval: Duration::ZERO,
        };
        let separator = RemoteSeparator::with_api(
            config,
            Box::new(FakeApi {
                uploaded: Cell::new(false),
            }),
        );

        let operation = separator
            .submit(&source, SeparationProfile::Balanced)
            .unwrap();

        assert_eq!(operation.server_profile(), "test-server");
        assert_eq!(operation.input_id(), "input_test");
        assert_eq!(operation.job_id(), "job_test");
    }

    #[test]
    fn job_protocol_rejects_unknown_or_incomplete_terminal_statuses() {
        assert!(serde_json::from_str::<JobResponse>(r#"{"status":"mystery"}"#).is_err());
        assert!(serde_json::from_str::<JobResponse>(r#"{"status":"completed"}"#).is_err());
        assert!(serde_json::from_str::<JobResponse>(r#"{"status":"queued"}"#).is_ok());
    }

    struct InvalidAudioApi {
        bytes: Vec<u8>,
        checkpoint_sha256: String,
    }

    impl SeparatorApi for InvalidAudioApi {
        fn capabilities(&self) -> Result<Capabilities, Box<dyn Error>> {
            unreachable!()
        }

        fn create_input(
            &self,
            _request: &CreateInputRequest,
        ) -> Result<CreatedInput, Box<dyn Error>> {
            unreachable!()
        }

        fn upload_input(&self, _input_id: &str, _path: &Path) -> Result<(), Box<dyn Error>> {
            unreachable!()
        }

        fn create_job(
            &self,
            _idempotency_key: &str,
            _request: &CreateJobRequest,
        ) -> Result<CreatedJob, Box<dyn Error>> {
            unreachable!()
        }

        fn get_job(&self, _job_id: &str) -> Result<JobResponse, Box<dyn Error>> {
            let sha256 = format!("{:x}", Sha256::digest(&self.bytes));
            Ok(JobResponse::Completed {
                result: JobResult {
                    artifacts: ["vocals", "accompaniment"]
                        .into_iter()
                        .map(|role| Artifact {
                            role: role.into(),
                            id: role.into(),
                            media_type: "audio/wav".into(),
                            size_bytes: self.bytes.len() as u64,
                            sha256: sha256.clone(),
                        })
                        .collect(),
                    provenance: Provenance {
                        provider: "fake".into(),
                        architecture: "fake".into(),
                        checkpoint_id: "fake-separator".into(),
                        checkpoint_sha256: self.checkpoint_sha256.clone(),
                        backing_vocals_model: None,
                    },
                },
            })
        }

        fn download_artifact(
            &self,
            _artifact_id: &str,
            destination: &mut dyn Write,
        ) -> Result<(), Box<dyn Error>> {
            destination.write_all(&self.bytes)?;
            Ok(())
        }
    }

    #[test]
    fn invalid_downloaded_wav_does_not_replace_existing_stems() {
        let sandbox = tempfile::tempdir().unwrap();
        let stems = sandbox.path().join("stems");
        fs::create_dir(&stems).unwrap();
        fs::write(stems.join("vocals.wav"), b"old vocals").unwrap();
        fs::write(stems.join("accompaniment.wav"), b"old music").unwrap();
        let config = RemoteSeparatorConfig {
            server_profile: "test-server".into(),
            server_url: "http://127.0.0.1:1".into(),
            token: "test-token".into(),
            model_id: "fake-separator".into(),
            output_layout: SeparationOutputLayout::TwoStem,
            poll_interval: Duration::ZERO,
        };
        let separator = RemoteSeparator::with_api(
            config,
            Box::new(InvalidAudioApi {
                bytes: b"not a wav".to_vec(),
                checkpoint_sha256: "0".repeat(64),
            }),
        );
        let operation = SeparationOperation::remote(
            "test-server".into(),
            "input_test".into(),
            "job_test".into(),
            "fake-separator".into(),
            SeparationProfile::Balanced,
            SeparationOutputLayout::TwoStem,
        );

        let error = separator
            .wait_and_download(sandbox.path(), &operation)
            .unwrap_err()
            .to_string();

        assert!(error.contains("invalid WAV"));
        assert_eq!(fs::read(stems.join("vocals.wav")).unwrap(), b"old vocals");
        assert_eq!(
            fs::read(stems.join("accompaniment.wav")).unwrap(),
            b"old music"
        );
    }

    #[test]
    fn completed_remote_job_publishes_one_immutable_stem_directory() {
        let sandbox = tempfile::tempdir().unwrap();
        let source_wav = sandbox.path().join("valid.wav");
        let mut writer = hound::WavWriter::create(
            &source_wav,
            hound::WavSpec {
                channels: 2,
                sample_rate: 44_100,
                bits_per_sample: 32,
                sample_format: hound::SampleFormat::Float,
            },
        )
        .unwrap();
        writer.write_sample(0.1_f32).unwrap();
        writer.write_sample(0.1_f32).unwrap();
        writer.finalize().unwrap();
        let wav = fs::read(source_wav).unwrap();
        let separator = RemoteSeparator::with_api(
            RemoteSeparatorConfig {
                server_profile: "test-server".into(),
                server_url: "http://127.0.0.1:1".into(),
                token: "test-token".into(),
                model_id: "fake-separator".into(),
                output_layout: SeparationOutputLayout::TwoStem,
                poll_interval: Duration::ZERO,
            },
            Box::new(InvalidAudioApi {
                bytes: wav,
                checkpoint_sha256: "0".repeat(64),
            }),
        );
        let operation = SeparationOperation::remote(
            "test-server".into(),
            "input_test".into(),
            "job_test".into(),
            "fake-separator".into(),
            SeparationProfile::Balanced,
            SeparationOutputLayout::TwoStem,
        );

        let manifest = separator
            .wait_and_download(sandbox.path(), &operation)
            .unwrap();

        assert_eq!(manifest.vocals.as_str(), "stems/job_test/vocals.wav");
        assert_eq!(
            manifest.accompaniment.as_str(),
            "stems/job_test/accompaniment.wav"
        );
        assert!(sandbox.path().join(manifest.vocals.as_str()).is_file());
        assert!(!sandbox.path().join("stems/vocals.wav").exists());
    }

    #[test]
    fn wav_contract_rejects_non_finite_samples() {
        let sandbox = tempfile::tempdir().unwrap();
        let path = sandbox.path().join("non-finite.wav");
        let mut writer = hound::WavWriter::create(
            &path,
            hound::WavSpec {
                channels: 2,
                sample_rate: 44_100,
                bits_per_sample: 32,
                sample_format: hound::SampleFormat::Float,
            },
        )
        .unwrap();
        writer.write_sample(f32::NAN).unwrap();
        writer.write_sample(0.0_f32).unwrap();
        writer.finalize().unwrap();

        let error = validate_wav_contract(&path, "vocals")
            .unwrap_err()
            .to_string();

        assert!(error.contains("non-finite"));
    }

    #[test]
    fn invalid_provenance_does_not_replace_existing_stems() {
        let sandbox = tempfile::tempdir().unwrap();
        let stems = sandbox.path().join("stems");
        fs::create_dir(&stems).unwrap();
        fs::write(stems.join("vocals.wav"), b"old vocals").unwrap();
        fs::write(stems.join("accompaniment.wav"), b"old music").unwrap();
        let mut wav = vec![0_u8; 44];
        wav[0..4].copy_from_slice(b"RIFF");
        wav[8..12].copy_from_slice(b"WAVE");
        wav[12..16].copy_from_slice(b"fmt ");
        wav[20..22].copy_from_slice(&3_u16.to_le_bytes());
        wav[22..24].copy_from_slice(&2_u16.to_le_bytes());
        wav[24..28].copy_from_slice(&44_100_u32.to_le_bytes());
        wav[34..36].copy_from_slice(&32_u16.to_le_bytes());
        let separator = RemoteSeparator::with_api(
            RemoteSeparatorConfig {
                server_profile: "test-server".into(),
                server_url: "http://127.0.0.1:1".into(),
                token: "test-token".into(),
                model_id: "fake-separator".into(),
                output_layout: SeparationOutputLayout::TwoStem,
                poll_interval: Duration::ZERO,
            },
            Box::new(InvalidAudioApi {
                bytes: wav,
                checkpoint_sha256: "invalid".into(),
            }),
        );
        let operation = SeparationOperation::remote(
            "test-server".into(),
            "input_test".into(),
            "job_test".into(),
            "fake-separator".into(),
            SeparationProfile::Balanced,
            SeparationOutputLayout::TwoStem,
        );

        separator
            .wait_and_download(sandbox.path(), &operation)
            .unwrap_err();

        assert_eq!(fs::read(stems.join("vocals.wav")).unwrap(), b"old vocals");
        assert_eq!(
            fs::read(stems.join("accompaniment.wav")).unwrap(),
            b"old music"
        );
    }
}
