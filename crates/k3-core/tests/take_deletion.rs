use std::fs;

use k3_core::{
    CreateProject, FileProjectRepository, Project, ProjectError, ProjectPath, ProjectRepository,
    RecordingSession, Take,
};

fn fixture() -> (tempfile::TempDir, Project) {
    let dir = tempfile::tempdir().unwrap();
    let song = dir.path().join("song.wav");
    fs::write(&song, b"source").unwrap();
    let project = FileProjectRepository
        .create(CreateProject {
            root: dir.path().join("project"),
            song,
            lyrics: None,
            title: Some("Deletion".into()),
        })
        .unwrap();
    let mut session = RecordingSession::new(project);
    for id in ["first", "second"] {
        session.arm().unwrap();
        session.start().unwrap();
        let dry = ProjectPath::new(format!("takes/{id}-dry.wav")).unwrap();
        let mix = ProjectPath::new(format!("takes/{id}-mix.wav")).unwrap();
        fs::write(dry.resolve(session.project().root()), id).unwrap();
        fs::write(mix.resolve(session.project().root()), id).unwrap();
        session
            .stop(Take::new(id, dry).with_mix_audio(mix))
            .unwrap();
    }
    FileProjectRepository.save(session.project_mut()).unwrap();
    (dir, session.project().clone())
}

#[test]
fn deletes_only_selected_take_and_its_files() {
    let (_dir, project) = fixture();
    fs::write(project.root().join("takes/unrelated.wav"), b"keep").unwrap();
    let deleted = FileProjectRepository
        .delete_take(project.root(), "second", project.document_revision())
        .unwrap();
    assert!(deleted.cleanup_warning.is_none());
    assert_eq!(deleted.project.takes().len(), 1);
    assert_eq!(deleted.project.takes()[0].id(), "first");
    assert!(!project.root().join("takes/second-dry.wav").exists());
    assert!(!project.root().join("takes/second-mix.wav").exists());
    assert_eq!(
        fs::read(project.root().join("takes/first-dry.wav")).unwrap(),
        b"first"
    );
    assert_eq!(
        fs::read(project.root().join("takes/unrelated.wav")).unwrap(),
        b"keep"
    );
    assert_eq!(fs::read(project.source_path()).unwrap(), b"source");
    assert_eq!(
        FileProjectRepository
            .open(project.root())
            .unwrap()
            .takes()
            .len(),
        1
    );
}

#[test]
fn stale_frontend_cannot_delete_after_another_writer_saves() {
    let (_dir, stale) = fixture();
    let mut newer = FileProjectRepository.open(stale.root()).unwrap();
    newer.set_latency_compensation_ms(42);
    FileProjectRepository.save(&mut newer).unwrap();
    let before = fs::read(stale.root().join("project.json")).unwrap();
    assert!(matches!(
        FileProjectRepository.delete_take(stale.root(), "second", stale.document_revision()),
        Err(ProjectError::ConcurrentModification(_))
    ));
    assert_eq!(fs::read(stale.root().join("project.json")).unwrap(), before);
    assert!(stale.root().join("takes/second-dry.wav").exists());
    assert!(stale.root().join("takes/second-mix.wav").exists());
}

#[test]
fn restores_audio_when_project_commit_fails() {
    let (_dir, project) = fixture();
    let before = fs::read(project.root().join("project.json")).unwrap();
    fs::create_dir(project.root().join("project.json.tmp")).unwrap();
    assert!(
        FileProjectRepository
            .delete_take(project.root(), "second", None)
            .is_err()
    );
    assert_eq!(
        fs::read(project.root().join("project.json")).unwrap(),
        before
    );
    for kind in ["dry", "mix"] {
        assert_eq!(
            fs::read(project.root().join(format!("takes/second-{kind}.wav"))).unwrap(),
            b"second"
        );
    }
    assert!(fs::read_dir(project.root()).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".take-delete-")
    }));
}

#[test]
fn keeps_shared_audio_and_removes_a_last_take_with_missing_media() {
    let (_dir, mut project) = fixture();
    let mut document: serde_json::Value =
        serde_json::from_slice(&fs::read(project.root().join("project.json")).unwrap()).unwrap();
    document["takes"][1]["dry_audio"] = document["takes"][0]["dry_audio"].clone();
    document["takes"][1]["mix_audio"] = document["takes"][0]["mix_audio"].clone();
    fs::write(
        project.root().join("project.json"),
        serde_json::to_vec(&document).unwrap(),
    )
    .unwrap();
    project = FileProjectRepository.open(project.root()).unwrap();
    let remaining = FileProjectRepository
        .delete_take(project.root(), "second", None)
        .unwrap()
        .project;
    assert!(project.root().join("takes/first-dry.wav").exists());
    assert!(project.root().join("takes/first-mix.wav").exists());
    fs::remove_file(project.root().join("takes/first-dry.wav")).unwrap();
    fs::remove_file(project.root().join("takes/first-mix.wav")).unwrap();
    assert!(
        FileProjectRepository
            .delete_take(project.root(), "first", remaining.document_revision())
            .unwrap()
            .project
            .takes()
            .is_empty()
    );
}

#[cfg(unix)]
#[test]
fn rejects_audio_in_a_redirected_takes_directory() {
    let (dir, project) = fixture();
    let outside = dir.path().join("outside");
    fs::rename(project.root().join("takes"), &outside).unwrap();
    std::os::unix::fs::symlink(&outside, project.root().join("takes")).unwrap();
    assert!(
        FileProjectRepository
            .delete_take(project.root(), "second", None)
            .is_err()
    );
    assert_eq!(fs::read(outside.join("second-dry.wav")).unwrap(), b"second");
    assert_eq!(
        FileProjectRepository
            .open(project.root())
            .unwrap()
            .takes()
            .len(),
        2
    );
}
