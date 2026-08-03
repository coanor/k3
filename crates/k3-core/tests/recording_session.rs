mod support;

use k3_core::{ProjectPath, RecordingSession, RecordingState, Take};

#[test]
fn recording_session_records_a_dry_take_through_legal_transitions() {
    let project = support::project_fixture();
    let mut session = RecordingSession::new(project);

    session.arm().unwrap();
    session.start().unwrap();
    session
        .stop(Take::new(
            "take-001",
            ProjectPath::new("takes/take-001-dry.wav").unwrap(),
        ))
        .unwrap();

    assert_eq!(session.state(), RecordingState::Idle);
    assert_eq!(session.project().takes().len(), 1);
    assert_eq!(
        session.project().takes()[0].dry_audio().as_str(),
        "takes/take-001-dry.wav"
    );
}

#[test]
fn recording_session_rejects_invalid_ordering() {
    let project = support::project_fixture();
    let mut session = RecordingSession::new(project);

    let error = session.start().unwrap_err();

    assert!(error.to_string().contains("Idle"));
    assert_eq!(session.state(), RecordingState::Idle);
}
