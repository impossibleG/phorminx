//! An isolated, opt-in Tauri integration proof. The installed Phorminx binary
//! remains unchanged until the replacement passes parity QA.

#![cfg_attr(windows, windows_subsystem = "windows")]

use std::path::PathBuf;

use phorminx_app::settings::SettingsStore;
use phorminx_app::tauri_meetings::{MeetingTauriState, install_meeting_commands};

fn settings_store() -> Result<SettingsStore, String> {
    let mut args = std::env::args_os().skip(1);
    match (args.next(), args.next(), args.next()) {
        (None, None, None) => {
            // A double-clicked preview must never modify the installed app's
            // settings or history. An explicit --config is required for that.
            let directory =
                std::env::temp_dir().join(format!("phorminx-tauri-preview-{}", std::process::id()));
            std::fs::create_dir_all(&directory)
                .map_err(|_| "An isolated preview profile could not be created.")?;
            SettingsStore::new(directory.join("settings.toml")).map_err(|e| e.to_string())
        }
        (Some(flag), Some(path), None) if flag == "--config" => {
            SettingsStore::new(PathBuf::from(path)).map_err(|e| e.to_string())
        }
        _ => Err("Usage: phorminx-tauri-preview [--config SETTINGS_PATH]".into()),
    }
}

fn main() {
    let result = (|| -> Result<(), String> {
        let store = settings_store()?;
        let state = MeetingTauriState::start(store)?;
        install_meeting_commands(tauri::Builder::default(), state)
            .run(tauri::generate_context!(
                "src/bin/tauri-preview-config/tauri.conf.json"
            ))
            .map_err(|e| e.to_string())
    })();
    if let Err(error) = result {
        eprintln!("Tauri preview failed: {error}");
        std::process::exit(1);
    }
}
