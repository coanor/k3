#![cfg(unix)]

use std::{fs, os::unix::fs::PermissionsExt, process::Command};

use k3_core::{
    CreateProject, FileProjectRepository, ProjectRepository, SeparationProfile, SeparationState,
};

#[test]
fn separate_command_runs_worker_and_persists_ready_project() {
    let sandbox = tempfile::tempdir().unwrap();
    let source = sandbox.path().join("song.wav");
    fs::write(&source, b"source audio").unwrap();
    let project_root = sandbox.path().join("project");
    FileProjectRepository
        .create(CreateProject {
            root: project_root.clone(),
            song: source,
            lyrics: None,
            title: Some("Worker integration".into()),
        })
        .unwrap();

    let worker = sandbox.path().join("fake-worker");
    fs::write(
        &worker,
        r#"#!/usr/bin/env python3
import json
import pathlib
import sys

request = json.loads(sys.stdin.readline())
output = pathlib.Path(request["params"]["output_dir"])
output.mkdir(parents=True, exist_ok=True)
vocals = output / "vocals.wav"
accompaniment = output / "accompaniment.wav"
vocals.write_bytes(b"vocals")
accompaniment.write_bytes(b"accompaniment")
(output / "request.json").write_text(json.dumps(request), encoding="utf-8")
print(json.dumps({
    "id": request["id"],
    "ok": True,
    "result": {
        "vocals": str(vocals),
        "accompaniment": str(accompaniment),
        "provenance": {
            "provider": "fake-python",
            "architecture": "fake-roformer",
            "checkpoint_id": request["params"]["model_id"],
            "checkpoint_sha256": "a" * 64,
            "profile": request["params"]["profile"],
        },
    },
}))
"#,
    )
    .unwrap();
    let mut permissions = fs::metadata(&worker).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&worker, permissions).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_k3"))
        .args([
            "separate",
            "--project",
            project_root.to_str().unwrap(),
            "--profile",
            "quality",
            "--model",
            "chosen-model",
            "--worker",
            worker.to_str().unwrap(),
            "--segment-size",
            "128",
        ])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let project = FileProjectRepository.open(&project_root).unwrap();
    let SeparationState::Ready(manifest) = project.separation() else {
        panic!("expected ready separation state")
    };
    assert_eq!(manifest.provenance.provider, "fake-python");
    assert_eq!(manifest.provenance.checkpoint_id, "chosen-model");
    assert_eq!(manifest.provenance.profile, SeparationProfile::Quality);

    let request: serde_json::Value =
        serde_json::from_slice(&fs::read(project_root.join("stems/request.json")).unwrap())
            .unwrap();
    assert_eq!(request["params"]["options"]["segment_size"], 128);
    assert_eq!(request["params"]["options"]["autocast"], true);
}
