//! Peer-to-peer messaging in the Tauri shell: owns the messaging engine,
//! serves the `host_messaging_*` commands, raises a tray notification for
//! incoming messages and runs launcher dictation: record, review, send.
//!
//! Nothing here starts Tor. The engine only goes online after the user turns
//! messaging on (or had it on when Phorminx last ran).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;

use phorminx_assistant::ProtectedBytes;
use phorminx_messaging::{
    Config, ContactView, Event, KeyVault, MessageView, MessagingError, Messenger, SecretBytes,
    StatusView, TorTransport,
};
use phorminx_persistence::Persistence;
use phorminx_ui::workspace::WorkspaceEvent;
use serde::Serialize;
use tauri::{Manager, State, WebviewUrl, WebviewWindowBuilder};

use crate::meeting_workspace::{MessageCapture, MessageDraft};
use crate::settings::SettingsStore;
use crate::tauri_meetings::MeetingTauriState;

const REVIEW_WINDOW: &str = "message-review";
const CONVERSATION_LIMIT: usize = 500;

/// DPAPI protection for the identity key, bound to this Windows account.
struct DpapiVault;

impl KeyVault for DpapiVault {
    fn protect(&self, secret: &[u8]) -> Result<Vec<u8>, MessagingError> {
        ProtectedBytes::protect_identity_key(secret)
            .map(|p| p.ciphertext().to_vec())
            .map_err(|_| MessagingError::Identity)
    }

