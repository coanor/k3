mod support;

use std::path::Path;

use k3_core::{
    CheckpointSha256, ModelProvenance, ProjectPath, SeparationFailure, SeparationManifest,
    SeparationProfile, SeparationState, SongPreparation, StemSeparator,
};

struct SuccessfulSeparator;

impl StemSeparator for SuccessfulSeparator {
    fn separate(
        &mut self,
        input: &Path,
        profile: SeparationProfile,
    ) -> Result<SeparationManifest, SeparationFailure> {
        assert!(input.ends_with("source/song.wav"));
        assert_eq!(profile, SeparationProfile::Quality);
        Ok(SeparationManifest {
            vocals: ProjectPath::new("stems/vocals.wav").unwrap(),
            accompaniment: ProjectPath::new("stems/accompaniment.wav").unwrap(),
            provenance: ModelProvenance {
                provider: "local-worker".into(),
                architecture: "mel-band-roformer".into(),
                checkpoint_id: "vocals-v1".into(),
                checkpoint_sha256: CheckpointSha256::new("a".repeat(64)).unwrap(),
                profile,
            },
        })
    }
}

struct FailingSeparator;

impl StemSeparator for FailingSeparator {
    fn separate(
        &mut self,
        _input: &Path,
        _profile: SeparationProfile,
    ) -> Result<SeparationManifest, SeparationFailure> {
        Err(SeparationFailure::Worker("model crashed".into()))
    }
}

struct InvalidManifestSeparator;

impl StemSeparator for InvalidManifestSeparator {
    fn separate(
        &mut self,
        _input: &Path,
        profile: SeparationProfile,
    ) -> Result<SeparationManifest, SeparationFailure> {
        Ok(SeparationManifest {
            vocals: ProjectPath::new("stems/vocals.wav").unwrap(),
            accompaniment: ProjectPath::new("exports/not-a-stem.wav").unwrap(),
            provenance: ModelProvenance {
                provider: "broken".into(),
                architecture: "broken".into(),
                checkpoint_id: "broken".into(),
                checkpoint_sha256: CheckpointSha256::new("b".repeat(64)).unwrap(),
                profile,
            },
        })
    }
}

#[test]
fn preparation_records_stems_and_model_provenance() {
    let mut project = support::project_fixture();
    let mut preparation = SongPreparation::new(SuccessfulSeparator);

    preparation
        .prepare(&mut project, SeparationProfile::Quality)
        .unwrap();

    let SeparationState::Ready(manifest) = project.separation() else {
        panic!("expected prepared project")
    };
    assert_eq!(manifest.accompaniment.as_str(), "stems/accompaniment.wav");
    assert_eq!(
        manifest.provenance.checkpoint_sha256.as_str(),
        "a".repeat(64)
    );
}

#[test]
fn preparation_failure_preserves_the_source_and_records_failure() {
    let mut project = support::project_fixture();
    let source = project.source_path();
    let mut preparation = SongPreparation::new(FailingSeparator);

    let error = preparation
        .prepare(&mut project, SeparationProfile::Balanced)
        .unwrap_err();

    assert!(error.to_string().contains("model crashed"));
    assert_eq!(project.source_path(), source);
    assert_eq!(
        project.separation(),
        &SeparationState::Failed {
            message: "model crashed".into()
        }
    );
}

#[test]
fn invalid_worker_manifest_is_recorded_as_a_failure() {
    let mut project = support::project_fixture();
    let mut preparation = SongPreparation::new(InvalidManifestSeparator);

    preparation
        .prepare(&mut project, SeparationProfile::Balanced)
        .unwrap_err();

    assert!(matches!(
        project.separation(),
        SeparationState::Failed { .. }
    ));
}

#[test]
fn preparation_rejects_incomplete_model_provenance() {
    struct IncompleteProvenance;
    impl StemSeparator for IncompleteProvenance {
        fn separate(
            &mut self,
            _input: &Path,
            profile: SeparationProfile,
        ) -> Result<SeparationManifest, SeparationFailure> {
            Ok(SeparationManifest {
                vocals: ProjectPath::new("stems/vocals.wav").unwrap(),
                accompaniment: ProjectPath::new("stems/accompaniment.wav").unwrap(),
                provenance: ModelProvenance {
                    provider: String::new(),
                    architecture: "model".into(),
                    checkpoint_id: "v1".into(),
                    checkpoint_sha256: CheckpointSha256::new("c".repeat(64)).unwrap(),
                    profile,
                },
            })
        }
    }

    let mut project = support::project_fixture();
    let mut preparation = SongPreparation::new(IncompleteProvenance);

    preparation
        .prepare(&mut project, SeparationProfile::Balanced)
        .unwrap_err();

    assert!(matches!(
        project.separation(),
        SeparationState::Failed { .. }
    ));
}

#[test]
fn preparation_cannot_restart_a_completed_project() {
    let mut project = support::project_fixture();
    let mut preparation = SongPreparation::new(SuccessfulSeparator);
    preparation
        .prepare(&mut project, SeparationProfile::Quality)
        .unwrap();

    let error = preparation
        .prepare(&mut project, SeparationProfile::Quality)
        .unwrap_err();

    assert!(matches!(error, SeparationFailure::InvalidState(_)));
    assert!(matches!(project.separation(), SeparationState::Ready(_)));
}
