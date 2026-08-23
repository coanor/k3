use std::{fs, path::Path};

use k3_app::{LoadedProject, TrackKind};
use k3_core::{CreateProject, FileProjectRepository, ProjectRepository};

#[test]
fn loaded_project_exposes_original_audio_and_synced_lyrics() {
    let directory = tempfile::tempdir().unwrap();
    let project_root = create_project(directory.path());

    let loaded = LoadedProject::open(&project_root).unwrap();

    assert_eq!(loaded.title, "Moon Song");
    assert_eq!(loaded.default_track(), Some(TrackKind::Original));
    assert!(loaded.track(TrackKind::Original).unwrap().available());
    assert_eq!(
        loaded
            .lyrics
            .as_ref()
            .unwrap()
            .lines()
            .iter()
            .map(|line| line.text.as_str())
            .collect::<Vec<_>>(),
        ["first", "second"]
    );
}

fn create_project(root: &Path) -> std::path::PathBuf {
    let song = root.join("moon.wav");
    let lyrics = root.join("moon.lrc");
    fs::write(&song, b"test audio").unwrap();
    fs::write(&lyrics, "[00:01.00]first\n[00:03.00]second\n").unwrap();
    let project_root = root.join("moon");
    FileProjectRepository
        .create(CreateProject {
            root: project_root.clone(),
            song,
            lyrics: Some(lyrics),
            title: Some("Moon Song".into()),
        })
        .unwrap();
    project_root
}
