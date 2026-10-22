//! Explicit, review-before-send feedback. No automatic diagnostics or audio upload.
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use eframe::egui;
use serde::{Deserialize, Serialize};

use crate::meeting_audio::{MeetingAudioEvent, MeetingAudioService, MeetingAudioSource};
use crate::review::ReformatResult;
use crate::settings::{FormattingStrength, Settings, SettingsStore};

// The apex host: www redirects there, and submissions deliberately refuse
// redirects, so a www endpoint would never reach the service.
const ENDPOINT: &str = "https://phorminx.net/api/v1/feedback";
const MAX_TEXT_BYTES: usize = 12_000;
/// Comments people choose to share on phorminx.net; the team approves them first.
const COMMENTS_ENDPOINT: &str = "https://phorminx.net/api/v1/comments";
const MAX_COMMENT_BYTES: usize = 2_000;

/// A comment for the website: text, an optional first name, and consent.
#[derive(Clone, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CommentSubmission {
    schema_version: u8,
    submission_id: String,
    text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    display_name: Option<String>,
    consent_public: bool,
}

/// A first name or nickname: letters, digits, spaces, dots, apostrophes or
/// hyphens, at most 40 characters. The server applies the same rule.
fn valid_display_name(name: &str) -> bool {
    let mut chars = name.chars();
    name.chars().count() <= 40
        && chars.next().is_some_and(char::is_alphanumeric)
        && chars.all(|c| c.is_alphanumeric() || matches!(c, ' ' | '.' | '\'' | '-'))
}

impl CommentSubmission {
    fn new(text: &str, display_name: &str, consent: bool) -> Result<Self, String> {
        let text = text.trim();
        if text.is_empty() || text.len() > MAX_COMMENT_BYTES {
            return Err("A comment must contain between 1 and 2,000 UTF-8 bytes.".into());
        }
        if !consent {
            return Err("Tick the box to let phorminx.net show your comment, or send it as a report instead.".into());
        }
        let name = display_name.trim();
        if !name.is_empty() && !valid_display_name(name) {
            return Err("Use a first name or nickname: up to 40 letters, digits, spaces, dots, apostrophes or hyphens.".into());
        }
        Ok(Self {
            schema_version: 1,
            submission_id: submission_id()?,
            text: text.into(),
            display_name: (!name.is_empty()).then(|| name.to_owned()),
            consent_public: true,
        })
    }
}

#[derive(Clone)]
enum Outgoing {
    Report(FeedbackSubmission),
    Comment(CommentSubmission),
}

impl Outgoing {
    fn kind(&self) -> &'static str {
        match self {
            Self::Report(_) => "report",
            Self::Comment(_) => "comment",
        }
    }
    fn text(&self) -> &str {
        match self {
            Self::Report(request) => &request.text,
            Self::Comment(request) => &request.text,
        }
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as i64)
        .unwrap_or_default()
}

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
    ticket_id: Option<String>,
    comment_id: Option<String>,
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
    post(ENDPOINT, request, &request.submission_id)
}

fn submit_comment(request: &CommentSubmission) -> Result<String, String> {
    post(COMMENTS_ENDPOINT, request, &request.submission_id)
}