    fn expose(&self, protected: &[u8]) -> Result<SecretBytes, MessagingError> {
        let exposed = ProtectedBytes::from_ciphertext(protected.to_vec())
            .expose_identity_key()
            .map_err(|_| MessagingError::Identity)?;
        Ok(SecretBytes::new(exposed.as_bytes().to_vec()))
    }
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DraftView {
    /// idle, recording, review, empty or failed.
    pub phase: String,
    pub contact_id: Option<i64>,
    pub contact_name: String,
    pub text: String,
    pub message: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MessagingSnapshot {
    pub revision: u64,
    pub status: StatusView,
    pub contacts: Vec<ContactView>,
    pub launcher_slot: Option<u8>,
    /// Main-launcher keys 3-9 not used by a saved HTTP action.
    pub free_main_keys: Vec<u8>,
}

pub struct MessagingHost {
    messenger: Option<Messenger>,
    start_error: Option<String>,
    database: PathBuf,
    settings_dir: Option<PathBuf>,
    /// Cached messaging launcher key; written through on change.
    launcher_slot: Mutex<Option<u8>>,
    revision: Arc<AtomicU64>,
    /// The conversation open in the Messages page, or 0.
    focused: Arc<AtomicI64>,
    draft: Arc<Mutex<DraftView>>,
    drafts_tx: mpsc::Sender<MessageDraft>,
    drafts_rx: Mutex<Option<mpsc::Receiver<MessageDraft>>>,
}

impl MessagingHost {
    pub fn start(store: &SettingsStore, database: &Path) -> Self {
        let revision = Arc::new(AtomicU64::new(1));
        let focused = Arc::new(AtomicI64::new(0));
        let (drafts_tx, drafts_rx) = mpsc::channel();
        let events_revision = revision.clone();
        let events_focused = focused.clone();
        let result = Messenger::start(
            TorTransport::new(store.resolve_asset_path(Path::new("tor"))),
            database,
            Arc::new(DpapiVault),
            Config::default(),
            move |event| {
                events_revision.fetch_add(1, Ordering::AcqRel);
                if let Event::MessageReceived {
                    contact_id,
                    contact_name,
                    ..
                } = event
                    && events_focused.load(Ordering::Acquire) != contact_id
                {
                    // Names only: message text never appears in a notification.
                    let _ = phorminx_windows::show_tray_notification(
                        "Phorminx",
                        &format!("New message from {contact_name}"),
                    );
                }
            },
        );
        let (messenger, start_error) = match result {
            Ok(messenger) => (Some(messenger), None),
            Err(error) => (None, Some(error.to_string())),
        };
        Self {
            messenger,
            start_error,
            database: database.to_path_buf(),
            settings_dir: store.path().parent().map(Path::to_path_buf),
            launcher_slot: Mutex::new(
                Persistence::open(database)
                    .ok()
                    .and_then(|db| db.messaging().launcher_slot().ok().flatten()),
            ),
            revision,
            focused,
            draft: Arc::default(),
            drafts_tx,
            drafts_rx: Mutex::new(Some(drafts_rx)),
        }
    }

    fn messenger(&self) -> Result<&Messenger, String> {
        self.messenger.as_ref().ok_or_else(|| {
            self.start_error
                .clone()
                .unwrap_or_else(|| "Messaging is unavailable.".into())
        })
    }

    fn bump(&self) {
        self.revision.fetch_add(1, Ordering::AcqRel);
    }

    /// Launcher keys 3-9 used by saved HTTP actions.
    fn action_keys(&self) -> Vec<u8> {
        self.settings_dir
            .as_ref()
            .and_then(|dir| {
                phorminx_assistant::ConfigStore::new(dir.join("assistant.json"))
                    .load()
                    .ok()
            })
            .map(|config| {
                config
                    .actions
                    .iter()
                    .filter_map(|a| a.launcher_slot)
                    .collect()
            })
            .unwrap_or_default()
    }

    fn launcher_slot(&self) -> Option<u8> {
        self.launcher_slot.lock().ok().and_then(|slot| *slot)
    }

    pub fn snapshot(&self) -> Result<MessagingSnapshot, String> {
        let messenger = self.messenger()?;
        let used = self.action_keys();
        Ok(MessagingSnapshot {
            revision: self.revision.load(Ordering::Acquire),
            status: messenger.status().map_err(text)?,
            contacts: messenger.contacts().map_err(text)?,
            launcher_slot: self.launcher_slot(),
            free_main_keys: (3..=9).filter(|k| !used.contains(k)).collect(),
        })
    }

    /// Starts dictating to a contact (from the launcher or the app), or stops
    /// the recording if it is already for that contact.
    pub fn dictate(&self, app: &tauri::AppHandle, contact_id: i64) -> Result<(), String> {
        let contact = self
            .messenger()?
            .contacts()
            .map_err(text)?
            .into_iter()
            .find(|c| c.id == contact_id && !c.blocked)
            .ok_or("That contact is not available.")?;
        if let Ok(mut draft) = self.draft.lock() {
            *draft = DraftView {
                phase: "starting".into(),
                contact_id: Some(contact.id),
                contact_name: contact.name.clone(),
                ..DraftView::default()
            };
        }
        app.state::<MeetingTauriState>()
            .capture_message(MessageCapture {
                contact_id: contact.id,
                contact_name: contact.name,
                drafts: self.drafts_tx.clone(),
            })?;
        open_review(app)
    }

    /// Follows the dictation of a draft and opens the review window when the
    /// transcript is ready.
    pub fn spawn_draft_listener(&self, app: tauri::AppHandle) {
        let Some(drafts) = self.drafts_rx.lock().ok().and_then(|mut r| r.take()) else {
            return;
        };
        let draft = self.draft.clone();
        let _ = thread::Builder::new()
            .name("phorminx-message-drafts".into())
            .spawn(move || {
                while let Ok(event) = drafts.recv() {
                    if let Ok(mut view) = draft.lock() {
                        apply_draft(&mut view, event);
                    }
                    let _ = open_review(&app);
                }
            });
    }
}

fn apply_draft(view: &mut DraftView, event: MessageDraft) {
    match event {
        MessageDraft::Recording { contact_id } => {
            view.phase = "recording".into();
            view.contact_id = Some(contact_id);
            view.text.clear();
            view.message.clear();
        }
        MessageDraft::Ready { contact_id, text } => {
            view.phase = "review".into();
            view.contact_id = Some(contact_id);
            view.text = text;
        }
        MessageDraft::Empty { contact_id } => {
            view.phase = "empty".into();
            view.contact_id = Some(contact_id);
            view.message = "No speech was captured. Nothing was sent.".into();
        }
        MessageDraft::Failed {
            contact_id,
            message,
        } => {
            view.phase = "failed".into();
            view.contact_id = Some(contact_id);
            view.message = message;
        }
    }
}

fn open_review(app: &tauri::AppHandle) -> Result<(), String> {
    if let Some(window) = app.get_webview_window(REVIEW_WINDOW) {
        window
            .show()
            .map_err(|_| "The message window could not be shown.")?;
        let _ = window.set_focus();
        return Ok(());
    }
    WebviewWindowBuilder::new(
        app,
        REVIEW_WINDOW,
        WebviewUrl::App("tauri-preview/message.html".into()),
    )
    .title("Phorminx · Message")
    .inner_size(460.0, 300.0)
    .min_inner_size(360.0, 240.0)
    .always_on_top(true)
    .resizable(true)
    .build()
    .map(|_| ())
    .map_err(|_| "The message window could not be opened.".into())
}

fn close_review(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window(REVIEW_WINDOW) {
        let _ = window.close();
    }
}

fn text(error: MessagingError) -> String {
    error.to_string()
}

#[tauri::command]
pub(crate) fn host_messaging_snapshot(
    state: State<'_, MessagingHost>,
) -> Result<MessagingSnapshot, String> {
    state.snapshot()
}

#[tauri::command]
pub(crate) fn host_messaging_enable(
    name: Option<String>,
    state: State<'_, MessagingHost>,
) -> Result<(), String> {
    let result = state.messenger()?.enable(name.as_deref()).map_err(text);
    state.bump();
    result
}

#[tauri::command]
pub(crate) fn host_messaging_disable(state: State<'_, MessagingHost>) -> Result<(), String> {
    let result = state.messenger()?.disable().map_err(text);
    state.bump();
    result
}

#[tauri::command]
pub(crate) fn host_messaging_set_name(
    name: String,
    state: State<'_, MessagingHost>,
) -> Result<(), String> {
    let result = state.messenger()?.set_display_name(&name).map_err(text);
    state.bump();
    result
}

#[tauri::command]
pub(crate) fn host_messaging_invite(state: State<'_, MessagingHost>) -> Result<String, String> {
    state.messenger()?.create_invite().map_err(text)
}

#[tauri::command]
pub(crate) async fn host_messaging_redeem(
    invite: String,
    app: tauri::AppHandle,
) -> Result<ContactView, String> {
    // Redeeming takes seconds over Tor; keep it off the IPC thread.
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<MessagingHost>();
        let result = state.messenger()?.redeem_invite(&invite).map_err(text);
        state.bump();
        result
    })
    .await
    .map_err(|_| "Adding the contact was interrupted.".to_string())?
}

