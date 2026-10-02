//! Centralized English copy emitted by the Rust GUI adapter.

use std::path::Path;

pub const TUI_FALLBACK: &str = "Run `k3 tui` to use the terminal interface instead.";

#[must_use]
pub fn unreadable_projects_folder(path: &Path) -> String {
    format!("Projects folder is not readable: {}", path.display())
}

#[must_use]
pub fn gui_start_failed(error: &dyn std::fmt::Display) -> String {
    format!("K3 GUI could not start: {error}")
}

#[must_use]
pub fn diagnostics(path: &Path) -> String {
    format!("Diagnostics: {}", path.display())
}

#[must_use]
pub fn settings_load_failed(error: &dyn std::fmt::Display) -> String {
    format!("K3 GUI settings could not be loaded and will be preserved: {error}")
}

#[must_use]
pub fn settings_save_failed(error: &dyn std::fmt::Display) -> String {
    format!("K3 GUI settings could not be saved: {error}")
}
