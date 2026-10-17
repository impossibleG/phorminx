//! Explicit, review-before-send feedback. No automatic diagnostics or audio upload.
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

use eframe::egui;
use serde::{Deserialize, Serialize};

use crate::meeting_audio::{MeetingAudioEvent, MeetingAudioService, MeetingAudioSource};
use crate::settings::{Settings, SettingsStore};

// The apex host: www redirects there, and submissions deliberately refuse
// redirects, so a www endpoint would never reach the service.
const ENDPOINT: &str = "https://phorminx.net/api/v1/feedback";
const MAX_TEXT_BYTES: usize = 12_000;

#[derive(Clone, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FeedbackSubmission {
    schema_version: u8,
    submission_id: String,
    text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    diagnostics: Option<Diagnostics>,
}

#[derive(Clone, Serialize)]
struct Diagnostics {
    app_version: String,
    recognition_mode: &'static str,
}

#[derive(Deserialize)]
struct Receipt {
    ticket_id: String,
}

impl FeedbackSubmission {
    fn new(text: &str, settings: &Settings, diagnostics: bool) -> Result<Self, String> {
        let text = text.trim();
        if text.is_empty() || text.len() > MAX_TEXT_BYTES {
            return Err("Feedback must contain between 1 and 12,000 UTF-8 bytes.".into());
        }
        // Windows supplies cryptographically random UUIDs. Never use a device identity.
        let id = submission_id()?;
        Ok(Self {
            schema_version: 1,
            submission_id: id,
            text: text.into(),
            diagnostics: diagnostics.then(|| Diagnostics {
                app_version: crate::updates::current_version().into(),
                recognition_mode: match settings.recognition.mode {
                    crate::settings::RecognitionMode::Instant => "instant",
                    crate::settings::RecognitionMode::Accurate => "accurate",
                },
            }),
        })
    }
}

fn submission_id() -> Result<String, String> {
    #[cfg(windows)]
    {
        let id =
            windows::core::GUID::new().map_err(|_| "Could not create a feedback request ID.")?;
        Ok(format!("{id:?}").to_ascii_lowercase())
    }
    #[cfg(not(windows))]
    Err("Feedback is currently available on Windows.".into())
}

fn submit(request: &FeedbackSubmission) -> Result<String, String> {
    let body = serde_json::to_vec(request).map_err(|_| "Could not encode feedback.")?;
    if body.len() > 16 * 1024 {
        return Err("Feedback is too large to send. Please shorten it.".into());
    }
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(20)))
        .max_redirects(0)
        .build()
        .new_agent();
    let mut response = agent
        .post(ENDPOINT)
        .header("Content-Type", "application/json")
        .send(body.as_slice())
        .map_err(|_| "Feedback was not confirmed. Your draft is retained; retry when the service is available.")?;
    if !matches!(response.status().as_u16(), 200 | 201) {
        return Err("Feedback was not accepted. Your draft is retained.".into());
    }
    let body = response
        .body_mut()
        .with_config()
        .limit(4096)
        .read_to_string()
        .map_err(|_| "The feedback receipt could not be read. Retry with the same draft.")?;
    let receipt: Receipt = serde_json::from_str(&body)
        .map_err(|_| "The feedback receipt was invalid. Retry with the same draft.")?;
    if receipt.ticket_id != request.submission_id
        || receipt.ticket_id.len() > 80
        || !receipt
            .ticket_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-')
    {
        return Err("The feedback receipt was invalid. Retry with the same draft.".into());
    }
    Ok(receipt.ticket_id)
}

#[derive(Default)]
pub struct FeedbackPanel {
    pub open: bool,
    text: String,
    partial: String,
    notice: String,
    diagnostics: bool,
    capture: Option<MeetingAudioService>,
    capture_lease: Option<crate::performance_runtime::RuntimeActivityLease>,
    stopping: bool,
    pending: Option<FeedbackSubmission>,
    response: Option<Receiver<Result<String, String>>>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FeedbackView {
    pub text: String,
    pub provisional_text: String,
    pub notice: String,
    pub diagnostics: bool,
    pub recording: bool,
    pub stopping: bool,
    pub sending: bool,
}

impl FeedbackPanel {
    pub fn snapshot(&self) -> FeedbackView {
        FeedbackView {
            text: self.text.clone(),
            provisional_text: self.partial.clone(),
            notice: self.notice.clone(),
            diagnostics: self.diagnostics,
            recording: self.capture.is_some(),
            stopping: self.stopping,
            sending: self.response.is_some(),
        }
    }

    pub fn update_draft(&mut self, text: String, diagnostics: bool) -> Result<(), String> {
        if self.is_busy() {
            return Err("Finish recording or sending before editing the draft.".into());
        }
        if text.len() > MAX_TEXT_BYTES {
            return Err("Feedback must contain at most 12,000 UTF-8 bytes.".into());
        }
        if self.text != text || self.diagnostics != diagnostics {
            self.pending = None;
        }
        self.text = text;
        self.diagnostics = diagnostics;
        Ok(())
    }