#[tauri::command]
pub(crate) fn host_messaging_conversation(
    contact_id: i64,
    state: State<'_, MessagingHost>,
) -> Result<Vec<MessageView>, String> {
    state
        .messenger()?
        .conversation(contact_id, CONVERSATION_LIMIT)
        .map_err(text)
}

#[tauri::command]
pub(crate) fn host_messaging_send(
    contact_id: i64,
    text_body: String,
    state: State<'_, MessagingHost>,
) -> Result<MessageView, String> {
    let result = state
        .messenger()?
        .send(contact_id, &text_body)
        .map_err(text);
    state.bump();
    result
}

/// Marks the open conversation as read and suppresses notifications for it
/// while it stays open. `None` when the page is closed or hidden.
#[tauri::command]
pub(crate) fn host_messaging_focus(
    contact_id: Option<i64>,
    state: State<'_, MessagingHost>,
) -> Result<(), String> {
    state
        .focused
        .store(contact_id.unwrap_or(0), Ordering::Release);
    if let Some(id) = contact_id {
        state.messenger()?.mark_read(id).map_err(text)?;
        state.bump();
    }
    Ok(())
}

#[tauri::command]
pub(crate) fn host_messaging_contact(
    contact_id: i64,
    kind: String,
    nickname: Option<String>,
    slot: Option<u8>,
    state: State<'_, MessagingHost>,
) -> Result<(), String> {
    let messenger = state.messenger()?;
    let result = match kind.as_str() {
        "rename" => messenger.rename(contact_id, nickname.as_deref()),
        "block" => messenger.set_blocked(contact_id, true),
        "unblock" => messenger.set_blocked(contact_id, false),
        "remove" => messenger.remove(contact_id),
        "delete-history" => messenger.delete_history(contact_id),
        "contacts-key" => messenger.set_contacts_slot(contact_id, slot),
        "promote" => {
            if let Some(key) = slot
                && (state.action_keys().contains(&key) || state.launcher_slot() == Some(key))
            {
                return Err(format!("Launcher key {key} is already used."));
            }
            messenger.set_main_slot(contact_id, slot)
        }
        _ => return Err("Unknown contact action.".into()),
    };
    state.bump();
    result.map_err(text)
}

