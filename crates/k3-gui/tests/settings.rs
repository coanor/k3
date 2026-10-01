use std::path::PathBuf;

use k3_core::VocalEffectPreset;
use k3_gui::settings::{GuiSettings, RecordingSettings, WindowSize};

#[test]
fn gui_settings_round_trip_without_reusing_tui_configuration() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("gui.json");
    let expected = GuiSettings {
        projects_root: Some(PathBuf::from("/music/k3-projects")),
        volume: 0.72,
        recording: RecordingSettings {
            default_effect: VocalEffectPreset::Church,
        },
        window: WindowSize {
            width: 1280,
            height: 800,
        },
        last_project_id: None,
        ..GuiSettings::default()
    };

    expected.save_to(&path).unwrap();

    assert_eq!(GuiSettings::load_from(&path).unwrap(), expected);
}

#[test]
fn existing_gui_settings_default_to_clean_recordings() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("gui.json");
    std::fs::write(
        &path,
        r#"{"schema_version":1,"projects_root":null,"volume":1.0,"window":{"width":1280,"height":800},"last_project_id":null}"#,
    )
    .unwrap();

    let settings = GuiSettings::load_from(&path).unwrap();
    assert_eq!(settings.recording.default_effect, VocalEffectPreset::Clean);
}
