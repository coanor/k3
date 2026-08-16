#![cfg(unix)]

use std::{fs, os::unix::fs::PermissionsExt, process::Command};

use k3_core::{
    CreateProject, FileProjectRepository, ProjectRepository, SeparationProfile, SeparationState,
};

#[test]
fn separate_command_runs_worker_and_persists_ready_project() {
    let (sandbox, project_root) = project_fixture("Worker integration");

    let worker = sandbox.path().join("fake-worker");
    fs::write(
        &worker,
        r#"#!/usr/bin/env python3
import json
import pathlib
import sys

request = json.loads(sys.stdin.readline())
print("MODEL_PROGRESS_MUST_NOT_REACH_TERMINAL", file=sys.stderr)
output = pathlib.Path(request["params"]["output_dir"])
output.mkdir(parents=True, exist_ok=True)
vocals = output / "vocals.wav"
backing = output / "backing-vocals.wav"
accompaniment = output / "accompaniment.wav"
vocals.write_bytes(b"vocals")
backing.write_bytes(b"backing vocals")
accompaniment.write_bytes(b"accompaniment plus backing")
(output / "request.json").write_text(json.dumps(request), encoding="utf-8")
print(json.dumps({
    "id": request["id"],
    "ok": True,
    "result": {
        "vocals": str(vocals),
        "backing_vocals": str(backing),
        "accompaniment": str(accompaniment),
        "provenance": {
            "provider": "fake-python",
            "architecture": "fake-roformer",
            "checkpoint_id": request["params"]["model_id"],
            "checkpoint_sha256": "a" * 64,
            "profile": request["params"]["profile"],
            "backing_vocals_model": {
                "provider": "fake-python",
                "architecture": "mdx-net",
                "checkpoint_id": "uvr-mdx-karaoke-2",
                "checkpoint_sha256": "b" * 64
            }
        },
    },
}))
"#,
    )
    .unwrap();
    make_executable(&worker);
    let log_dir = sandbox.path().join("logs");
    fs::create_dir_all(&log_dir).unwrap();
    fs::write(log_dir.join("separate.log"), "旧分离日志").unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_k3"))
        .env("K3_LOG_DIR", &log_dir)
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
    assert!(
        !String::from_utf8_lossy(&output.stderr).contains("MODEL_PROGRESS_MUST_NOT_REACH_TERMINAL"),
        "worker stderr leaked into the K3 terminal"
    );
    assert_eq!(
        fs::read_to_string(log_dir.join("separate.log")).unwrap(),
        "MODEL_PROGRESS_MUST_NOT_REACH_TERMINAL\n"
    );
    let project = FileProjectRepository.open(&project_root).unwrap();
    let SeparationState::Ready(manifest) = project.separation() else {
        panic!("expected ready separation state")
    };
    assert_eq!(manifest.provenance.provider, "fake-python");
    assert_eq!(manifest.provenance.checkpoint_id, "chosen-model");
    assert_eq!(manifest.provenance.profile, SeparationProfile::Quality);

    let request = read_json(&project_root.join("stems/request.json"));
    assert_eq!(request["params"]["options"]["segment_size"], 128);
    assert_eq!(request["params"]["options"]["autocast"], true);
    assert_eq!(request["params"]["preserve_backing_vocals"], true);

    assert_overwrite_rerun(&project_root, &worker, &log_dir);
}

#[test]
fn separate_command_can_disable_backing_vocal_preservation() {
    let sandbox = tempfile::tempdir().unwrap();
    let source = sandbox.path().join("song.wav");
    fs::write(&source, b"source audio").unwrap();
    let project_root = sandbox.path().join("project");
    FileProjectRepository
        .create(CreateProject {
            root: project_root.clone(),
            song: source,
            lyrics: None,
            title: Some("Backing vocals integration".into()),
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
if request["params"].get("preserve_backing_vocals") is not False:
    print(json.dumps({"id": request["id"], "ok": False, "error": {
        "code": "wrong_mode", "message": "preserve mode was not disabled"}}))
    raise SystemExit()
output = pathlib.Path(request["params"]["output_dir"])
output.mkdir(parents=True, exist_ok=True)
vocals = output / "vocals.wav"
accompaniment = output / "accompaniment.wav"
vocals.write_bytes(b"lead vocals")
accompaniment.write_bytes(b"plain accompaniment")
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
            "checkpoint_id": "primary-model",
            "checkpoint_sha256": "a" * 64,
            "profile": request["params"]["profile"]
        }
    }
}))
"#,
    )
    .unwrap();
    make_executable(&worker);

    let output = Command::new(env!("CARGO_BIN_EXE_k3"))
        .args([
            "separate",
            "--project",
            project_root.to_str().unwrap(),
            "--profile",
            "quality",
            "--model",
            "primary-model",
            "--worker",
            worker.to_str().unwrap(),
            "--no-preserve-backing-vocals",
        ])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let request = read_json(&project_root.join("stems/request.json"));
    assert_eq!(request["params"]["preserve_backing_vocals"], false);
    let project: serde_json::Value =
        serde_json::from_slice(&fs::read(project_root.join("project.json")).unwrap()).unwrap();
    let details = &project["separation"]["details"];
    assert!(details.get("backing_vocals").is_none());
    assert!(details["provenance"].get("backing_vocals_model").is_none());
}

fn make_executable(path: &std::path::Path) {
    let mut permissions = fs::metadata(path).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).unwrap();
}

fn project_fixture(title: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let sandbox = tempfile::tempdir().unwrap();
    let source = sandbox.path().join("song.wav");
    fs::write(&source, b"source audio").unwrap();
    let project_root = sandbox.path().join("project");
    FileProjectRepository
        .create(CreateProject {
            root: project_root.clone(),
            song: source,
            lyrics: None,
            title: Some(title.into()),
        })
        .unwrap();
    (sandbox, project_root)
}

fn assert_overwrite_rerun(
    project_root: &std::path::Path,
    worker: &std::path::Path,
    log_dir: &std::path::Path,
) {
    let rerun = Command::new(env!("CARGO_BIN_EXE_k3"))
        .env("K3_LOG_DIR", log_dir)
        .args([
            "separate",
            "--project",
            project_root.to_str().unwrap(),
            "--profile",
            "quality",
            "--model",
            "replacement-model",
            "--worker",
            worker.to_str().unwrap(),
            "--overwrite",
        ])
        .output()
        .unwrap();
    assert!(
        rerun.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&rerun.stderr)
    );
    let replaced = FileProjectRepository.open(project_root).unwrap();
    let SeparationState::Ready(manifest) = replaced.separation() else {
        panic!("expected replaced separation state")
    };
    assert_eq!(manifest.provenance.checkpoint_id, "replacement-model");
    let request = read_json(&project_root.join("stems/request.json"));
    assert_eq!(request["params"]["overwrite"], true);
}

fn read_json(path: &std::path::Path) -> serde_json::Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}
