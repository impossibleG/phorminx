//! Tauri commands for "Reinstall Phorminx" and the restore that follows it,
//! plus the minimal window behind the "Repair Phorminx" shortcut, which
//! works even when settings or the database are damaged.
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Serialize;
use tauri::{State, WebviewUrl, WebviewWindowBuilder};

use crate::reinstall::{self, BackupView, FlowView, ReinstallFlow};

pub struct ReinstallState {
    flow: ReinstallFlow,
    data_dir: PathBuf,
    /// The `--reinstall` window: no dictation, meetings or settings loaded.
    minimal: bool,
    restore: Arc<Mutex<RestoreView>>,
}

impl ReinstallState {
    pub fn new(data_dir: PathBuf, minimal: bool) -> Self {
        Self {
            flow: ReinstallFlow::default(),
            data_dir,
            minimal,
            restore: Arc::default(),
        }
    }
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RestoreView {
    /// `idle`, `restoring`, `models`, `restarting`, `failed`.
    pub state: String,
    pub message: String,
    pub model: String,
    pub percent: Option<u8>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReinstallView {
    flow: FlowView,
    minimal: bool,
    after_reinstall: bool,
    backups: Vec<BackupView>,
    restore: RestoreView,
    /// Why a reinstall cannot start right now, in plain words.
    blocked: String,
}

fn blocked_reason(app: &tauri::AppHandle, state: &ReinstallState) -> String {
    if state.minimal {
        return String::new();
    }
    if let Err(reason) = reinstall::installed_program_dir() {
        return reason;
    }
    crate::tauri_shell::reinstall_blocker(app).unwrap_or_default()
}

#[tauri::command(async)]
pub fn reinstall_snapshot(app: tauri::AppHandle, state: State<'_, ReinstallState>) -> Result<ReinstallView, String> {
    Ok(ReinstallView {
        flow: state.flow.snapshot(),
        minimal: state.minimal,
        after_reinstall: reinstall::after_reinstall(),
        backups: reinstall::list_backups(&state.data_dir),
        restore: state.restore.lock().map(|view| view.clone()).unwrap_or_default(),
        blocked: blocked_reason(&app, &state),
    })
}

/// Look up, download and verify the installer. Nothing changes yet.
#[tauri::command(async)]
pub fn reinstall_start(app: tauri::AppHandle, state: State<'_, ReinstallState>) -> Result<FlowView, String> {
    reinstall::installed_program_dir()?;
    let blocked = blocked_reason(&app, &state);
    if !blocked.is_empty() {
        return Err(blocked);
    }
    state.flow.start(&state.data_dir)?;
    Ok(state.flow.snapshot())
}

/// Back up, hand off to the helper and close Phorminx.
#[tauri::command(async)]
pub fn reinstall_finish(app: tauri::AppHandle, state: State<'_, ReinstallState>) -> Result<FlowView, String> {
    let program_dir = reinstall::installed_program_dir()?;
    let blocked = blocked_reason(&app, &state);
    if !blocked.is_empty() {
        return Err(blocked);
    }
    if !state.flow.ready() {
        return Err("The installer is not ready yet.".into());
    }
    state.flow.hand_off(&state.data_dir, &program_dir)?;
    if state.minimal {
        app.exit(0);
    } else if !crate::tauri_shell::request_quit(&app) {
        return Err("Phorminx could not close itself. Close it from the tray icon to finish reinstalling.".into());
    }
    Ok(state.flow.snapshot())
}

/// Close the Repair Phorminx window without changing anything.
#[tauri::command]
pub fn reinstall_close(app: tauri::AppHandle, state: State<'_, ReinstallState>) -> Result<(), String> {
    if !state.minimal {
        return Err("Close the dialog instead.".into());
    }
    if state.flow.snapshot().state == "handing-off" {
        return Ok(());
    }
    app.exit(0);
    Ok(())
}

/// True once, right after a reinstall: the page then starts setup.
#[tauri::command]
pub fn reinstall_take_auto_setup() -> bool {
    reinstall::take_auto_setup()
}

#[tauri::command(async)]
pub fn reinstall_dismiss(id: String, state: State<'_, ReinstallState>) -> Result<(), String> {
    reinstall::dismiss(&state.data_dir, &id)
}

#[tauri::command]
pub fn reinstall_restore_status(state: State<'_, ReinstallState>) -> RestoreView {
    state.restore.lock().map(|view| view.clone()).unwrap_or_default()
}

/// Copy the backed-up settings back, install the Ollama models they use,
/// then restart Phorminx the way a settings save does.
#[tauri::command(async)]
pub fn reinstall_restore(id: String, app: tauri::AppHandle, state: State<'_, ReinstallState>) -> Result<RestoreView, String> {
    if state.minimal {
        return Err("Open Phorminx normally to restore settings.".into());
    }
    if let Some(reason) = crate::tauri_shell::reinstall_blocker(&app) {
        return Err(reason);
    }
    {
        let mut view = state.restore.lock().map_err(|_| "Restore is unavailable.")?;
        if matches!(view.state.as_str(), "restoring" | "models" | "restarting") {
            return Err("Settings are already being restored.".into());
        }
        *view = RestoreView {
            state: "restoring".into(),
            message: "Restoring your previous settings…".into(),
            ..RestoreView::default()
        };
    }
    let outcome = match reinstall::restore(&state.data_dir, &id) {
        Ok(outcome) => outcome,
        Err(message) => {
            let mut view = state.restore.lock().map_err(|_| "Restore is unavailable.")?;
            view.state = "failed".into();
            view.message = message.clone();
            return Err(message);
        }
    };
    let view = state.restore.clone();
    let store = crate::tauri_shell::settings_store(&app).ok_or("Settings are unavailable.")?;
    std::thread::Builder::new()
        .name("phorminx-restore-models".into())
        .spawn(move || {
            let set = |update: &dyn Fn(&mut RestoreView)| {
                if let Ok(mut view) = view.lock() {
                    update(&mut view);
                }
            };
            let installed: Vec<String> = crate::model_registry::list(&store)
                .into_iter()
                .filter(|entry| entry.family == "ollama" && entry.installed)
                .map(|entry| entry.name)
                .collect();
            let mut failed = Vec::new();
            for model in outcome.models.iter().filter(|model| !installed.contains(model)) {
                set(&|view| {
                    view.state = "models".into();
                    view.model = model.clone();
                    view.percent = Some(0);
                    view.message = format!("Installing {model}, which your settings use…");
                });
                if crate::model_registry::start_pull(model, None).is_err() {
                    failed.push(model.clone());
                    continue;
                }
                loop {
                    std::thread::sleep(Duration::from_millis(500));
                    let pull = crate::model_registry::pull_status();
                    if pull.state != "pulling" {
                        if pull.state != "done" {
                            failed.push(model.clone());
                        }
                        break;
                    }
                    set(&|view| view.percent = pull.percent);
                }
            }
            let message = if failed.is_empty() {
                "Settings restored. Phorminx is restarting…".to_owned()
            } else {
                format!(
                    "Settings restored, but {} could not be installed. Install it later from Models. Phorminx is restarting…",
                    failed.join(", ")
                )
            };
            set(&|view| {
                view.state = "restarting".into();
                view.message = message.clone();
                view.percent = None;
            });
            if !failed.is_empty() {
                std::thread::sleep(Duration::from_secs(4));
            }
            crate::tauri_shell::request_settings_restart(&app);
        })
        .map_err(|_| "Could not finish restoring settings.")?;
    Ok(state.restore.lock().map(|view| view.clone()).unwrap_or_default())
}

/// The window opened by `phorminx-app.exe --reinstall`. It loads nothing but
/// the reinstall flow, so it works when settings or the database are broken.
pub fn run_reinstall_window(data_dir: PathBuf) -> Result<(), String> {
    let mut context = crate::tauri_shell::app_context();
    context.config_mut().app.windows.clear();
    tauri::Builder::default()
        .manage(ReinstallState::new(data_dir, true))
        .invoke_handler(tauri::generate_handler![
            reinstall_snapshot,
            reinstall_start,
            reinstall_finish,
            reinstall_close,
        ])
        .setup(|app| {
            WebviewWindowBuilder::new(app, "reinstall", WebviewUrl::App("tauri-preview/reinstall.html".into()))
                .title("Repair Phorminx")
                .inner_size(560.0, 600.0)
                .min_inner_size(420.0, 460.0)
                .resizable(true)
                .build()?;
            Ok(())
        })
        .build(context)
        .map(|app| {
            app.run_return(|_, _| {});
        })
        .map_err(|error| error.to_string())
}

