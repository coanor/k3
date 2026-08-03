use std::fs;

use k3_core::{CreateProject, FileProjectRepository, Project, ProjectRepository};

pub fn project_fixture() -> Project {
    let sandbox = tempfile::tempdir().unwrap();
    let root = sandbox.keep();
    let song = root.join("song.wav");
    fs::write(&song, b"audio").unwrap();

    FileProjectRepository
        .create(CreateProject {
            root: root.join("project"),
            song,
            lyrics: None,
            title: Some("Fixture".into()),
        })
        .unwrap()
}
