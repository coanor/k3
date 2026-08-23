use std::fs;

use k3_core::{
    CreateProject, FileProjectRepository, ProjectMutation, ProjectPath, ProjectRepository,
};

#[test]
fn user_can_create_and_reopen_a_project_with_local_media() {
    let sandbox = tempfile::tempdir().unwrap();
    let song = sandbox.path().join("song.mp3");
    let lyrics = sandbox.path().join("song.lrc");
    fs::write(&song, b"audio").unwrap();
    fs::write(&lyrics, b"[00:01.00]hello").unwrap();
    let project_dir = sandbox.path().join("my-song");

    let repository = FileProjectRepository;
    let created = repository
        .create(CreateProject {
            root: project_dir.clone(),
            song,
            lyrics: Some(lyrics),
            title: Some("My Song".into()),
        })
        .unwrap();

    assert_eq!(created.title(), "My Song");
    assert_eq!(
        fs::read(project_dir.join("source/song.mp3")).unwrap(),
        b"audio"
    );
    assert_eq!(
        fs::read(project_dir.join("lyrics/song.lrc")).unwrap(),
        b"[00:01.00]hello"
    );
    for directory in ["source", "stems", "takes", "lyrics", "exports"] {
        assert!(project_dir.join(directory).is_dir());
    }

    let reopened = repository.open(&project_dir).unwrap();
    assert_eq!(created, reopened);
}

#[test]
fn project_creation_refuses_to_overwrite_an_existing_project() {
    let sandbox = tempfile::tempdir().unwrap();
    let song = sandbox.path().join("song.wav");
    fs::write(&song, b"audio").unwrap();
    let root = sandbox.path().join("existing");
    fs::create_dir(&root).unwrap();

    let error = FileProjectRepository
        .create(CreateProject {
            root,
            song,
            lyrics: None,
            title: None,
        })
        .unwrap_err();

    assert!(error.to_string().contains("already exists"));
}

#[test]
fn project_paths_reject_absolute_and_traversing_locations() {
    for unsafe_path in [
        "/tmp/escape.wav",
        "../escape.wav",
        "stems/../escape.wav",
        "C:\\escape.wav",
        "stems\\..\\escape.wav",
        ".",
    ] {
        assert!(
            ProjectPath::new(unsafe_path).is_err(),
            "accepted {unsafe_path}"
        );
    }
}

#[test]
fn project_key_is_validated_and_persisted() {
    let sandbox = tempfile::tempdir().unwrap();
    let song = sandbox.path().join("song.wav");
    fs::write(&song, b"audio").unwrap();
    let root = sandbox.path().join("keyed-song");
    let repository = FileProjectRepository;
    let mut project = repository
        .create(CreateProject {
            root: root.clone(),
            song,
            lyrics: None,
            title: None,
        })
        .unwrap();

    assert_eq!(project.key_shift_semitones(), 0);
    project.set_key_shift_semitones(3).unwrap();
    repository.save(&mut project).unwrap();
    assert_eq!(repository.open(&root).unwrap().key_shift_semitones(), 3);
    assert!(project.set_key_shift_semitones(7).is_err());
    assert_eq!(project.key_shift_semitones(), 3);
}

#[test]
fn narrow_project_updates_preserve_changes_from_other_frontends() {
    let sandbox = tempfile::tempdir().unwrap();
    let song = sandbox.path().join("song.wav");
    fs::write(&song, b"audio").unwrap();
    let root = sandbox.path().join("shared-song");
    let repository = FileProjectRepository;
    repository
        .create(CreateProject {
            root: root.clone(),
            song,
            lyrics: None,
            title: None,
        })
        .unwrap();

    repository
        .apply(&root, ProjectMutation::SetKeyShift(3))
        .unwrap();
    repository
        .apply(&root, ProjectMutation::SetLatencyCompensation(140))
        .unwrap();

    let reopened = repository.open(&root).unwrap();
    assert_eq!(reopened.key_shift_semitones(), 3);
    assert_eq!(reopened.latency_compensation_ms(), 140);
}

#[test]
fn stale_full_project_save_reports_a_conflict_instead_of_losing_changes() {
    let sandbox = tempfile::tempdir().unwrap();
    let song = sandbox.path().join("song.wav");
    fs::write(&song, b"audio").unwrap();
    let root = sandbox.path().join("shared-song");
    let repository = FileProjectRepository;
    repository
        .create(CreateProject {
            root: root.clone(),
            song,
            lyrics: None,
            title: None,
        })
        .unwrap();

    let mut first_frontend = repository.open(&root).unwrap();
    let mut stale_frontend = repository.open(&root).unwrap();
    first_frontend.set_key_shift_semitones(3).unwrap();
    repository.save(&mut first_frontend).unwrap();
    stale_frontend.set_latency_compensation_ms(140);

    let error = repository.save(&mut stale_frontend).unwrap_err();

    assert!(matches!(
        error,
        k3_core::ProjectError::ConcurrentModification(_)
    ));
    let reopened = repository.open(&root).unwrap();
    assert_eq!(reopened.key_shift_semitones(), 3);
    assert_eq!(reopened.latency_compensation_ms(), 0);
}
