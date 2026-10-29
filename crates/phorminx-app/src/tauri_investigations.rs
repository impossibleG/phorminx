//! Tauri command boundary for Investigations. Commands validate intent and
//! delegate to `InvestigationService`; voice notes reuse the meeting
//! workspace's text-only note capture (the same recognizer path as
//! Record-for-action), and quick capture owns one dedicated global shortcut.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use phorminx_ui::workspace::{CaptureSource, CaptureState, WorkspaceEvent};
use phorminx_windows::{RegisteredHotkey, Shortcut};
use serde::{Deserialize, Serialize};
use tauri::{Manager, State, WebviewUrl, WebviewWindowBuilder};

use crate::investigation_assistant::{AssistantView, InvestigationAssistant};
use crate::investigations::{
    Attachment, ClipboardCapture, InvestigationDetailView, InvestigationService,
    InvestigationsView, OcrLineView,
};
use crate::settings::SettingsStore;
use crate::tauri_meetings::MeetingTauriState;

/// Quick capture's default; rarely bound by other applications.
pub const DEFAULT_CAPTURE_SHORTCUT: &str = "Ctrl+Alt+Shift+I";
const VOICE_MAX_DURATION: Duration = Duration::from_secs(30 * 60);

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VoiceView {
    /// `idle`, `starting`, `recording` or `finishing`.
    pub state: String,
    pub investigation_id: Option<i64>,
    pub text: String,
    pub notice: String,
    pub revision: u64,
}