    pub fn start_recording(&mut self, settings: &Settings, store: &SettingsStore) {
        if !self.is_busy() {
            self.record(settings, store);
        }
    }

    pub fn stop_recording(&mut self) {
        if let Some(capture) = &self.capture {
            let _ = capture.stop();
            self.stopping = true;
        }
    }

    pub fn close_window(&mut self) {
        self.stop_recording();
        self.open = false;
    }

    pub fn send_report(&mut self, settings: &Settings) -> Result<(), String> {
        if self.is_busy() {
            return Err("Finish recording or sending first.".into());
        }
        // Reuse the request ID on uncertain retries; an edited draft is a new report.
        let unchanged = self.pending.as_ref().is_some_and(|request| {
            request.text == self.text.trim() && request.diagnostics.is_some() == self.diagnostics
        });
        if !unchanged {
            self.pending = Some(FeedbackSubmission::new(
                &self.text,
                settings,
                self.diagnostics,
            )?);
        }
        if let Some(request) = self.pending.clone() {
            let (tx, rx) = mpsc::sync_channel(1);
            std::thread::Builder::new()
                .name("phorminx-feedback".into())
                .spawn(move || {
                    let _ = tx.send(submit(&request));
                })
                .map_err(|_| "Could not start sending. Your draft is retained.".to_owned())?;
            self.response = Some(rx);
            self.notice = "Sending report…".into();
        }
        Ok(())
    }
    pub fn is_busy(&self) -> bool {
        self.capture.is_some() || self.response.is_some()
    }
    pub fn open(&mut self, settings: &Settings, store: &SettingsStore, can_record: bool) {
        self.open = true;
        if self.text.is_empty() && self.capture.is_none() && self.response.is_none() && can_record {
            self.record(settings, store);
        }
    }

    fn record(&mut self, settings: &Settings, store: &SettingsStore) {
        let lease = match crate::performance_runtime::production_workload_coordinator()
            .try_begin(crate::performance_runtime::RuntimeActivityKind::Dictation)
        {
            Ok(lease) => lease,
            Err(_) => {
                self.notice =
                    "Another capture is active. Finish it first, or type your feedback.".into();
                return;
            }
        };
        let mut recognition = settings.recognition.clone();
        recognition.model_path = store.resolve_asset_path(&recognition.model_path);
        recognition.instant_model_path = store.resolve_asset_path(&recognition.instant_model_path);
        recognition.instant_runtime_path =
            store.resolve_asset_path(&recognition.instant_runtime_path);
        let device = recognition.microphone.clone();
        match MeetingAudioService::start(
            recognition,
            MeetingAudioSource::Microphone,
            device,
            store.resolve_asset_path(std::path::Path::new("feedback-scratch")),
        ) {
            Ok(capture) => {
                self.capture = Some(capture);
                self.capture_lease = Some(lease);
                self.stopping = false;
                self.notice =
                    "Starting microphone. Nothing is sent until you review and press Send.".into();
            }
            Err(error) => {
                self.notice = format!("Recording unavailable: {error} You can type instead.")
            }
        }
    }

