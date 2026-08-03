use std::fs;

use k3_core::{CreateProject, FileProjectRepository, ProjectPath, ProjectRepository};

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