struct VoiceSession {
    investigation_id: i64,
    attachment: Attachment,
    stop_requested: bool,
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureShortcutView {
    pub shortcut: String,
    pub enabled: bool,
    pub registered: bool,
    pub notice: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OverviewView {
    #[serde(flatten)]
    overview: InvestigationsView,
    voice: VoiceView,
    capture: CaptureShortcutView,
    assistant: AssistantView,
}

pub struct InvestigationTauriState {
    service: Option<Arc<InvestigationService>>,
    assistant: Option<InvestigationAssistant>,
    store: SettingsStore,
    voice: Arc<Mutex<(Option<VoiceSession>, VoiceView)>>,
    hotkey: Mutex<Option<RegisteredHotkey>>,
    capture: Mutex<CaptureShortcutView>,
    toasts: AtomicU64,
}

impl InvestigationTauriState {
    /// Opens investigation storage and its workers. A failure leaves the tab
    /// visible with an explanation instead of stopping the whole shell.
    pub fn start(store: SettingsStore) -> Self {
        let directory = store.path().parent().map(PathBuf::from);
        let service = directory.as_ref().and_then(|directory| {
            let service = InvestigationService::open(directory).ok()?;
            service.remove_orphaned_folders();
            let _ = service.start_workers(directory);
            Some(Arc::new(service))
        });
        let assistant = service.clone().zip(directory).map(|(service, directory)| {
            InvestigationAssistant::new(service, directory.join("assistant.json"))
        });
        Self {
            service,
            assistant,
            store,
            voice: Arc::new(Mutex::new((
                None,
                VoiceView {
                    state: "idle".into(),
                    ..Default::default()
                },
            ))),
            hotkey: Mutex::new(None),
            capture: Mutex::new(CaptureShortcutView::default()),
            toasts: AtomicU64::new(0),
        }
    }

    pub(crate) fn service(&self) -> Result<&Arc<InvestigationService>, String> {
        self.service.as_ref().ok_or_else(|| {
            "Investigations are unavailable because local storage could not open.".to_owned()
        })
    }

    pub(crate) fn assistant(&self) -> Result<&InvestigationAssistant, String> {
        self.assistant
            .as_ref()
            .ok_or_else(|| "The investigation assistant is unavailable.".to_owned())
    }

    fn voice_view(&self) -> VoiceView {
        self.voice
            .lock()
            .map(|voice| voice.1.clone())
            .unwrap_or_default()
    }
}

/// Pauses screenshot OCR while dictation is listening or transcribing.
pub fn set_runtime_busy(app: &tauri::AppHandle, busy: bool) {
    if let Some(state) = app.try_state::<InvestigationTauriState>()
        && let Some(service) = state.service.as_ref()
    {
        service.set_capture_busy(busy);
    }
}

/// Registers the saved (or default) quick-capture shortcut. Called once the
/// Tauri app exists, because a press needs the app handle to show a toast.
pub fn install_quick_capture(app: &tauri::AppHandle) {
    let state = app.state::<InvestigationTauriState>();
    let saved = state.service.as_ref().and_then(|service| {
        service
            .db()
            .ok()?
            .investigations()
            .capture_shortcut()
            .ok()
            .flatten()
    });
    let text = saved.unwrap_or_else(|| DEFAULT_CAPTURE_SHORTCUT.to_owned());
    let view = apply_capture_shortcut(app, &state, &text);
    if let Ok(mut capture) = state.capture.lock() {
        *capture = view;
    }
}

fn apply_capture_shortcut(
    app: &tauri::AppHandle,
    state: &InvestigationTauriState,
    text: &str,
) -> CaptureShortcutView {
    let Ok(mut slot) = state.hotkey.lock() else {
        return CaptureShortcutView {
            notice: "The quick-capture shortcut is unavailable.".into(),
            ..Default::default()
        };
    };
    slot.take();
    if text.trim().is_empty() {
        return CaptureShortcutView {
            notice: "Quick capture shortcut is off.".into(),
            ..Default::default()
        };
    }
    let shortcut = match Shortcut::parse(text) {
        Ok(shortcut) => shortcut,
        Err(error) => {
            return CaptureShortcutView {
                shortcut: text.into(),
                enabled: true,
                notice: error.to_string(),
                ..Default::default()
            };
        }
    };
    let handle = app.clone();
    match RegisteredHotkey::register(shortcut, move || {
        let handle = handle.clone();
        let _ = thread::Builder::new()
            .name("phorminx-quick-capture".into())
            .spawn(move || quick_capture(&handle));
    }) {
        Ok(hotkey) => {
            *slot = Some(hotkey);
            CaptureShortcutView {
                shortcut: shortcut.to_string(),
                enabled: true,
                registered: true,
                notice: format!(
                    "Press {shortcut} anywhere to send the clipboard to the active investigation."
                ),
            }
        }
        Err(error) => CaptureShortcutView {
            shortcut: shortcut.to_string(),
            enabled: true,
            registered: false,
            notice: error.to_string(),
        },
    }
}

fn read_clipboard() -> Result<ClipboardCapture, String> {
    let mut clipboard =
        arboard::Clipboard::new().map_err(|_| "The clipboard is busy. Try again.".to_owned())?;
    // Text first: rich text copied from documents often carries a picture of
    // the same text that would otherwise win.
    if let Ok(text) = clipboard.get_text()
        && !text.trim().is_empty()
    {
        return Ok(ClipboardCapture::Text(text));
    }
    if let Ok(image) = clipboard.get_image() {
        return Ok(ClipboardCapture::Image {
            width: image.width as u32,
            height: image.height as u32,
            rgba: image.bytes.into_owned(),
        });
    }
    Err("The clipboard has no text, link or image to capture.".into())
}

fn quick_capture(app: &tauri::AppHandle) {
    let state = app.state::<InvestigationTauriState>();
    let outcome = state
        .service()
        .and_then(|service| read_clipboard().and_then(|capture| service.quick_capture(capture)));
    match outcome {
        Ok(outcome) => {
            let what = match outcome.kind {
                "image" => "Screenshot added",
                "link" => "Link added",
                _ => "Text added",
            };
            show_toast(app, &state, what, &outcome.investigation_title, false);
        }
        Err(error) => show_toast(app, &state, "Nothing captured", &error, true),
    }
}

/// A small, unfocused, always-on-top confirmation in the work-area corner.
pub(crate) fn show_toast(
    app: &tauri::AppHandle,
    state: &InvestigationTauriState,
    title: &str,
    detail: &str,
    error: bool,
) {
    show_toast_for(
        app,
        state,
        title,
        detail,
        error,
        Duration::from_millis(2800),
    );
}

pub(crate) fn show_toast_for(
    app: &tauri::AppHandle,
    state: &InvestigationTauriState,
    title: &str,
    detail: &str,
    error: bool,
    visible: Duration,
) {
    let sequence = state.toasts.fetch_add(1, Ordering::AcqRel) + 1;
    for (label, window) in app.webview_windows() {
        if label.starts_with("capture-toast-") {
            let _ = window.destroy();
        }
    }
    let payload = serde_json::json!({ "title": title, "detail": detail, "error": error });
    let (width, height) = (360.0, 84.0);
    let mut builder = WebviewWindowBuilder::new(
        app,
        format!("capture-toast-{sequence}"),
        WebviewUrl::App("tauri-preview/toast.html".into()),
    )
    .title("Phorminx capture")
    .inner_size(width, height)
    .resizable(false)
    .decorations(false)
    .always_on_top(true)
    .skip_taskbar(true)
    .focused(false)
    .initialization_script(format!("window.__PHORMINX_TOAST__ = {payload};"));
    if let Ok(Some(monitor)) = app.primary_monitor() {
        let scale = monitor.scale_factor();
        let area = monitor.work_area();
        let x = area.position.x as f64 / scale + area.size.width as f64 / scale - width - 18.0;
        let y = area.position.y as f64 / scale + area.size.height as f64 / scale - height - 18.0;
        builder = builder.position(x, y);
    }
    if builder.build().is_ok() {
        let handle = app.clone();
        let _ = thread::Builder::new()
            .name("phorminx-toast".into())
            .spawn(move || {
                thread::sleep(visible);
                if let Some(window) =
                    handle.get_webview_window(&format!("capture-toast-{sequence}"))
                {
                    let _ = window.destroy();
                }
            });
    }
}

fn attachment(parent_node: Option<i64>) -> Attachment {
    Attachment { parent_node }
}

#[tauri::command]
pub(crate) fn investigation_overview(
    state: State<'_, InvestigationTauriState>,
) -> Result<OverviewView, String> {
    let overview = state.service()?.overview()?;
    Ok(OverviewView {
        overview,
        voice: state.voice_view(),
        capture: state
            .capture
            .lock()
            .map(|view| view.clone())
            .unwrap_or_default(),
        assistant: state
            .assistant
            .as_ref()
            .map(InvestigationAssistant::view)
            .unwrap_or_default(),
    })
}

#[tauri::command]
pub(crate) fn investigation_create(
    title: String,
    state: State<'_, InvestigationTauriState>,
) -> Result<i64, String> {
    if title.chars().count() > 200 {
        return Err("Titles are limited to 200 characters.".into());
    }
    state.service()?.create(&title)
}

#[tauri::command]
pub(crate) fn investigation_rename(
    id: i64,
    title: String,
    state: State<'_, InvestigationTauriState>,
) -> Result<(), String> {
    if title.chars().count() > 200 {
        return Err("Titles are limited to 200 characters.".into());
    }
    state.service()?.rename(id, &title)
}

#[tauri::command]
pub(crate) fn investigation_delete(
    id: i64,
    state: State<'_, InvestigationTauriState>,
) -> Result<(), String> {
    if let Some(assistant) = state.assistant.as_ref() {
        assistant.cancel_for(id);
    }
    if state.voice.lock().is_ok_and(|voice| {
        voice
            .0
            .as_ref()
            .is_some_and(|session| session.investigation_id == id)
    }) {
        return Err("Finish the voice note for this investigation first.".into());
    }
    state.service()?.delete(id)
}

#[tauri::command]
pub(crate) fn investigation_set_active(
    id: Option<i64>,
    state: State<'_, InvestigationTauriState>,
) -> Result<(), String> {
    state.service()?.set_active(id)
}

#[tauri::command]
pub(crate) fn investigation_detail(
    id: i64,
    since: Option<u64>,
    state: State<'_, InvestigationTauriState>,
) -> Result<Option<InvestigationDetailView>, String> {
    state.service()?.detail(id, since)
}

#[tauri::command]
pub(crate) fn investigation_search(
    id: i64,
    query: String,
    kind: Option<String>,
    state: State<'_, InvestigationTauriState>,
) -> Result<Vec<i64>, String> {
    state.service()?.search(id, &query, kind.as_deref())
}

#[tauri::command]
pub(crate) fn investigation_add_text(
    id: i64,
    text: String,
    note: bool,
    parent_node: Option<i64>,
    state: State<'_, InvestigationTauriState>,
) -> Result<i64, String> {
    state.service()?.add_text(
        id,
        &text,
        note,
        if note { "typed" } else { "paste" },
        attachment(parent_node),
    )
}

#[tauri::command]
pub(crate) async fn investigation_add_image(
    id: i64,
    data: String,
    source: String,
    parent_node: Option<i64>,
    state: State<'_, InvestigationTauriState>,
) -> Result<i64, String> {
    let source = match source.as_str() {
        "paste" | "drop" | "file" => source,
        _ => return Err("Unknown image source.".into()),
    };
    let service = state.service()?.clone();
    tauri::async_runtime::spawn_blocking(move || {
        service.add_image_base64(id, &data, &source, attachment(parent_node))
    })
    .await
    .map_err(|_| "The image could not be added.".to_owned())?
}

#[tauri::command]
pub(crate) fn investigation_update_item(
    item_id: i64,
    title: String,
    body: String,
    state: State<'_, InvestigationTauriState>,
) -> Result<(), String> {
    state.service()?.update_item(item_id, &title, &body)
}

#[tauri::command]
pub(crate) fn investigation_delete_item(
    item_id: i64,
    state: State<'_, InvestigationTauriState>,
) -> Result<(), String> {
    state.service()?.delete_item(item_id)
}

#[tauri::command]
pub(crate) fn investigation_retry(
    item_id: i64,
    state: State<'_, InvestigationTauriState>,
) -> Result<(), String> {
    state.service()?.retry(item_id)
}

#[tauri::command]
pub(crate) async fn investigation_image(
    id: i64,
    sha: String,
    state: State<'_, InvestigationTauriState>,
) -> Result<String, String> {
    let service = state.service()?.clone();
    tauri::async_runtime::spawn_blocking(move || service.image_data_url(id, &sha))
        .await
        .map_err(|_| "The image could not be read.".to_owned())?
}

#[tauri::command]
pub(crate) fn investigation_ocr_lines(
    item_id: i64,
    state: State<'_, InvestigationTauriState>,
) -> Result<Vec<OcrLineView>, String> {
    state.service()?.ocr_lines(item_id)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct DiagramRequest {
    kind: String,
    investigation_id: Option<i64>,
    item_id: Option<i64>,
    node_id: Option<i64>,
    other_node_id: Option<i64>,
    edge_id: Option<i64>,
    label: Option<String>,
    x: Option<f64>,
    y: Option<f64>,
}

/// One entry point for diagram edits keeps the command surface small.
#[tauri::command]
pub(crate) fn investigation_diagram(
    request: DiagramRequest,
    state: State<'_, InvestigationTauriState>,
) -> Result<Option<i64>, String> {
    let service = state.service()?;
    let required =
        |value: Option<i64>| value.ok_or_else(|| "The diagram request is incomplete.".to_owned());
    let point = || -> Result<(f64, f64), String> {
        match (request.x, request.y) {
            (Some(x), Some(y)) if x.is_finite() && y.is_finite() => Ok((x, y)),
            _ => Err("The diagram position is invalid.".into()),
        }
    };
    let label = request.label.as_deref().unwrap_or_default();
    if label.chars().count() > 300 {
        return Err("Labels are limited to 300 characters.".into());
    }
    match request.kind.as_str() {
        "place" => {
            let (x, y) = point()?;
            service
                .place_item(required(request.item_id)?, x, y)
                .map(Some)
        }
        "concept" => {
            let (x, y) = point()?;
            service
                .add_concept(
                    required(request.investigation_id)?,
                    label,
                    x,
                    y,
                    attachment(request.node_id),
                )
                .map(Some)
        }
        "move" => {
            let (x, y) = point()?;
            service
                .move_node(required(request.node_id)?, x, y)
                .map(|()| None)
        }
        "rename" => service
            .rename_node(required(request.node_id)?, label)
            .map(|()| None),
        "remove" => service
            .delete_node(required(request.node_id)?)
            .map(|()| None),
        "connect" => service
            .connect(required(request.node_id)?, required(request.other_node_id)?)
            .map(|()| None),
        "accept" => service
            .accept_edge(required(request.edge_id)?)
            .map(|()| None),
        "disconnect" => service
            .delete_edge(required(request.edge_id)?)
            .map(|()| None),
        _ => Err("Unknown diagram action.".into()),
    }
}

#[tauri::command]
pub(crate) fn investigation_save_layout(
    id: i64,
    layout: String,
    state: State<'_, InvestigationTauriState>,
) -> Result<(), String> {
    state.service()?.save_layout(id, &layout)
}

#[tauri::command]
pub(crate) fn investigation_export(
    id: i64,
    state: State<'_, InvestigationTauriState>,
) -> Result<String, String> {
    state.service()?.export_markdown(id)
}

#[tauri::command]
pub(crate) fn investigation_models(
    kind: String,
    state: State<'_, InvestigationTauriState>,
) -> Result<(), String> {
    let service = state.service()?;
    match kind.as_str() {
        "install" => service.install_ocr_models(),
        "remove" => service.remove_ocr_models(),
        _ => Err("Unknown model action.".into()),
    }
}

#[tauri::command]
pub(crate) fn investigation_capture_shortcut(
    shortcut: String,
    app: tauri::AppHandle,
    state: State<'_, InvestigationTauriState>,
) -> Result<CaptureShortcutView, String> {
    let shortcut = shortcut.trim();
    if !shortcut.is_empty() {
        let parsed = Shortcut::parse(shortcut).map_err(|error| error.to_string())?;
        let settings = state.store.load().map_err(|error| error.to_string())?;
        let taken = std::iter::once(settings.interaction.launcher_shortcut.as_str())
            .chain(settings.interaction.direct_dictation_shortcut.as_deref())
            .any(|existing| Shortcut::parse(existing).is_ok_and(|existing| existing == parsed));
        if taken {
            return Err(
                "That shortcut already opens Phorminx or starts dictation. Choose another.".into(),
            );
        }
    }
    let previous = state
        .capture
        .lock()
        .map(|view| view.shortcut.clone())
        .unwrap_or_default();
    let view = apply_capture_shortcut(&app, &state, shortcut);
    if !shortcut.is_empty() && !view.registered {
        // Keep the working shortcut instead of leaving quick capture dead.
        let restored = apply_capture_shortcut(&app, &state, &previous);
        if let Ok(mut capture) = state.capture.lock() {
            *capture = restored;
        }
        return Err(view.notice);
    }
    state
        .service()?
        .db()?
        .investigations()
        .set_capture_shortcut(shortcut)
        .map_err(|_| "The shortcut could not be saved.")?;
    if let Ok(mut capture) = state.capture.lock() {
        *capture = view.clone();
    }
    Ok(view)
}

/// Captures the clipboard into the active investigation from the window.
#[tauri::command]
pub(crate) fn investigation_capture_clipboard(
    id: Option<i64>,
    state: State<'_, InvestigationTauriState>,
) -> Result<String, String> {
    let service = state.service()?;
    let capture = read_clipboard()?;
    let outcome = match id {
        Some(id) => service.capture_into(id, capture)?,
        None => service.quick_capture(capture)?,
    };
    Ok(outcome.investigation_title)
}

#[tauri::command]
pub(crate) fn investigation_voice(
    kind: String,
    id: Option<i64>,
    parent_node: Option<i64>,
    app: tauri::AppHandle,
    state: State<'_, InvestigationTauriState>,
) -> Result<VoiceView, String> {
    match kind.as_str() {
        "start" => start_voice(
            &app,
            &state,
            id.ok_or("Choose an investigation first.")?,
            attachment(parent_node),
        )?,
        "stop" => stop_voice(&app, &state)?,
        "status" => {}
        _ => return Err("Unknown voice action.".into()),
    }
    Ok(state.voice_view())
}

fn start_voice(
    app: &tauri::AppHandle,
    state: &InvestigationTauriState,
    investigation_id: i64,
    attachment: Attachment,
) -> Result<(), String> {
    state.service()?.title(investigation_id)?;
    let meeting = app.state::<MeetingTauriState>();
    let (capture, generation, _, _) = meeting
        .note_state()?
        .ok_or("Recording is busy. Try again in a moment.")?;
    if capture != CaptureState::Idle {
        return Err("Finish the current meeting or recording before adding a voice note.".into());
    }
    {
        let mut voice = state
            .voice
            .lock()
            .map_err(|_| "Voice notes are unavailable.")?;
        if voice.0.is_some() {
            return Err("A voice note is already recording.".into());
        }
        voice.0 = Some(VoiceSession {
            investigation_id,
            attachment,
            stop_requested: false,
        });
        voice.1 = VoiceView {
            state: "starting".into(),
            investigation_id: Some(investigation_id),
            text: String::new(),
            notice: "Starting the microphone…".into(),
            revision: voice.1.revision + 1,
        };
    }
    if let Err(error) = meeting.send(WorkspaceEvent::Start {
        source: CaptureSource::Microphone,
        device: None,
        title: "Investigation voice note".into(),
        note: true,
    }) {
        if let Ok(mut voice) = state.voice.lock() {
            voice.0 = None;
            voice.1.state = "idle".into();
            voice.1.notice = error.clone();
        }
        return Err(error);
    }
    let handle = app.clone();
    let voice = state.voice.clone();
    let service = state.service()?.clone();
    service.set_voice_busy(true);
    let resume = service.clone();
    if thread::Builder::new()
        .name("phorminx-voice-note".into())
        .spawn(move || watch_voice(handle, voice, service, generation))
        .is_err()
    {
        resume.set_voice_busy(false);
        // The capture already started; stop it rather than record unobserved.
        let _ = app.state::<MeetingTauriState>().send(WorkspaceEvent::Stop);
        if let Ok(mut voice) = state.voice.lock() {
            voice.0 = None;
            voice.1.state = "idle".into();
            voice.1.notice = "The voice note could not be followed and was stopped.".into();
            voice.1.revision += 1;
        }
        return Err("The voice note could not be followed and was stopped.".into());
    }
    Ok(())
}

fn stop_voice(app: &tauri::AppHandle, state: &InvestigationTauriState) -> Result<(), String> {
    {
        let mut voice = state
            .voice
            .lock()
            .map_err(|_| "Voice notes are unavailable.")?;
        let Some(session) = voice.0.as_mut() else {
            return Ok(());
        };
        session.stop_requested = true;
        voice.1.state = "finishing".into();
        voice.1.notice = "Finishing the transcription…".into();
        voice.1.revision += 1;
    }
    app.state::<MeetingTauriState>().send(WorkspaceEvent::Stop)
}

/// Follows one note capture until its text is final, then files it. Runs
/// even if the window is closed, so a started note is never lost silently.
fn watch_voice(
    app: tauri::AppHandle,
    voice: Arc<Mutex<(Option<VoiceSession>, VoiceView)>>,
    service: Arc<InvestigationService>,
    start_generation: u64,
) {
    let started = Instant::now();
    let meeting = app.state::<MeetingTauriState>();
    let mut seen_capture = false;
    loop {
        thread::sleep(Duration::from_millis(250));
        let Ok(Some((capture, generation, text, notice))) = meeting.note_state() else {
            continue;
        };
        let ours = generation == start_generation.wrapping_add(1);
        if ours && capture != CaptureState::Idle {
            seen_capture = true;
            if let Ok(mut guard) = voice.lock() {
                let stopping = guard
                    .0
                    .as_ref()
                    .is_some_and(|session| session.stop_requested);
                let view = &mut guard.1;
                let state = if stopping || capture == CaptureState::Stopping {
                    "finishing"
                } else if capture == CaptureState::Starting {
                    "starting"
                } else {
                    "recording"
                };
                if view.state != state || view.text != text {
                    view.state = state.into();
                    view.text = text.clone();
                    view.notice = if state == "recording" {
                        "Listening. Stop when you are done; only the text is kept.".into()
                    } else {
                        view.notice.clone()
                    };
                    view.revision += 1;
                }
            }
            if started.elapsed() > VOICE_MAX_DURATION {
                let _ = meeting.send(WorkspaceEvent::Stop);
            }
            continue;
        }
        let finished = ours && capture == CaptureState::Idle;
        let never_started = !seen_capture && !ours && started.elapsed() > Duration::from_secs(5);
        if !finished && !never_started && generation <= start_generation.wrapping_add(1) {
            continue;
        }
        service.set_voice_busy(false);
        let Ok(mut guard) = voice.lock() else {
            return;
        };
        let session = guard.0.take();
        let outcome = match (finished, session) {
            (true, Some(session)) => service
                .add_voice_note(session.investigation_id, &text, session.attachment)
                .map(|_| "Voice note added.".to_owned()),
            (_, _) if !notice.is_empty() => Err(notice),
            _ => Err("The microphone did not start.".into()),
        };
        let view = &mut guard.1;
        view.state = "idle".into();
        view.text.clear();
        view.notice = outcome.unwrap_or_else(|error| error);
        view.revision += 1;
        return;
    }
}

#[tauri::command]
pub(crate) fn investigation_assistant(
    kind: String,
    id: Option<i64>,
    text: Option<String>,
    message_id: Option<i64>,
    state: State<'_, InvestigationTauriState>,
) -> Result<AssistantView, String> {
    let assistant = state.assistant()?;
    let id = || id.ok_or_else(|| "Choose an investigation first.".to_owned());
    match kind.as_str() {
        "ask" => assistant.ask(id()?, text.as_deref().unwrap_or_default())?,
        "cancel" => assistant.cancel(),
        "suggest" => assistant.suggest(id()?)?,
        "dismiss-suggestions" => assistant.dismiss_suggestions(id()?)?,
        "clear" => {
            if assistant.busy_kind().is_some() {
                return Err("Wait for the current answer to finish.".into());
            }
            state.service()?.clear_messages(id()?)?
        }
        "pin" => {
            state
                .service()?
                .pin_answer(message_id.ok_or("Choose an answer to pin.")?)?;
        }
        _ => return Err("Unknown assistant action.".into()),
    }
    Ok(assistant.view())
}
