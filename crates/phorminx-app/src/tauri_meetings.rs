//! Tauri's narrow meeting command boundary, shared by the isolated preview and
//! the eventual product shell. Commands receive intent; the meeting worker
//! owns capture, persistence, translation, and assistant state.

use std::sync::Mutex;

use phorminx_ui::workspace::{
    ActionDraft, ActionPayloadMode, CaptureSource, CaptureState, CompanionView, ProviderChoice,
    ProviderDraft, TranslationView, WorkspaceEvent, WorkspaceSnapshot,
};
use serde::Deserialize;
use tauri::{Manager, State, WebviewUrl, WebviewWindowBuilder};

use crate::meeting_workspace::MeetingWorkspace;
use crate::settings::{AppearancePreference, SettingsStore};
use crate::tauri_contract::{ActionItemView, MeetingView, ProviderView};

pub struct MeetingTauriState {
    meeting: Mutex<MeetingWorkspace>,
    store: SettingsStore,
}

impl MeetingTauriState {
    pub fn start(store: SettingsStore) -> Result<Self, String> {
        let meeting = MeetingWorkspace::start(store.clone())?;
        Ok(Self {
            meeting: Mutex::new(meeting),
            store,
        })
    }

    pub fn send(&self, event: WorkspaceEvent) -> Result<(), String> {
        self.meeting
            .lock()
            .map_err(|_| "Meeting state is unavailable.")?
            .send(event)
    }

    /// The text-only note capture used by Record-for-action and investigation
    /// voice notes: capture state, note generation, recognized text, notice.
    pub(crate) fn note_state(&self) -> Result<Option<(CaptureState, u64, String, String)>, String> {
        let meeting = self
            .meeting
            .lock()
            .map_err(|_| "Meeting state is unavailable.")?;
        Ok(meeting.snapshot_after(u64::MAX).map(|snapshot| {
            (
                snapshot.capture,
                snapshot.note_generation,
                snapshot.note_text.clone(),
                snapshot.notice.clone(),
            )
        }))
    }

    /// Starts (or stops) dictating a message for a messaging contact.
    pub fn capture_message(
        &self,
        capture: crate::meeting_workspace::MessageCapture,
    ) -> Result<(), String> {
        self.meeting
            .lock()
            .map_err(|_| "Meeting state is unavailable.")?
            .capture_message(capture)
    }

    pub fn snapshot(&self, since: u64) -> Result<Option<MeetingView>, String> {
        let meeting = self
            .meeting
            .lock()
            .map_err(|_| "Meeting state is unavailable.")?;
        Ok(meeting
            .snapshot_after(since)
            .as_ref()
            .map(MeetingView::from))
    }
}

pub fn install_meeting_commands(
    builder: tauri::Builder<tauri::Wry>,
    state: MeetingTauriState,
) -> tauri::Builder<tauri::Wry> {
    builder
        .manage(state)
        .invoke_handler(tauri::generate_handler![
            meeting_snapshot,
            meeting_actions,
            meeting_save_action,
            meeting_delete_action,
            meeting_provider,
            meeting_save_provider,
            meeting_action,
            meeting_layout,
            meeting_save_translation,
            meeting_save_devices,
            meeting_theme
        ])
}