/// Assigns the main-launcher key that opens the messaging contacts panel.
#[tauri::command]
pub(crate) fn host_messaging_launcher_key(
    slot: Option<u8>,
    state: State<'_, MessagingHost>,
) -> Result<(), String> {
    if let Some(key) = slot
        && state.action_keys().contains(&key)
    {
        return Err(format!("Launcher key {key} is used by a saved action."));
    }
    let db = Persistence::open(&state.database).map_err(|e| e.to_string())?;
    db.messaging()
        .set_launcher_slot(slot)
        .map_err(|e| e.to_string())?;
    if let Ok(mut cached) = state.launcher_slot.lock() {
        *cached = slot;
    }
    state.bump();
    Ok(())
}

#[tauri::command]
pub(crate) fn host_messaging_dictate(
    contact_id: i64,
    app: tauri::AppHandle,
    state: State<'_, MessagingHost>,
) -> Result<(), String> {
    state.dictate(&app, contact_id)
}

#[tauri::command]
pub(crate) fn host_messaging_draft(state: State<'_, MessagingHost>) -> Result<DraftView, String> {
    state
        .draft
        .lock()
        .map(|d| d.clone())
        .map_err(|_| "The draft is unavailable.".into())
}

/// Review window actions: `stop` ends the recording, `send` sends the edited
/// text, `discard` cancels the recording or drops the draft.
#[tauri::command]
pub(crate) fn host_messaging_draft_action(
    kind: String,
    text_body: Option<String>,
    app: tauri::AppHandle,
    state: State<'_, MessagingHost>,
) -> Result<(), String> {
    let current = state
        .draft
        .lock()
        .map(|d| d.clone())
        .map_err(|_| "The draft is unavailable.")?;
    match kind.as_str() {
        "stop" => {
            let contact_id = current.contact_id.ok_or("Nothing is recording.")?;
            app.state::<MeetingTauriState>()
                .capture_message(MessageCapture {
                    contact_id,
                    contact_name: current.contact_name,
                    drafts: state.drafts_tx.clone(),
                })
        }
        "send" => {
            let contact_id = current.contact_id.ok_or("There is no message to send.")?;
            let body = text_body.unwrap_or(current.text);
            state
                .messenger()?
                .send(contact_id, body.trim())
                .map_err(text)?;
            if let Ok(mut draft) = state.draft.lock() {
                *draft = DraftView::default();
            }
            state.bump();
            close_review(&app);
            Ok(())
        }
        "discard" => {
            if matches!(current.phase.as_str(), "starting" | "recording") {
                let _ = app
                    .state::<MeetingTauriState>()
                    .send(WorkspaceEvent::CancelActionCapture);
            }
            if let Ok(mut draft) = state.draft.lock() {
                *draft = DraftView::default();
            }
            close_review(&app);
            Ok(())
        }
        _ => Err("Unknown draft action.".into()),
    }
}
