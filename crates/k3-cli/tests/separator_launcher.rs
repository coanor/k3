use std::{
    fs,
    io::{BufRead, BufReader, Read},
    path::PathBuf,
    process::{Command, Stdio},
    sync::mpsc,
    thread,
    time::Duration,
};

fn fixture() -> (tempfile::TempDir, PathBuf) {
    let sandbox = tempfile::Builder::new()
        .prefix("K3 启动器 ")
        .tempdir()
        .unwrap();
    let launcher = sandbox.path().join(if cfg!(windows) {
        "k3-separator.exe"
    } else {
        "k3-separator"
    });
    fs::copy(env!("CARGO_BIN_EXE_k3-separator"), &launcher).unwrap();
    let python = sandbox.path().join(if cfg!(windows) {
        "runtime/python/python.exe"
    } else {
        "runtime/python/bin/python3"
    });
    fs::create_dir_all(python.parent().unwrap()).unwrap();
    let compiler = Command::new("rustc")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/launcher_python.rs"
        ))
        .arg("-o")
        .arg(python)
        .output()
        .unwrap();
    assert!(
        compiler.status.success(),
        "{}",
        String::from_utf8_lossy(&compiler.stderr)
    );
    (sandbox, launcher)
}

#[test]
fn launcher_preserves_exit_status_and_explicit_unicode_model_directory() {
    let (_sandbox, launcher) = fixture();
    let output = Command::new(launcher)
        .args(["--model-dir", "模型 目录"])
        .env("PYTHONHOME", "missing-python")
        .env("PYTHONPATH", "missing-packages")
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(42),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).unwrap().trim(),
        "模型 目录"
    );
}

#[test]
fn killing_launcher_terminates_worker_and_closes_stdout() {
    let (sandbox, launcher) = fixture();
    let pid_file = sandbox.path().join("worker.pid");
    let mut child = Command::new(launcher)
        .env("K3_TEST_PID_FILE", &pid_file)
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let (sender, receiver) = mpsc::channel();
    let reader = thread::spawn(move || {
        let mut stdout = BufReader::new(stdout);
        let mut pid = String::new();
        stdout.read_line(&mut pid).unwrap();
        sender.send(pid).unwrap();
        let mut rest = Vec::new();
        stdout.read_to_end(&mut rest).unwrap();
        let _ = sender.send("EOF".into());
    });
    let pid = receiver.recv_timeout(Duration::from_secs(10));
    child.kill().unwrap();
    child.wait().unwrap();
    let eof = receiver.recv_timeout(Duration::from_secs(3));
    // 回归失败时清理测试进程树，避免旧实现留下后台进程。
    if eof.is_err()
        && let Ok(pids) = fs::read_to_string(&pid_file)
    {
        for pid in pids.split_whitespace() {
            #[cfg(windows)]
            let _ = Command::new("taskkill")
                .args(["/F", "/T", "/PID", pid])
                .output();
            #[cfg(unix)]
            let _ = Command::new("kill").args(["-9", pid]).output();
        }
    }
    assert!(pid.is_ok(), "worker did not start: {pid:?}");
    assert_eq!(
        eof.unwrap(),
        "EOF",
        "worker retained the stdout pipe after cancellation"
    );
    reader.join().unwrap();
}