#[tauri::command(async)]
pub(crate) fn meeting_actions(
    state: State<'_, MeetingTauriState>,
) -> Result<Vec<ActionItemView>, String> {
    let meeting = state
        .meeting
        .lock()
        .map_err(|_| "Actions are unavailable.")?;
    let snapshot = meeting
        .snapshot_after(u64::MAX)
        .ok_or("Actions are temporarily busy. Please retry.")?;
    Ok(snapshot.actions.iter().map(ActionItemView::from).collect())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ActionEditRequest {
    id: String,
    name: String,
    endpoint: String,
    method: String,
    payload_template: String,
    authorization: String,
    headers_json: String,
    launcher_slot: Option<u8>,
    payload_mode: String,
    payload_model: String,
    payload_prompt: String,
}

#[tauri::command(async)]
pub(crate) fn meeting_save_action(
    action: ActionEditRequest,
    state: State<'_, MeetingTauriState>,
) -> Result<(), String> {
    if action.id.len() > 128
        || action.name.len() > 200
        || action.endpoint.len() > 2048
        || action.payload_template.len() > 32768
        || action.authorization.len() > 8192
        || action.headers_json.len() > 32768
        || action.payload_prompt.len() > 8192
        || action.payload_model.len() > 200
    {
        return Err("Action settings are too long.".into());
    }
    let payload_mode = match action.payload_mode.as_str() {
        "template" => ActionPayloadMode::Template,
        "ai-json" => ActionPayloadMode::AiJson,
        _ => return Err("Choose a supported payload mode.".into()),
    };
    state.send(WorkspaceEvent::SaveAction(ActionDraft {
        id: action.id,
        name: action.name,
        endpoint: action.endpoint,
        method: action.method,
        payload_template: action.payload_template,
        authorization: action.authorization,
        headers_json: action.headers_json,
        launcher_slot: action.launcher_slot,
        payload_mode,
        payload_model: action.payload_model,
        payload_prompt: action.payload_prompt,
        ..Default::default()
    }))
}

#[tauri::command(async)]
pub(crate) fn meeting_delete_action(
    id: String,
    state: State<'_, MeetingTauriState>,
) -> Result<(), String> {
    let meeting = state
        .meeting
        .lock()
        .map_err(|_| "Actions are unavailable.")?;
    let snapshot = meeting
        .snapshot_after(u64::MAX)
        .ok_or("Actions are temporarily busy. Please retry.")?;
    if !snapshot.actions.iter().any(|action| action.id == id) {
        return Err("Choose a saved action.".into());
    }
    meeting.send(WorkspaceEvent::DeleteAction(id))
}

#[tauri::command(async)]
pub(crate) fn meeting_provider(
    state: State<'_, MeetingTauriState>,
) -> Result<ProviderView, String> {
    let meeting = state
        .meeting
        .lock()
        .map_err(|_| "Assistant settings are unavailable.")?;
    let snapshot = meeting
        .snapshot_after(u64::MAX)
        .ok_or("Assistant settings are temporarily busy. Please retry.")?;
    Ok(ProviderView::from_workspace(&snapshot))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ProviderEditRequest {
    kind: String,
    model: String,
    fallback_model: String,
    endpoint: String,
    api_key: String,
    clear_credential: bool,
    preset: String,
}

#[tauri::command(async)]
pub(crate) fn meeting_save_provider(
    provider: ProviderEditRequest,
    state: State<'_, MeetingTauriState>,
) -> Result<(), String> {
    if provider.model.len() > 200
        || provider.fallback_model.len() > 200
        || provider.endpoint.len() > 2048
        || provider.api_key.len() > 8192
        || provider.preset.len() > 8192
    {
        return Err("Assistant settings are too long.".into());
    }
    let kind = match provider.kind.as_str() {
        "ollama" => ProviderChoice::Ollama,
        "openai" => ProviderChoice::OpenAi,
        "anthropic" => ProviderChoice::Anthropic,
        _ => return Err("Choose a supported assistant provider.".into()),
    };
    state.send(WorkspaceEvent::SaveProvider(ProviderDraft {
        kind,
        model: provider.model,
        fallback_model: provider.fallback_model,
        endpoint: provider.endpoint,
        api_key: provider.api_key,
        clear_credential: provider.clear_credential,
        preset: provider.preset,
        ..Default::default()
    }))
}

#[tauri::command(async)]
pub(crate) fn meeting_theme(state: State<'_, MeetingTauriState>) -> Result<&'static str, String> {
    let preference = state
        .store
        .load()
        .map_err(|e| e.to_string())?
        .appearance
        .theme;
    let system = phorminx_windows::system_appearance();
    let dark = if system.high_contrast {
        system.contrast_theme_is_dark
    } else {
        match preference {
            AppearancePreference::System => phorminx_windows::system_apps_use_dark_theme(),
            AppearancePreference::Light => false,
            AppearancePreference::Dark => true,
        }
    };
    Ok(if dark { "dark" } else { "light" })
}

#[tauri::command(async)]
pub(crate) fn meeting_snapshot(
    since: Option<u64>,
    state: State<'_, MeetingTauriState>,
) -> Result<Option<MeetingView>, String> {
    state.snapshot(since.unwrap_or(u64::MAX))
}

pub fn open_companion(app: &tauri::AppHandle) -> Result<(), String> {
    if let Some(window) = app.get_webview_window("companion") {
        window.show().map_err(|_| "Companion could not be shown.")?;
        window
            .set_focus()
            .map_err(|_| "Companion could not be focused.")?;
    } else {
        WebviewWindowBuilder::new(
            app,
            "companion",
            WebviewUrl::App("tauri-preview/companion.html".into()),
        )
        .title("Phorminx · Live companion")
        .inner_size(980.0, 720.0)
        .min_inner_size(430.0, 500.0)
        .resizable(true)
        .build()
        .map_err(|_| "Companion could not be opened.")?;
    }
    Ok(())
}

#[tauri::command]
pub(crate) async fn meeting_action(
    kind: String,
    value: Option<String>,
    app: tauri::AppHandle,
    state: State<'_, MeetingTauriState>,
) -> Result<(), String> {
    if kind == "companion" {
        return open_companion(&app);
    }
    if kind == "hide-companion" {
        let window = app
            .get_webview_window("companion")
            .ok_or("Companion is not open.")?;
        window
            .hide()
            .map_err(|_| "Companion could not be hidden.")?;
        return Ok(());
    }
    let meeting = state
        .meeting
        .lock()
        .map_err(|_| "Meeting state is unavailable.")?;
    let snapshot = meeting
        .snapshot_after(u64::MAX)
        .ok_or("Meeting state is temporarily busy. Please retry.")?;
    let Some(event) = meeting_event(&kind, value.as_deref(), &snapshot)? else {
        return Ok(());
    };
    meeting.send(event)
}

fn meeting_event(
    kind: &str,
    value: Option<&str>,
    snapshot: &WorkspaceSnapshot,
) -> Result<Option<WorkspaceEvent>, String> {
    let event = match kind {
        "start" => WorkspaceEvent::StartMeeting,
        "refresh" => WorkspaceEvent::Refresh,
        "new" => WorkspaceEvent::New,
        "select" => {
            let id = value
                .unwrap_or_default()
                .parse::<i64>()
                .map_err(|_| "Choose a saved session from the list.")?;
            if id <= 0 {
                return Err("Choose a saved session from the list.".into());
            }
            WorkspaceEvent::Select(id)
        }
        "search" => {
            let query = value.unwrap_or_default().trim();
            if query.len() > 300 {
                return Err("Search query is too long.".into());
            }
            WorkspaceEvent::Search(query.to_owned())
        }
        "stop" if snapshot.capture == CaptureState::Idle => return Ok(None),
        "stop" => WorkspaceEvent::Stop,
        "commentary" => WorkspaceEvent::SetAutoCommentary(!snapshot.auto_commentary_enabled),
        "ask" => {
            let text = value.unwrap_or_default().trim();
            if text.is_empty() || text.len() > 12_000 {
                return Err("Enter a question of at most 12,000 characters.".into());
            }
            WorkspaceEvent::SendChat(text.to_owned())
        }
        "trigger-action" => {
            let id = value.unwrap_or_default();
            if !snapshot.actions.iter().any(|action| action.id == id) {
                return Err("Choose a saved action.".into());
            }
            WorkspaceEvent::TriggerAction(id.to_owned())
        }
        "mute-microphone" => WorkspaceEvent::SetMeetingSourceMuted {
            source: CaptureSource::Microphone,
            muted: !snapshot.microphone_muted,
        },
        "mute-computer" => WorkspaceEvent::SetMeetingSourceMuted {
            source: CaptureSource::SystemAudio,
            muted: !snapshot.computer_audio_muted,
        },
        _ => return Err("Unknown meeting action.".into()),
    };
    Ok(Some(event))
}

#[tauri::command(async)]
pub(crate) fn meeting_layout(
    view: String,
    language: String,
    split: u16,
    state: State<'_, MeetingTauriState>,
) -> Result<(), String> {
    let view = match view.as_str() {
        "transcript" => CompanionView::Transcript,
        "assistant" => CompanionView::Assistant,
        "together" => CompanionView::Split,
        _ => return Err("Unknown companion view.".into()),
    };
    let translation_view = match language.as_str() {
        "original" => TranslationView::Original,
        "translation" => TranslationView::Translation,
        "both" => TranslationView::Both,
        _ => return Err("Unknown translation view.".into()),
    };
    if !(280..=720).contains(&split) {
        return Err("Companion divider is outside its safe range.".into());
    }
    state.send(WorkspaceEvent::SaveCompanionLayout {
        view,
        translation_view,
        split_fraction: split,
    })
}

#[tauri::command(async)]
pub(crate) fn meeting_save_translation(
    language: String,
    model: String,
    state: State<'_, MeetingTauriState>,
) -> Result<(), String> {
    let language = language.trim();
    let model = model.trim();
    if language.len() > 80 || model.len() > 200 {
        return Err("Translation settings are too long.".into());
    }
    let meeting = state
        .meeting
        .lock()
        .map_err(|_| "Meeting state is unavailable.")?;
    if !language.is_empty() {
        let snapshot = meeting
            .snapshot_after(u64::MAX)
            .ok_or("Meeting state is temporarily busy. Please retry.")?;
        if !snapshot.available_models.iter().any(|name| name == model) {
            return Err("Choose an installed local model for translation.".into());
        }
    }
    meeting.send(WorkspaceEvent::SaveTranslation {
        language: language.to_owned(),
        model: model.to_owned(),
    })
}

#[tauri::command(async)]
pub(crate) fn meeting_save_devices(
    output_device: Option<String>,
    microphone_device: Option<String>,
    state: State<'_, MeetingTauriState>,
) -> Result<(), String> {
    let meeting = state
        .meeting
        .lock()
        .map_err(|_| "Meeting state is unavailable.")?;
    let snapshot = meeting
        .snapshot_after(u64::MAX)
        .ok_or("Meeting state is temporarily busy. Please retry.")?;
    if output_device
        .as_ref()
        .is_some_and(|selected| !snapshot.output_devices.contains(selected))
        || microphone_device
            .as_ref()
            .is_some_and(|selected| !snapshot.input_devices.contains(selected))
    {
        return Err("Choose an available audio device or Windows default.".into());
    }
    meeting.send(WorkspaceEvent::SaveMeetingDevices {
        output_device,
        microphone_device,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn companion_actions_map_to_existing_meeting_intents() {
        let mut snapshot = WorkspaceSnapshot {
            capture: CaptureState::Listening,
            ..Default::default()
        };
        assert_eq!(
            meeting_event("mute-microphone", None, &snapshot).unwrap(),
            Some(WorkspaceEvent::SetMeetingSourceMuted {
                source: CaptureSource::Microphone,
                muted: true
            })
        );
        snapshot.microphone_muted = true;
        assert_eq!(
            meeting_event("mute-microphone", None, &snapshot).unwrap(),
            Some(WorkspaceEvent::SetMeetingSourceMuted {
                source: CaptureSource::Microphone,
                muted: false
            })
        );
        assert_eq!(
            meeting_event("commentary", None, &snapshot).unwrap(),
            Some(WorkspaceEvent::SetAutoCommentary(true))
        );
        assert_eq!(
            meeting_event("ask", Some("  What changed?  "), &snapshot).unwrap(),
            Some(WorkspaceEvent::SendChat("What changed?".into()))
        );
        assert_eq!(
            meeting_event("stop", None, &snapshot).unwrap(),
            Some(WorkspaceEvent::Stop)
        );
        snapshot.capture = CaptureState::Idle;
        assert_eq!(meeting_event("stop", None, &snapshot).unwrap(), None);
        assert_eq!(
            meeting_event("select", Some("42"), &snapshot).unwrap(),
            Some(WorkspaceEvent::Select(42))
        );
    }

    #[test]
    fn companion_actions_reject_empty_oversize_and_unknown_requests() {
        let snapshot = WorkspaceSnapshot::default();
        assert!(meeting_event("ask", Some("   "), &snapshot).is_err());
        assert!(meeting_event("ask", Some(&"x".repeat(12_001)), &snapshot).is_err());
        assert!(meeting_event("delete-everything", None, &snapshot).is_err());
        assert!(meeting_event("select", Some("-2"), &snapshot).is_err());
        assert!(meeting_event("search", Some(&"x".repeat(301)), &snapshot).is_err());
    }
}
