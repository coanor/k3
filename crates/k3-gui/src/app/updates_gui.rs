//! Nonblocking release checks for the About panel.

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use k3_app::updates::{ReleaseStatus, check_release, release_repository};
use slint::ComponentHandle;

use super::K3Window;

pub(super) fn install(ui: &K3Window) {
    ui.set_app_version(env!("CARGO_PKG_VERSION").into());
    let running = Arc::new(AtomicBool::new(false));
    let weak = ui.as_weak();
    ui.on_check_updates(move || start(weak.clone(), Arc::clone(&running)));
    ui.on_open_release({
        let weak = ui.as_weak();
        move || {
            let Some(ui) = weak.upgrade() else { return };
            let url = ui.get_release_url();
            if url.is_empty() {
                return;
            }
            #[cfg(target_os = "windows")]
            let program = "explorer.exe";
            #[cfg(target_os = "macos")]
            let program = "open";
            #[cfg(not(any(target_os = "windows", target_os = "macos")))]
            let program = "xdg-open";
            if let Err(error) = std::process::Command::new(program)
                .arg(url.as_str())
                .spawn()
            {
                ui.set_update_status(format!("Cannot open release page: {error}").into());
            }
        }
    });
    ui.invoke_check_updates();
}

fn start(weak: slint::Weak<K3Window>, running: Arc<AtomicBool>) {
    if running.swap(true, Ordering::AcqRel) {
        return;
    }
    if let Some(ui) = weak.upgrade() {
        ui.set_update_checking(true);
        ui.set_update_status("Checking for updates…".into());
    }
    std::thread::spawn(move || {
        let outcome =
            release_repository().and_then(|repo| check_release(&repo, env!("CARGO_PKG_VERSION")));
        running.store(false, Ordering::Release);
        let _ = weak.upgrade_in_event_loop(move |ui| {
            ui.set_update_checking(false);
            let (message, url) = match outcome {
                Ok(ReleaseStatus::Current) => ("K3 is up to date".into(), String::new()),
                Ok(ReleaseStatus::Available { version, url }) => {
                    (format!("K3 {version} is available"), url)
                }
                Err(error) => (format!("Update check failed: {error}"), String::new()),
            };
            ui.set_update_status(message.into());
            ui.set_release_url(url.into());
        });
    });
}
