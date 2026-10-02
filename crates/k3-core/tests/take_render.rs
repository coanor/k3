use std::fs;

use k3_core::{
    CreateProject, FileProjectRepository, Project, ProjectPath, ProjectRepository,
    RecordingSession, Take, VocalEffectPreset,
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
            title: Some("Render".into()),
        })
        .unwrap();
    let mut session = RecordingSession::new(project);
    session.arm().unwrap();
    session.start().unwrap();
    let dry = ProjectPath::new("takes/dry.wav").unwrap();
    let mix = ProjectPath::new("takes/old.wav").unwrap();
    fs::write(dry.resolve(session.project().root()), b"dry").unwrap();
    fs::write(mix.resolve(session.project().root()), b"old").unwrap();
    session
        .stop(Take::new("first", dry).with_mix_audio(mix))
        .unwrap();
    FileProjectRepository.save(session.project_mut()).unwrap();
    (dir, session.project().clone())
}

fn commit(project: &mut Project) -> Option<String> {
    let mix = ProjectPath::new("takes/new.wav").unwrap();
    fs::write(mix.resolve(project.root()), b"new").unwrap();
    FileProjectRepository
        .commit_take_render(project, "first", VocalEffectPreset::Church, mix)
        .unwrap()
}

#[test]
fn commit_preserves_old_mix_referenced_as_another_takes_dry_audio() {
    let (_dir, project) = fixture();
    let mut session = RecordingSession::new(project);
    session.arm().unwrap();
    session.start().unwrap();
    session
        .stop(Take::new(
            "second",
            ProjectPath::new("takes/old.wav").unwrap(),
        ))
        .unwrap();
    FileProjectRepository.save(session.project_mut()).unwrap();

    assert!(commit(session.project_mut()).is_none());

    assert_eq!(
        fs::read(session.project().root().join("takes/old.wav")).unwrap(),
        b"old"
    );
    assert_eq!(
        fs::read(session.project().root().join("takes/dry.wav")).unwrap(),
        b"dry"
    );
}

#[cfg(unix)]
#[test]
fn commit_does_not_clean_old_mix_through_external_directory_link() {
    let (dir, mut project) = fixture();
    let external = dir.path().join("external");
    fs::create_dir(&external).unwrap();
    fs::write(external.join("keep.wav"), b"keep").unwrap();
    std::os::unix::fs::symlink(&external, project.root().join("takes/external")).unwrap();
    project
        .set_take_render(
            "first",
            VocalEffectPreset::Clean,
            ProjectPath::new("takes/external/keep.wav").unwrap(),
        )
        .unwrap();
    FileProjectRepository.save(&mut project).unwrap();

    assert!(commit(&mut project).is_some());

    assert_eq!(fs::read(external.join("keep.wav")).unwrap(), b"keep");
    assert_eq!(
        project.take("first").unwrap().effect_preset(),
        VocalEffectPreset::Church
    );
    assert_eq!(FileProjectRepository.open(project.root()).unwrap(), project);
}
