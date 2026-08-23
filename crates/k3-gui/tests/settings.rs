use std::path::PathBuf;

use k3_gui::settings::{GuiSettings, WindowSize};

#[test]
fn gui_settings_round_trip_without_reusing_tui_configuration() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("gui.json");
    let expected = GuiSettings {
        projects_root: Some(PathBuf::from("/music/k3-projects")),
        volume: 0.72,
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
