use std::{fs, time::Duration, time::Instant};

use k3_app::ProjectLibrary;
use k3_core::{CreateProject, FileProjectRepository, ProjectRepository};

#[test]
fn thousand_project_library_scan_stays_within_the_first_screen_budget() {
    let sandbox = tempfile::tempdir().unwrap();
    let song = sandbox.path().join("fixture.wav");
    fs::write(&song, b"audio").unwrap();
    for index in 0..1_000 {
        FileProjectRepository
            .create(CreateProject {
                root: sandbox.path().join(format!("project-{index:04}")),
                song: song.clone(),
                lyrics: None,
                title: Some(format!("Project {index:04}")),
            })
            .unwrap();
    }

    let started = Instant::now();
    let projects = ProjectLibrary::scan(sandbox.path()).unwrap();

    assert_eq!(projects.len(), 1_000);
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "1000-project scan took {:?}",
        started.elapsed()
    );
}
