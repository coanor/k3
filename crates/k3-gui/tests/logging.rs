use std::fs;

use k3_gui::logging::DiagnosticLog;

#[test]
fn diagnostic_log_rotates_a_full_previous_log() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("k3-gui.log");
    fs::write(&path, vec![b'x'; 1_048_576]).unwrap();

    let log = DiagnosticLog::open_at(path.clone()).unwrap();
    log.record("GUI started");

    assert!(directory.path().join("k3-gui.log.1").is_file());
    assert_eq!(fs::read_to_string(path).unwrap().lines().count(), 1);
}