/// Posts `request` and returns the id the service confirmed, which must be
/// the id that was sent.
fn post(endpoint: &str, request: &impl Serialize, submission_id: &str) -> Result<String, String> {
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
        .post(endpoint)
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
    let confirmed = receipt.ticket_id.or(receipt.comment_id).unwrap_or_default();
    if confirmed != submission_id
        || confirmed.len() > 80
        || !confirmed
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-')
    {
        return Err("The feedback receipt was invalid. Retry with the same draft.".into());
    }
    Ok(confirmed)
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
    pending: Option<Outgoing>,
    response: Option<Receiver<Result<String, String>>>,
    /// Share as a public comment instead of a report.
    comment: bool,
    consent: bool,
    display_name: String,
    reformat: Option<Receiver<ReformatResult>>,
    /// The local database that remembers what was sent.
    history: Option<PathBuf>,
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
    pub comment: bool,
    pub consent: bool,
    pub display_name: String,
    pub reformatting: bool,
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
            comment: self.comment,
            consent: self.consent,
            display_name: self.display_name.clone(),
            reformatting: self.reformat.is_some(),
        }
    }

    /// Report or public comment, and the comment's consent and name.
    pub fn set_comment(
        &mut self,
        comment: bool,
        consent: bool,
        display_name: String,
    ) -> Result<(), String> {
        if self.is_busy() {
            return Err("Finish recording or sending before changing this.".into());
        }
        if display_name.chars().count() > 40 {
            return Err("The name is at most 40 characters.".into());
        }
        if self.comment != comment || self.display_name != display_name || self.consent != consent {
            self.pending = None;
        }
        self.comment = comment;
        self.consent = consent;
        self.display_name = display_name;
        Ok(())
    }

    /// Tidies the draft with the local formatting model, as the review
    /// window does, before it is sent.
    pub fn start_reformat(
        &mut self,
        strength: FormattingStrength,
        instruction: Option<&str>,
        settings: &Settings,
    ) -> Result<(), String> {
        if self.is_busy() {
            return Err("Finish recording or sending first.".into());
        }
        let text = self.text.trim().to_owned();
        if text.is_empty() {
            return Err("There is no text to reformat.".into());
        }
        let profile = crate::review::reformat_profile(
            strength,
            settings.formatting.custom_instructions.as_deref(),
            instruction,
        )?;
        let settings = settings.clone();
        let (sender, receiver) = mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("phorminx-feedback-reformat".into())
            .spawn(move || {
                let _ = sender.send(crate::review::reformat_with_local_model(
                    &settings, profile, text,
                ));
            })
            .map_err(|_| "Reformat could not start.".to_owned())?;
        self.reformat = Some(receiver);
        self.notice = "Reformatting with the local model…".into();
        Ok(())
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
        let unchanged = self.pending.as_ref().is_some_and(|pending| match pending {
            Outgoing::Report(request) => {
                !self.comment
                    && request.text == self.text.trim()
                    && request.diagnostics.is_some() == self.diagnostics
            }
            Outgoing::Comment(request) => {
                self.comment
                    && request.text == self.text.trim()
                    && request.display_name.as_deref().unwrap_or_default()
                        == self.display_name.trim()
            }
        });
        if !unchanged {
            self.pending = Some(if self.comment {
                Outgoing::Comment(CommentSubmission::new(
                    &self.text,
                    &self.display_name,
                    self.consent,
                )?)
            } else {
                Outgoing::Report(FeedbackSubmission::new(
                    &self.text,
                    settings,
                    self.diagnostics,
                )?)
            });
        }
        if let Some(pending) = self.pending.clone() {
            let (tx, rx) = mpsc::sync_channel(1);
            std::thread::Builder::new()
                .name("phorminx-feedback".into())
                .spawn(move || {
                    let _ = tx.send(match &pending {
                        Outgoing::Report(request) => submit(request),
                        Outgoing::Comment(request) => submit_comment(request),
                    });
                })
                .map_err(|_| "Could not start sending. Your draft is retained.".to_owned())?;
            self.response = Some(rx);
            self.notice = if self.comment {
                "Sending comment…"
            } else {
                "Sending report…"
            }
            .into();
        }
        Ok(())
    }
    pub fn is_busy(&self) -> bool {
        self.capture.is_some() || self.response.is_some() || self.reformat.is_some()
    }
    pub fn open(&mut self, settings: &Settings, store: &SettingsStore, can_record: bool) {
        self.open = true;
        self.history = store
            .path()
            .parent()
            .map(|directory| directory.join("phorminx.db"));
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
        if let Some(reformat) = &self.reformat {
            match reformat.try_recv() {
                Ok(Ok(result)) => {
                    self.text = result.text;
                    self.pending = None;
                    self.notice = match result.note {
                        Some(note) => format!("{note}. Review it before sending."),
                        None => "Reformatted. Review it before sending.".into(),
                    };
                    self.reformat = None;
                }
                Ok(Err(reason)) => {
                    self.notice = format!("{reason} Your text is unchanged.");
                    self.reformat = None;
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.notice = "Reformat stopped unexpectedly. Your text is unchanged.".into();
                    self.reformat = None;
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        if let Some(response) = &self.response {
            match response.try_recv() {
                Ok(Ok(id)) => {
                    let sent = self.pending.take();
                    let kind = sent.as_ref().map_or("report", Outgoing::kind);
                    if let (Some(sent), Some(history)) = (&sent, &self.history)
                        && let Ok(store) = phorminx_persistence::Persistence::open(history)
                    {
                        let _ = store.feedback().record(&id, kind, sent.text(), now_ms());
                    }
                    self.notice = if kind == "comment" {
                        "Thanks! Your comment will appear on phorminx.net once the team has read it. You can remove it from Feedback.".into()
                    } else {
                        format!(
                            "Report received (ticket {id}). Feedback shows its status; you will be told when it is solved."
                        )
                    };
                    self.text.clear();
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
                if ui.add_enabled(editing && !self.text.trim().is_empty() && self.text.trim().len() <= MAX_TEXT_BYTES, egui::Button::new("Send report")).clicked()
                    && let Err(error) = self.send_report(settings)
                {
                    self.notice = error;
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
        panel
            .update_draft("synthetic unsent report".into(), false)
            .unwrap();
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
    #[cfg(windows)]
    fn comments_need_consent_and_take_only_a_plain_name() {
        // No Debug on submissions: their text must never reach a log.
        assert!(
            CommentSubmission::new("Love it", "", false)
                .err()
                .is_some_and(|error| error.contains("Tick the box"))
        );
        assert!(CommentSubmission::new(" ", "", true).is_err());
        assert!(CommentSubmission::new(&"a".repeat(2001), "", true).is_err());
        assert!(CommentSubmission::new("Love it", "mail me at a@b.c", true).is_err());
        let comment = CommentSubmission::new("  Love it  ", " Ana ", true).unwrap();
        let json = serde_json::to_value(&comment).unwrap();
        assert_eq!(json["text"], "Love it");
        assert_eq!(json["display_name"], "Ana");
        assert_eq!(json["consent_public"], true);
        let anonymous =
            serde_json::to_value(CommentSubmission::new("Love it", "", true).unwrap()).unwrap();
        assert!(anonymous.get("display_name").is_none());
        assert!(COMMENTS_ENDPOINT.starts_with("https://phorminx.net/"));
    }

    #[test]
    fn comment_mode_is_part_of_the_draft() {
        let mut panel = FeedbackPanel::default();
        panel.set_comment(true, true, "Ana".into()).unwrap();
        let view = panel.snapshot();
        assert!(view.comment && view.consent);
        assert_eq!(view.display_name, "Ana");
        assert!(panel.set_comment(true, true, "x".repeat(41)).is_err());
        assert!(valid_display_name("Mary-Jo O'Neil"));
        assert!(!valid_display_name("-dash"));
    }

    #[test]
    fn reformat_needs_text_and_never_runs_while_sending() {
        let mut panel = FeedbackPanel::default();
        let settings = Settings::default();
        assert!(
            panel
                .start_reformat(FormattingStrength::Balanced, None, &settings)
                .is_err()
        );
        let (_sender, receiver) = mpsc::sync_channel(1);
        panel.text = "um so the app froze".into();
        panel.response = Some(receiver);
        assert!(
            panel
                .start_reformat(FormattingStrength::Balanced, None, &settings)
                .is_err()
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
