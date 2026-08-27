use std::{fs, path::Path};

use k3_app::ProjectLibrary;
use k3_core::{CreateProject, FileProjectRepository, ProjectMutation, ProjectRepository};

#[test]
fn project_library_finds_projects_sorted_by_title() {
    let directory = tempfile::tempdir().unwrap();
    create_project(directory.path(), "zulu", "Zulu");
    create_project(directory.path(), "alpha", "Alpha");
    fs::create_dir(directory.path().join("not-a-project")).unwrap();

    let projects = ProjectLibrary::scan(directory.path()).unwrap();

    assert_eq!(
        projects
            .iter()
            .map(|project| project.title.as_str())
            .collect::<Vec<_>>(),
        ["Alpha", "Zulu"]
    );
}

#[test]
fn project_library_filters_unicode_titles_without_case_sensitivity() {
    let directory = tempfile::tempdir().unwrap();
    create_project(directory.path(), "one", "Neon Moon");
    create_project(directory.path(), "two", "月亮代表我的心");
    let projects = ProjectLibrary::scan(directory.path()).unwrap();

    let english = ProjectLibrary::filter(&projects, "NEON");
    let chinese = ProjectLibrary::filter(&projects, "代表");

    assert_eq!(english[0].title, "Neon Moon");
    assert_eq!(chinese[0].title, "月亮代表我的心");
}

#[test]
fn project_library_revision_changes_with_the_project_document() {
    let directory = tempfile::tempdir().unwrap();
    create_project(directory.path(), "one", "Song");
    let before = ProjectLibrary::scan(directory.path()).unwrap();

    FileProjectRepository
        .apply(
            &directory.path().join("one"),
            ProjectMutation::SetKeyShift(2),
        )
        .unwrap();
    let after = ProjectLibrary::scan(directory.path()).unwrap();

    assert_ne!(before[0].document_revision, after[0].document_revision);
}

#[test]
fn project_library_uses_the_revision_from_the_same_repository_read() {
    let directory = tempfile::tempdir().unwrap();
    create_project(directory.path(), "one", "Song");

    let summary = ProjectLibrary::scan(directory.path()).unwrap().remove(0);
    let loaded = FileProjectRepository
        .open(&directory.path().join("one"))
        .unwrap();

    assert_eq!(
        summary.document_revision.unwrap().as_bytes(),
        loaded.document_revision().unwrap().as_bytes()
    );
}

fn create_project(root: &Path, directory: &str, title: &str) {
    let song = root.join(format!("{directory}.wav"));
    fs::write(&song, b"test audio").unwrap();
    FileProjectRepository
        .create(CreateProject {
            root: root.join(directory),
            song,
            lyrics: None,
            title: Some(title.to_owned()),
        })
        .unwrap();
}