    pub fn poll(&mut self) {
        let mut ended = false;
        if let Some(capture) = &self.capture {
            for _ in 0..64 {
                let Some(event) = capture.try_recv() else {
                    break;
                };
                match event {
                    MeetingAudioEvent::Ready => {
                        self.notice = "Listening. Stop when your report is complete.".into()
                    }
                    MeetingAudioEvent::Partial { text, .. } => self.partial = text,
                    MeetingAudioEvent::Segment { text, .. } => {
                        if !text.trim().is_empty() {
                            if !self.text.is_empty() {
                                self.text.push('\n');
                            }
                            self.text.push_str(text.trim());
                        }
                        self.partial.clear();
                    }
                    MeetingAudioEvent::Stopped { .. } => {
                        ended = true;
                        self.notice = "Review your report before sending.".into();
                    }
                    MeetingAudioEvent::Error { message } => {
                        ended = true;
                        self.notice =
                            format!("{message} Your draft is retained; you can type instead.");
                    }
                    _ => {}
                }
            }
        }
        if ended {
            self.capture = None;
            self.capture_lease = None;
            self.stopping = false;
            self.partial.clear();
        }
        if let Some(response) = &self.response {
            match response.try_recv() {
                Ok(Ok(ticket)) => {
                    self.notice = format!("Report received. Ticket {ticket}");
                    self.text.clear();
                    self.pending = None;
                    self.response = None;
                }
                Ok(Err(error)) => {
                    self.notice = error;
                    self.response = None;
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.notice = "Feedback worker exited. Your draft is retained; retry to confirm delivery.".into();
                    self.response = None;
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
    }

    pub fn show(
        &mut self,
        ctx: &egui::Context,
        settings: &Settings,
        store: &SettingsStore,
        can_record: bool,
    ) {
        if !self.open {
            return;
        }
        let mut open = true;
        egui::Window::new("Feedback · Phorminx").open(&mut open).default_width(500.0).resizable(true).show(ctx, |ui| {
            ui.heading("Tell us what happened");
            ui.label("Only this report is sent to phorminx.net. No audio, meeting text, or history is attached.");
            ui.add_space(12.0);
            let editing = self.capture.is_none() && self.response.is_none();
            ui.add_enabled(editing, egui::TextEdit::multiline(&mut self.text).desired_rows(9).desired_width(f32::INFINITY));
            if !self.partial.is_empty() { ui.weak(&self.partial); }
            ui.label(format!("{} / {MAX_TEXT_BYTES} bytes", self.text.len()));
            ui.add_enabled(editing, egui::Checkbox::new(&mut self.diagnostics, "Include app version and recognition mode only"));
            ui.horizontal(|ui| {
                if self.capture.is_some() {
                    if ui.add_enabled(!self.stopping, egui::Button::new("Stop recording")).clicked() {
                        self.stop_recording();
                    }
                } else if ui.add_enabled(can_record && editing, egui::Button::new("Record report")).clicked() {
                    self.start_recording(settings, store);
                }
                if ui.add_enabled(editing && !self.text.trim().is_empty() && self.text.trim().len() <= MAX_TEXT_BYTES, egui::Button::new("Send report")).clicked() {
                    if let Err(error) = self.send_report(settings) { self.notice = error; }
                }
            });
            if !self.notice.is_empty() { ui.add_space(8.0); ui.label(&self.notice); }
            ui.weak("Drafts are kept in memory only. Closing Phorminx discards an unsent draft.");
        });
        if !open {
            self.close_window();
        }
        if self.capture.is_some() || self.response.is_some() {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn qa_pending_send_is_busy_and_failure_preserves_draft_without_recapture() {
        let (sender, receiver) = mpsc::sync_channel(1);
        let mut panel = FeedbackPanel {
            text: "synthetic feedback draft".into(),
            response: Some(receiver),
            ..FeedbackPanel::default()
        };
        assert!(panel.is_busy());
        sender
            .send(Err("synthetic unavailable service".into()))
            .unwrap();
        panel.poll();
        assert!(!panel.is_busy());
        assert_eq!(panel.text, "synthetic feedback draft");
        assert!(panel.capture.is_none());
        assert!(panel.capture_lease.is_none());
    }
    #[test]
    fn qa_blocked_feedback_open_does_not_start_a_microphone_or_hold_a_lease() {
        let directory = tempfile::tempdir().unwrap();
        let store = SettingsStore::new(directory.path().join("settings.toml")).unwrap();
        let mut panel = FeedbackPanel::default();
        panel.open(&Settings::default(), &store, false);
        assert!(panel.open);
        assert!(!panel.is_busy());
        assert!(panel.capture.is_none());
        assert!(panel.capture_lease.is_none());
    }
    #[test]
    fn closing_and_reopening_feedback_preserves_an_unsent_local_draft() {
        let directory = tempfile::tempdir().unwrap();
        let store = SettingsStore::new(directory.path().join("settings.toml")).unwrap();
        let settings = Settings::default();
        let mut panel = FeedbackPanel::default();
        panel.open(&settings, &store, false);
        panel.update_draft("synthetic unsent report".into(), false).unwrap();
        panel.close_window();
        assert!(!panel.open);
        assert_eq!(panel.snapshot().text, "synthetic unsent report");
        assert!(panel.response.is_none());
        panel.open(&settings, &store, false);
        assert!(panel.open);
        assert_eq!(panel.snapshot().text, "synthetic unsent report");
    }
    #[test]
    fn feedback_rejects_empty_and_byte_oversize_before_network() {
        assert!(FeedbackSubmission::new("  ", &Settings::default(), false).is_err());
        assert!(FeedbackSubmission::new(&"é".repeat(6001), &Settings::default(), false).is_err());
    }
    #[test]
    #[cfg(windows)]
    fn feedback_contains_only_explicit_text_and_optional_minimal_diagnostics() {
        let request =
            FeedbackSubmission::new("synthetic report", &Settings::default(), false).unwrap();
        let json = serde_json::to_value(&request).unwrap();
        assert_eq!(request.submission_id.len(), 36);
        assert!(json.get("diagnostics").is_none());
        assert_eq!(json.as_object().unwrap().len(), 3);
        let with = FeedbackSubmission::new("synthetic report", &Settings::default(), true).unwrap();
        assert_eq!(
            serde_json::to_value(with).unwrap()["diagnostics"]
                .as_object()
                .unwrap()
                .len(),
            2
        );
    }

    #[test]
    fn feedback_posts_to_the_apex_host_because_redirects_are_refused() {
        assert!(ENDPOINT.starts_with("https://phorminx.net/"));
    }

    #[test]
    #[ignore = "sends one clearly marked test ticket to the production feedback service"]
    fn live_feedback_service_accepts_and_confirms_a_test_ticket() {
        let request = FeedbackSubmission {
            schema_version: 1,
            submission_id: submission_id().unwrap(),
            text: "TEST - app feedback path verification. Safe to ignore or delete.".into(),
            diagnostics: None,
        };
        assert_eq!(submit(&request).unwrap(), request.submission_id);
    }
}
