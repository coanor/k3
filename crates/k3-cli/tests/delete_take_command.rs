use std::{
    fs,
    io::Write,
    process::{Command, Output, Stdio},
};

use k3_core::{
    CreateProject, FileProjectRepository, Project, ProjectPath, ProjectRepository,
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
            title: None,
        })
        .unwrap();
    let mut session = RecordingSession::new(project);
    for id in ["older", "newer"] {
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

fn delete(project: &Project, args: &[&str], answer: &str) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_k3"))
        .arg("delete-take")
        .arg("--project")
        .arg(project.root())
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(answer.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

#[test]
fn enter_no_and_eof_keep_the_recording() {
    let (_dir, project) = fixture();
    let before = fs::read(project.root().join("project.json")).unwrap();
    for answer in ["\n", "n\n", "", "maybe\n"] {
        let output = delete(&project, &[], answer);
        assert!(output.status.success());
        assert!(String::from_utf8_lossy(&output.stdout).contains("Take kept"));
        assert_eq!(
            fs::read(project.root().join("project.json")).unwrap(),
            before
        );
        assert!(project.root().join("takes/newer-dry.wav").exists());
    }
}

#[test]
fn yes_deletes_latest_and_preserves_older_take() {
    let (_dir, project) = fixture();
    let output = delete(&project, &[], "y\n");
    assert!(output.status.success(), "{output:?}");
    let remaining = FileProjectRepository.open(project.root()).unwrap();
    assert_eq!(remaining.takes().len(), 1);
    assert_eq!(remaining.takes()[0].id(), "older");
    assert!(!project.root().join("takes/newer-dry.wav").exists());
    assert!(!project.root().join("takes/newer-mix.wav").exists());
    assert!(project.root().join("takes/older-mix.wav").exists());
}

#[test]
fn explicit_id_and_confirmation_flag_delete_only_that_take() {
    let (_dir, project) = fixture();
    let output = delete(&project, &["--take", "older", "--yes"], "");
    assert!(output.status.success(), "{output:?}");
    let remaining = FileProjectRepository.open(project.root()).unwrap();
    assert_eq!(remaining.takes()[0].id(), "newer");
    assert!(project.root().join("takes/newer-dry.wav").exists());
    assert!(!project.root().join("takes/older-dry.wav").exists());
}

#[test]
fn unknown_take_fails_without_changing_project_or_files() {
    let (_dir, project) = fixture();
    let before = fs::read(project.root().join("project.json")).unwrap();
    let output = delete(&project, &["--take", "missing", "--yes"], "");
    assert!(!output.status.success());
    assert_eq!(
        fs::read(project.root().join("project.json")).unwrap(),
        before
    );
    assert!(project.root().join("takes/older-dry.wav").exists());
    assert!(project.root().join("takes/newer-dry.wav").exists());
}
