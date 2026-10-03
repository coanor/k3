#![cfg(unix)]

use std::{fs, os::unix::fs::PermissionsExt, process::Command};

#[test]
fn shell_wrapper_prints_paths_from_the_persisted_manifest() {
    let sandbox = tempfile::tempdir().unwrap();
    let song = sandbox.path().join("song.wav");
    fs::write(&song, b"audio").unwrap();
    let projects = sandbox.path().join("projects");
    let fake_k3 = sandbox.path().join("fake-k3");
    fs::write(
        &fake_k3,
        r"#!/usr/bin/env -S python3 -I
import json
import pathlib
import sys

args = sys.argv[1:]
root = pathlib.Path(args[args.index('--root' if args[0] == 'new' else '--project') + 1])
if args[0] == 'new':
    root.mkdir(parents=True)
else:
    stems = root / 'stems'
    stems.mkdir()
    details = {
        'vocals': 'stems/vocals-version-1.wav',
        'backing_vocals': 'stems/backing-vocals-version-1.wav',
        'accompaniment': 'stems/accompaniment-version-1.wav',
    }
    for relative in details.values():
        (root / relative).write_bytes(b'audio')
    (root / 'project.json').write_text(json.dumps({
        'separation': {'status': 'ready', 'details': details}
    }), encoding='utf-8')
",
    )
    .unwrap();
    let mut permissions = fs::metadata(&fake_k3).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&fake_k3, permissions).unwrap();

    let python = Command::new("sh")
        .args(["-c", "command -v python3"])
        .output()
        .unwrap();
    assert!(python.status.success());
    let python = String::from_utf8(python.stdout).unwrap();
    let wrapper = concat!(env!("CARGO_MANIFEST_DIR"), "/../../separate.sh");
    let output = Command::new("bash")
        .arg(wrapper)
        .args([
            "-f",
            song.to_str().unwrap(),
            "-d",
            projects.to_str().unwrap(),
        ])
        .env("K3_BIN", &fake_k3)
        .env("K3_PYTHON", python.trim())
        .env("PYTHONHOME", sandbox.path().join("missing-python"))
        .env("PYTHONPATH", sandbox.path().join("missing-packages"))
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let project = projects.join("song");
    let stdout = String::from_utf8(output.stdout).unwrap();
    for (label, name) in [
        ("vocals", "vocals-version-1.wav"),
        ("backing vocals", "backing-vocals-version-1.wav"),
        ("accompaniment", "accompaniment-version-1.wav"),
    ] {
        assert!(
            stdout.contains(&format!(
                "{label}: {}",
                project.join("stems").join(name).display()
            )),
            "missing {label} from wrapper output: {stdout}"
        );
    }
    assert!(!stdout.contains("stems/vocals.wav"));
}

#[test]
fn shell_wrapper_refuses_existing_project_when_no_overwrite_is_set() {
    let sandbox = tempfile::tempdir().unwrap();
    let song = sandbox.path().join("song.wav");
    fs::write(&song, b"audio").unwrap();
    let projects = sandbox.path().join("projects");
    let project = projects.join("song");
    fs::create_dir_all(&project).unwrap();
    fs::write(project.join("project.json"), b"existing project").unwrap();
    let python = Command::new("sh")
        .args(["-c", "command -v python3"])
        .output()
        .unwrap();
    assert!(python.status.success());
    let python = String::from_utf8(python.stdout).unwrap();
    let wrapper = concat!(env!("CARGO_MANIFEST_DIR"), "/../../separate.sh");
    let output = Command::new("bash")
        .arg(wrapper)
        .args([
            "-f",
            song.to_str().unwrap(),
            "-d",
            projects.to_str().unwrap(),
        ])
        .env("K3_BIN", "/bin/true")
        .env("K3_PYTHON", python.trim())
        .env("K3_NO_OVERWRITE", "1")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("refusing to replace stems"));
    assert_eq!(
        fs::read(project.join("project.json")).unwrap(),
        b"existing project"
    );
}
