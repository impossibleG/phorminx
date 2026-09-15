//! Meeting presentation. Commands carry intent; this module performs no I/O.
mod actions_view;
mod meeting_view;
use crate::components::{ActionTone, action, hairline};
use crate::model::ShellEvent;
use crate::theme::{Space, UiThemeExt};
use eframe::egui;
use eframe::egui::{RichText, ScrollArea, TextEdit, Ui};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum CaptureSource {
    #[default]
    SystemAudio,
    Microphone,
}
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum CaptureState {
    #[default]
    Idle,
    Starting,
    Listening,
    Stopping,
}
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ProviderChoice {
    #[default]
    Ollama,
    OpenAi,
    Anthropic,
}
impl ProviderChoice {
    pub fn label(self) -> &'static str {
        match self {
            Self::Ollama => "Local · Ollama",
            Self::OpenAi => "External · OpenAI",
            Self::Anthropic => "External · Anthropic",
        }
    }
}
#[derive(Clone, Default, Eq, PartialEq)]
pub struct ProviderDraft {
    pub kind: ProviderChoice,
    pub model: String,
    pub endpoint: String,
    pub api_key: String,
    pub has_credential: bool,
    pub clear_credential: bool,
    pub preset: String,
}
impl std::fmt::Debug for ProviderDraft {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ProviderDraft([REDACTED])")
    }
}
#[derive(Clone, Default, Eq, PartialEq)]
pub struct ActionDraft {
    pub id: String,
    pub name: String,
    pub endpoint: String,
    pub method: String,
    pub payload_template: String,
    pub authorization: String,
    pub has_credential: bool,
    pub clear_credential: bool,
    pub headers_json: String,
    pub clear_headers: bool,
    pub header_names: Vec<String>,
    pub launcher_slot: Option<u8>,
    pub payload_mode: ActionPayloadMode,
    pub payload_model: String,
    pub payload_prompt: String,
}
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ActionPayloadMode {
    #[default]
    Template,
    AiJson,
}
impl std::fmt::Debug for ActionDraft {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ActionDraft([REDACTED])")
    }
}
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MeetingLine {
    pub start_sample: u64,
    pub end_sample: u64,
    pub text: String,
}
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ChatLine {
    pub role: String,
    pub text: String,
    pub status: String,
    pub context_text: String,
    pub context_samples: Option<(u64, u64)>,
}
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SavedMeeting {
    pub id: i64,
    pub title: String,
    pub source: String,
}
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct WorkspaceSnapshot {
    pub meeting_source: CaptureSource,
    pub meeting_device: Option<String>,
    pub revision: u64,
    pub deletion_revision: u64,
    pub deletion_failed: bool,
    pub deletion_notice: String,
    pub session_id: Option<i64>,
    pub saved_session: bool,
    pub title: String,
    pub capture: CaptureState,
    pub captured_samples: u64,
    pub committed_samples: u64,
    pub transcript: Vec<MeetingLine>,
    pub messages: Vec<ChatLine>,
    pub sessions: Vec<SavedMeeting>,
    pub provider: ProviderDraft,
    pub actions: Vec<ActionDraft>,
    pub notice: String,
    pub ai_busy: bool,
    pub send_pending: bool,
    pub delivery_busy: bool,
    pub note_text: String,
    pub note_generation: u64,
    pub provisional_text: String,
    pub shutdown_ready: bool,
    pub shutdown_error: String,
    pub history_enabled: bool,
    pub provider_revision: u64,
    pub action_revision: u64,
    pub last_saved_action: String,
    pub chat_revision: u64,
    pub chat_ack_id: u64,
    pub chat_error_id: u64,
    pub transcript_offset: u64,
    pub total_segments: u64,
    pub input_devices: Vec<String>,
    pub output_devices: Vec<String>,
    pub available_models: Vec<String>,
    pub capture_action: Option<String>,
    pub capture_action_endpoint: String,
    pub search_notice: String,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WorkspaceEvent {
    SaveCapturePreferences {
        source: CaptureSource,
        device: Option<String>,
    },
    TriggerAction(String),
    TriggerActionSlot(u8),
    StartMeeting,
    CancelActionCapture,
    OpenCompanion,
    HideCompanion,
    DeleteSelected {
        dictations: bool,
        meeting_transcripts: bool,
        chats: bool,
    },
    Refresh,
    New,
    Select(i64),
    Delete(i64),
    Search(String),
    Start {
        source: CaptureSource,
        device: Option<String>,
        title: String,
        note: bool,
    },
    Stop,
    SendCutoff,
    SendChat(String),
    SendChatRequest {
        id: u64,
        text: String,
    },
    CancelAnswer,
    RetryAnswer,
    TranscriptPage(u64),
    SendTranscriptPage,
    SaveProvider(ProviderDraft),
    SaveAction(ActionDraft),
    DeleteAction(String),
    SendAction {
        id: String,
        text: String,
    },
}
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum Tab {
    #[default]
    Live,
    Saved,
    Assistant,
    #[cfg(test)]
    Actions,
}
#[derive(Clone, Debug, Default)]
pub(crate) struct WorkspaceState {
    capture_preferences_pending: Option<(CaptureSource, Option<String>)>,
    capture_preferences_seen: Option<(CaptureSource, Option<String>)>,
    tab: Tab,
    source: CaptureSource,
    device: String,
    chat: String,
    query: String,
    provider: ProviderDraft,
    provider_dirty: bool,
    action: ActionDraft,
    action_dirty: bool,
    selected_action: Option<String>,
    action_page: actions_view::ActionPage,
    editor_step: actions_view::EditorStep,
    action_discard_pending: bool,
    action_delete_pending: Option<String>,
    delivery_review: Option<(u64, ActionDraft, String)>,
    saved_detail: Option<i64>,
    detail_conversation: bool,
    transcript_expanded: bool,
    companion_picker: bool,
    note: String,
    note_revision: String,
    note_generation: u64,
    confirm_delete: Option<i64>,
    confirm_delete_action: bool,
    confirm_send: bool,
    revision: u64,
    provider_pending: Option<(u64, ProviderDraft)>,
    action_pending: Option<(u64, ActionDraft)>,
    chat_pending: Option<(u64, String)>,
    chat_request_id: u64,
    markdown: crate::markdown::MarkdownCache,
}
impl WorkspaceState {
    pub fn reconcile(&mut self, snapshot: &WorkspaceSnapshot) {
        let received = (snapshot.meeting_source, snapshot.meeting_device.clone());
        if self.capture_preferences_pending.as_ref() == Some(&received) {
            self.capture_preferences_pending = None;
        }
        if self.capture_preferences_seen.as_ref() != Some(&received)
            && self.capture_preferences_pending.is_none()
        {
            self.source = received.0;
            self.device = received.1.clone().unwrap_or_default();
        }
        self.capture_preferences_seen = Some(received);
        if self
            .provider_pending
            .as_ref()
            .is_some_and(|(revision, _)| snapshot.provider_revision > *revision)
        {
            let (_, submitted) = self.provider_pending.take().expect("checked pending");
            if self.provider == submitted {
                self.provider = snapshot.provider.clone();
                self.provider_dirty = false;
            }
        }
        if self
            .action_pending
            .as_ref()
            .is_some_and(|(revision, draft)| {
                snapshot.action_revision > *revision
                    && (draft.id.is_empty() || draft.id == snapshot.last_saved_action)
            })
        {
            let (_, submitted) = self.action_pending.take().expect("checked pending");
            if let Some(saved) = snapshot
                .actions
                .iter()
                .find(|action| action.id == snapshot.last_saved_action)
            {
                if self.action == submitted {
                    self.action = saved.clone();
                    self.selected_action = Some(saved.id.clone());
                    self.action_dirty = false;
                } else if self.action.id == submitted.id {
                    // Retain edits made during save, but adopt a newly assigned identity
                    // so the next save updates this action instead of creating a duplicate.
                    self.action.id = saved.id.clone();
                    self.selected_action = Some(saved.id.clone());
                }
            }
        }
        if self
            .chat_pending
            .as_ref()
            .is_some_and(|(id, _)| snapshot.chat_ack_id == *id)
        {
            let (_, submitted) = self.chat_pending.take().expect("checked pending");
            if self.chat == submitted {
                self.chat.clear();
            }
        }
        if self
            .chat_pending
            .as_ref()
            .is_some_and(|(id, _)| snapshot.chat_error_id == *id)
        {
            self.chat_pending = None;
        }
        if !self.provider_dirty {
            self.provider = snapshot.provider.clone();
        }
        if self.note_generation != snapshot.note_generation {
            self.note_revision.clear();
            self.note_generation = snapshot.note_generation;
        }
        if self.note_revision != snapshot.note_text {
            let added = snapshot
                .note_text
                .strip_prefix(&self.note_revision)
                .unwrap_or(&snapshot.note_text)
                .trim();
            if !added.is_empty() {
                if !self.note.is_empty() {
                    self.note.push(' ');
                }
                self.note.push_str(added);
            }
            self.note_revision = snapshot.note_text.clone();
        }
        self.revision = snapshot.revision;
    }
    #[cfg(test)]
    pub fn show_actions(&mut self) {
        self.tab = Tab::Actions;
        self.confirm_send = false;
    }
    fn save_provider(&mut self, snapshot: &WorkspaceSnapshot, out: &mut Vec<ShellEvent>) {
        self.provider_dirty = true;
        self.provider_pending = Some((snapshot.provider_revision, self.provider.clone()));
        emit(out, WorkspaceEvent::SaveProvider(self.provider.clone()));
    }
    fn save_action(&mut self, snapshot: &WorkspaceSnapshot, out: &mut Vec<ShellEvent>) {
        self.action_dirty = true;
        self.action_pending = Some((snapshot.action_revision, self.action.clone()));
        emit(out, WorkspaceEvent::SaveAction(self.action.clone()));
    }
    fn send_chat(&mut self, snapshot: &WorkspaceSnapshot, out: &mut Vec<ShellEvent>) {
        if self.chat_pending.is_some() || snapshot.send_pending || self.chat.trim().is_empty() {
            return;
        }
        let previous_id = self
            .chat_request_id
            .max(snapshot.chat_ack_id)
            .max(snapshot.chat_error_id);
        let Some(id) = previous_id.checked_add(1) else {
            return;
        };
        self.chat_request_id = id;
        self.chat_pending = Some((id, self.chat.clone()));
        emit(
            out,
            WorkspaceEvent::SendChatRequest {
                id,
                text: self.chat.clone(),
            },
        );
    }
}
fn emit(out: &mut Vec<ShellEvent>, event: WorkspaceEvent) {
    out.push(ShellEvent::Workspace(event));
}
pub(crate) fn show(
    ui: &mut Ui,
    snapshot: &WorkspaceSnapshot,
    state: &mut WorkspaceState,
    out: &mut Vec<ShellEvent>,
) {
    state.reconcile(snapshot);
    meeting_view::show(ui, snapshot, state, out);
}
pub(crate) fn show_actions(
    ui: &mut Ui,
    snapshot: &WorkspaceSnapshot,
    state: &mut WorkspaceState,
    out: &mut Vec<ShellEvent>,
) {
    state.reconcile(snapshot);
    actions_view::show(ui, snapshot, state, out);
}
pub(crate) fn show_companion(
    ui: &mut Ui,
    snapshot: &WorkspaceSnapshot,
    state: &mut WorkspaceState,
    out: &mut Vec<ShellEvent>,
) {
    state.reconcile(snapshot);
    meeting_view::companion(ui, snapshot, state, out);
}

fn action_companion(
    ui: &mut Ui,
    snapshot: &WorkspaceSnapshot,
    state: &mut WorkspaceState,
    out: &mut Vec<ShellEvent>,
) {
    state.reconcile(snapshot);
    if let Some(name) = &snapshot.capture_action {
        ui.heading(name);
        ui.label("Voice action");
        ui.add_space(Space::MD);
        ui.label(
            if matches!(
                snapshot.capture,
                CaptureState::Starting | CaptureState::Listening
            ) {
                "On stop, your words are delivered to:"
            } else {
                "Destination"
            },
        );
        ui.label(RichText::new(&snapshot.capture_action_endpoint).strong());
        ui.label(format!(
            "{:?} · {:02}:{:02}",
            snapshot.capture,
            snapshot.captured_samples / 960000,
            (snapshot.captured_samples / 16000) % 60
        ));
        ui.add_space(Space::MD);
        if matches!(
            snapshot.capture,
            CaptureState::Starting | CaptureState::Listening
        ) {
            ui.horizontal_wrapped(|ui| {
                if action(ui, "Stop & execute", ActionTone::Primary).clicked() {
                    emit(out, WorkspaceEvent::Stop);
                }
                if action(ui, "Cancel recording", ActionTone::Quiet).clicked() {
                    emit(out, WorkspaceEvent::CancelActionCapture);
                }
            });
        }
        if snapshot.delivery_busy {
            ui.spinner();
            ui.label("Preparing and delivering your note…");
        } else if snapshot.capture == CaptureState::Stopping {
            ui.spinner();
            ui.label("Finishing the transcript…");
        }
        if !snapshot.notice.is_empty() {
            ui.label(&snapshot.notice);
        }
        if !snapshot.note_text.is_empty() {
            ui.add_space(Space::LG);
            ui.label(&snapshot.note_text);
        }
        if !snapshot.provisional_text.is_empty() {
            ui.label(RichText::new(&snapshot.provisional_text).italics());
        }
        if snapshot.capture == CaptureState::Idle {
            ui.add_space(Space::LG);
            ui.label("No automatic retries. Open Actions to review the retained note or deliberately start another action.");
            if action(ui, "Open Actions", ActionTone::Secondary).clicked() {
                out.push(ShellEvent::Navigate(crate::model::Route::Actions));
            }
        }
    }
}
fn device(state: &WorkspaceState) -> Option<String> {
    let value = state.device.trim();
    (!value.is_empty()).then(|| value.to_owned())
}
fn transcript(ui: &mut Ui, s: &WorkspaceSnapshot, out: &mut Vec<ShellEvent>) {
    transcript_content(ui, s, out, 420.0);
}
fn transcript_content(ui: &mut Ui, s: &WorkspaceSnapshot, out: &mut Vec<ShellEvent>, height: f32) {
    ui.heading("Transcript");
    hairline(ui);
    if s.saved_session && s.capture == CaptureState::Idle && s.total_segments > 0 {
        ui.horizontal_wrapped(|ui| {
            ui.label(format!(
                "Segments {}–{} of {}",
                s.transcript_offset + 1,
                (s.transcript_offset + 100).min(s.total_segments),
                s.total_segments
            ));
            ui.add_enabled_ui(s.transcript_offset > 0, |ui| {
                if ui.button("Previous page").clicked() {
                    emit(
                        out,
                        WorkspaceEvent::TranscriptPage(s.transcript_offset.saturating_sub(100)),
                    );
                }
            });
            ui.add_enabled_ui(
                s.transcript_offset.saturating_add(100) < s.total_segments,
                |ui| {
                    if ui.button("Next page").clicked() {
                        emit(
                            out,
                            WorkspaceEvent::TranscriptPage(s.transcript_offset.saturating_add(100)),
                        );
                    }
                },
            );
        });
        ui.label("Send this page submits only these segments to the assistant, not the entire recording.");
        ui.add_enabled_ui(!s.send_pending && !s.transcript.is_empty(), |ui| {
            if action(ui, "Send this page", ActionTone::Secondary).clicked() {
                emit(out, WorkspaceEvent::SendTranscriptPage);
            }
        });
    }
    ScrollArea::vertical()
        .id_salt(("meeting-transcript", s.session_id, s.transcript_offset))
        .max_height(height)
        .stick_to_bottom(s.capture != CaptureState::Idle)
        .show(ui, |ui| {
            if s.transcript.is_empty() {
                ui.label("Your next conversation starts here.");
            }
            for line in &s.transcript {
                if line.text.is_empty() {
                    continue;
                }
                ui.label(
                    RichText::new(format!(
                        "{:02}:{:02}",
                        line.start_sample / 960000,
                        (line.start_sample / 16000) % 60
                    ))
                    .small(),
                );
                ui.label(&line.text);
                ui.add_space(Space::SM);
            }
            if !s.provisional_text.is_empty() {
                ui.label(
                    RichText::new(&s.provisional_text)
                        .italics()
                        .color(ui.visuals().weak_text_color()),
                );
            }
        });
}
fn conversation(
    ui: &mut Ui,
    s: &WorkspaceSnapshot,
    state: &mut WorkspaceState,
    out: &mut Vec<ShellEvent>,
) {
    conversation_content(ui, s, state, out, 420.0);
}
fn conversation_content(
    ui: &mut Ui,
    s: &WorkspaceSnapshot,
    state: &mut WorkspaceState,
    out: &mut Vec<ShellEvent>,
    height: f32,
) {
    conversation_messages(ui, s, state, out, height);
    composer(ui, s, state, out);
}
fn conversation_messages(
    ui: &mut Ui,
    s: &WorkspaceSnapshot,
    state: &mut WorkspaceState,
    out: &mut Vec<ShellEvent>,
    height: f32,
) {
    let tokens = ui.tokens();
    if s.ai_busy || s.send_pending {
        ui.horizontal_wrapped(|ui| {
            ui.label(
                RichText::new(&s.provider.model)
                    .small()
                    .color(tokens.secondary_text),
            );
            if s.ai_busy && action(ui, "Stop answer", ActionTone::Quiet).clicked() {
                emit(out, WorkspaceEvent::CancelAnswer);
            }
            if s.send_pending {
                ui.spinner();
                ui.label("Attaching new context…");
            }
        });
        ui.add_space(Space::SM);
    }
    ScrollArea::vertical()
        .id_salt("meeting-messages")
        .max_height(height.max(8.0))
        .auto_shrink([false, false])
        .stick_to_bottom(true)
        .show(ui, |ui| {
            if s.messages.is_empty() {
                ui.label(RichText::new("An answer, when you need one.").size(20.0));
                ui.add_space(Space::XS);
                ui.label(
                    RichText::new(
                        "Ask about this conversation. New transcript is included automatically.",
                    )
                    .color(tokens.secondary_text),
                );
            }
            for (index, line) in s.messages.iter().enumerate() {
                ui.push_id(index, |ui| {
                    if !line.context_text.is_empty() || line.context_samples.is_some() {
                        let label = line.context_samples.map_or_else(
                            || "Meeting context attached".to_owned(),
                            |(start, end)| {
                                format!(
                                    "Meeting context · {:02}:{:02}–{:02}:{:02}",
                                    start / 960000,
                                    (start / 16000) % 60,
                                    end / 960000,
                                    (end / 16000) % 60
                                )
                            },
                        );
                        ui.label(RichText::new(label).size(11.0).color(tokens.secondary_text));
                        ui.add_space(Space::XXS);
                    }
                    if line.role.eq_ignore_ascii_case("assistant") {
                        let label = if s.provider.model.is_empty() {
                            "ASSISTANT".into()
                        } else {
                            format!("{} · ASSISTANT", s.provider.model)
                        };
                        ui.label(RichText::new(label).size(11.0).color(tokens.secondary_text));
                        state.markdown.show(ui, index, &line.text);
                        if !matches!(line.status.as_str(), "complete" | "sent" | "") {
                            ui.label(
                                RichText::new(&line.status)
                                    .small()
                                    .color(tokens.secondary_text),
                            );
                        }
                    } else {
                        let response = egui::Frame::new()
                            .inner_margin(egui::Margin {
                                left: 13,
                                right: 0,
                                top: 4,
                                bottom: 4,
                            })
                            .show(ui, |ui| {
                                ui.label(&line.text);
                            });
                        let rect = response.response.rect;
                        ui.painter().line_segment(
                            [rect.left_top(), rect.left_bottom()],
                            egui::Stroke::new(2.0, tokens.accent),
                        );
                    }
                    ui.add_space(Space::LG);
                });
            }
        });
    state.markdown.retain(s.messages.len());
}
fn composer(
    ui: &mut Ui,
    s: &WorkspaceSnapshot,
    state: &mut WorkspaceState,
    out: &mut Vec<ShellEvent>,
) {
    let edit_id = ui.make_persistent_id("meeting-chat-composer");
    let focused = ui.memory(|memory| memory.has_focus(edit_id));
    let ime_id = edit_id.with("ime-active");
    let mut ime_active = focused && ui.data(|data| data.get_temp::<bool>(ime_id).unwrap_or(false));
    let enter = ui.input_mut(|input| {
        let mut composing_this_frame = ime_active;
        for event in &input.events {
            if focused && let egui::Event::Ime(event) = event {
                composing_this_frame = true;
                match event {
                    egui::ImeEvent::Preedit { text, .. } => ime_active = !text.is_empty(),
                    egui::ImeEvent::Commit(_) => ime_active = false,
                    _ => {}
                }
            }
        }
        // `consume_key(NONE, Enter)` also matches Shift+Enter in egui's
        // logical modifier matching. Inspect exact modifiers, and do not let
        // held-key repeat generate multiple network submissions.
        if !focused || composing_this_frame {
            return false;
        }
        let mut submit = false;
        input.events.retain(|event| {
            if let egui::Event::Key {
                key: egui::Key::Enter,
                pressed: true,
                repeat,
                modifiers,
                ..
            } = event
                && modifiers.matches_exact(egui::Modifiers::NONE)
            {
                submit |= !repeat;
                false
            } else {
                true
            }
        });
        submit
    });
    ui.data_mut(|data| data.insert_temp(ime_id, ime_active));
    ScrollArea::vertical()
        .id_salt("composer-input-scroll")
        .max_height(48.0)
        .show(ui, |ui| {
            ui.add(
                TextEdit::multiline(&mut state.chat)
                    .id(edit_id)
                    .frame(egui::Frame::NONE)
                    .return_key(egui::KeyboardShortcut::new(
                        egui::Modifiers::SHIFT,
                        egui::Key::Enter,
                    ))
                    .hint_text("Ask about this conversation…")
                    .desired_rows(2)
                    .desired_width(f32::INFINITY)
                    .char_limit(32000),
            );
        });
    let can_send = !s.send_pending && state.chat_pending.is_none() && !state.chat.trim().is_empty();
    ui.add_enabled_ui(can_send, |ui| {
        let clicked = if ui.available_width() < 500.0 {
            let mut clicked = false;
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new("New transcript included automatically")
                        .size(11.0)
                        .color(ui.tokens().secondary_text),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let response = ui.add(
                        egui::Button::new(RichText::new("↑").color(ui.tokens().on_accent))
                            .fill(ui.tokens().accent)
                            .min_size(egui::Vec2::splat(30.0)),
                    );
                    response.widget_info(|| {
                        egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "Send message")
                    });
                    clicked = response.on_hover_text("Send message · Enter").clicked();
                });
            });
            clicked
        } else {
            let clicked = action(ui, "Send message", ActionTone::Primary).clicked();
            ui.label(RichText::new("Enter to send · Shift+Enter for a new line").size(11.0));
            clicked
        };
        if (clicked || enter) && can_send {
            state.send_chat(s, out);
        }
    });
    if answer_retry_available(s) && action(ui, "Retry answer", ActionTone::Quiet).clicked() {
        emit(out, WorkspaceEvent::RetryAnswer);
    }
}
fn answer_retry_available(s: &WorkspaceSnapshot) -> bool {
    !s.ai_busy
        && !s.send_pending
        && s.messages.last().is_some_and(|line| {
            line.role.eq_ignore_ascii_case("assistant")
                && matches!(
                    line.status.to_ascii_lowercase().as_str(),
                    "failed" | "error" | "incomplete" | "stopped" | "cancelled"
                )
        })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn action_companion_discloses_destination_and_hides_meeting_chat() {
        let context = egui::Context::default();
        let snapshot = WorkspaceSnapshot {
            capture_action: Some("Notes".into()),
            capture_action_endpoint: "https://example.test/notes".into(),
            capture: CaptureState::Listening,
            messages: vec![ChatLine {
                text: "Unrelated meeting answer".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let mut state = WorkspaceState::default();
        let mut out = vec![];
        let mut frame = context.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(520.0, 760.0),
                )),
                ..Default::default()
            },
            |ui| show_companion(ui, &snapshot, &mut state, &mut out),
        );
        let text: String = frame
            .shapes
            .iter()
            .filter_map(|shape| match &shape.shape {
                egui::epaint::Shape::Text(text) => Some(text.galley.text()),
                _ => None,
            })
            .collect();
        assert!(text.contains("https://example.test/notes"));
        assert!(text.contains("Stop & execute"));
        assert!(text.contains("Cancel recording"));
        assert!(!text.contains("Unrelated meeting answer"));
        assert!(!text.contains("Send message"));
        assert!(out.is_empty());
        frame.textures_delta.clear();
    }
    #[test]
    fn action_companion_retains_result_without_record_or_retry_side_effects() {
        for busy in [true, false] {
            let context = egui::Context::default();
            let snapshot = WorkspaceSnapshot {
                capture_action: Some("Notes".into()),
                capture_action_endpoint: "https://example.test/notes".into(),
                capture: CaptureState::Idle,
                delivery_busy: busy,
                notice: if busy {
                    "Preparing"
                } else {
                    "Delivery failed. Your note is retained."
                }
                .into(),
                note_text: "Retained note".into(),
                ..Default::default()
            };
            let mut state = WorkspaceState::default();
            let mut out = vec![];
            let mut frame = context.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(520.0, 760.0),
                    )),
                    ..Default::default()
                },
                |ui| show_companion(ui, &snapshot, &mut state, &mut out),
            );
            let text: String = frame
                .shapes
                .iter()
                .filter_map(|shape| match &shape.shape {
                    egui::epaint::Shape::Text(text) => Some(text.galley.text()),
                    _ => None,
                })
                .collect();
            assert!(text.contains("Retained note"));
            assert!(text.contains("Open Actions"));
            assert!(!text.contains("Stop & execute"));
            assert!(!text.contains("Cancel recording"));
            assert!(!text.contains("Send message"));
            assert_eq!(text.contains("Preparing and delivering"), busy);
            assert!(out.is_empty());
            frame.textures_delta.clear();
        }
    }
    fn key(modifiers: egui::Modifiers) -> egui::Event {
        egui::Event::Key {
            key: egui::Key::Enter,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        }
    }
    fn key_release() -> egui::Event {
        egui::Event::Key {
            key: egui::Key::Enter,
            physical_key: None,
            pressed: false,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }
    }
    #[test]
    fn enter_sends_shift_enter_edits_and_ime_commit_never_sends() {
        let context = egui::Context::default();
        companion_surface(&context, true);
        let snapshot = WorkspaceSnapshot::default();
        let mut state = WorkspaceState {
            chat: "A unique question".into(),
            ..Default::default()
        };
        let mut out = vec![];
        click(
            &context,
            &mut state,
            &snapshot,
            &mut out,
            "A unique question",
        );
        paint(
            &context,
            &mut state,
            &snapshot,
            &mut out,
            vec![key(egui::Modifiers::SHIFT), key_release()],
        );
        assert!(state.chat.contains('\n'), "Shift+Enter inserts a newline");
        assert!(out.is_empty());
        paint(
            &context,
            &mut state,
            &snapshot,
            &mut out,
            vec![
                egui::Event::Ime(egui::ImeEvent::Preedit {
                    text: "候補".into(),
                    active_range_chars: None,
                }),
                key(egui::Modifiers::NONE),
                key_release(),
            ],
        );
        assert!(out.is_empty(), "Composition must not submit");
        paint(
            &context,
            &mut state,
            &snapshot,
            &mut out,
            vec![
                egui::Event::Ime(egui::ImeEvent::Commit("確定".into())),
                key(egui::Modifiers::NONE),
                key_release(),
            ],
        );
        assert!(out.is_empty(), "IME commit Enter is not a send gesture");
        paint(
            &context,
            &mut state,
            &snapshot,
            &mut out,
            vec![key(egui::Modifiers::NONE), key_release()],
        );
        assert!(matches!(
            out.last(),
            Some(ShellEvent::Workspace(
                WorkspaceEvent::SendChatRequest { .. }
            ))
        ));
        assert_eq!(out.len(), 1);
    }
    #[test]
    fn transcript_context_is_compact_and_never_rendered_as_message_text() {
        let context = egui::Context::default();
        companion_surface(&context, true);
        let snapshot = WorkspaceSnapshot {
            messages: vec![ChatLine {
                role: "You".into(),
                text: "What should I say?".into(),
                context_text: "DO NOT LEAK THIS TRANSCRIPT TO THE CHAT VIEW".into(),
                context_samples: Some((0, 32000)),
                ..Default::default()
            }],
            ..Default::default()
        };
        let mut state = WorkspaceState::default();
        let mut out = vec![];
        let frame = paint(&context, &mut state, &snapshot, &mut out, vec![]);
        let text: String = frame
            .shapes
            .iter()
            .filter_map(|shape| match &shape.shape {
                egui::epaint::Shape::Text(text) => Some(text.galley.text()),
                _ => None,
            })
            .collect();
        assert!(text.contains("Meeting context"));
        assert!(!text.contains("DO NOT LEAK"));
        assert!(!text.contains("Send up to here"));
        assert!(out.is_empty());
    }
    #[test]
    fn saved_action_recording_is_explicit_and_cancellable() {
        let context = egui::Context::default();
        let draft = ActionDraft {
            id: "notes".into(),
            name: "Notes".into(),
            launcher_slot: Some(7),
            ..Default::default()
        };
        let mut snapshot = WorkspaceSnapshot {
            actions: vec![draft.clone()],
            ..Default::default()
        };
        let mut state = WorkspaceState {
            tab: Tab::Actions,
            selected_action: Some("notes".into()),
            action_page: actions_view::ActionPage::Delivery,
            action: draft,
            ..Default::default()
        };
        let mut out = vec![];
        click(
            &context,
            &mut state,
            &snapshot,
            &mut out,
            "Record & execute",
        );
        assert_eq!(
            out.last(),
            Some(&ShellEvent::Workspace(WorkspaceEvent::TriggerAction(
                "notes".into()
            )))
        );
        snapshot.capture_action = Some("Notes".into());
        snapshot.capture = CaptureState::Listening;
        click(
            &context,
            &mut state,
            &snapshot,
            &mut out,
            "Cancel recording",
        );
        assert_eq!(
            out.last(),
            Some(&ShellEvent::Workspace(WorkspaceEvent::CancelActionCapture))
        );
    }
    #[test]
    fn dictated_note_appends_without_overwriting_manual_edits() {
        let mut state = WorkspaceState {
            note: "Typed introduction.".into(),
            ..Default::default()
        };
        let mut snapshot = WorkspaceSnapshot {
            note_generation: 1,
            note_text: "First sentence.".into(),
            ..Default::default()
        };
        state.reconcile(&snapshot);
        assert_eq!(state.note, "Typed introduction. First sentence.");
        state.note.push_str(" Manual edit.");
        snapshot.note_text.push_str(" Second sentence.");
        state.reconcile(&snapshot);
        assert_eq!(
            state.note,
            "Typed introduction. First sentence. Manual edit. Second sentence."
        );
        snapshot.note_generation = 2;
        snapshot.note_text = "First sentence.".into();
        state.reconcile(&snapshot);
        assert!(state.note.ends_with("Second sentence. First sentence."));
        let unchanged = state.note.clone();
        state.reconcile(&snapshot);
        assert_eq!(state.note, unchanged);
    }
    #[test]
    fn workspace_routes_paint_and_emit_no_side_effects() {
        for tab in [Tab::Live, Tab::Saved, Tab::Assistant, Tab::Actions] {
            let mut state = WorkspaceState {
                tab,
                ..Default::default()
            };
            let mut out = vec![];
            egui::__run_test_ui(|ui| show(ui, &WorkspaceSnapshot::default(), &mut state, &mut out));
            assert!(out.is_empty());
        }
    }
    #[test]
    fn credential_drafts_are_redacted() {
        let provider = ProviderDraft {
            api_key: "never-log-me".into(),
            ..Default::default()
        };
        assert!(!format!("{provider:?}").contains("never-log-me"));
    }
    fn companion_surface(context: &egui::Context, enabled: bool) {
        context.data_mut(|data| data.insert_temp(egui::Id::new("test-surface-companion"), enabled));
    }
    fn paint(
        context: &egui::Context,
        state: &mut WorkspaceState,
        snapshot: &WorkspaceSnapshot,
        out: &mut Vec<ShellEvent>,
        events: Vec<egui::Event>,
    ) -> egui::FullOutput {
        let mut output = context.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    context
                        .data(|data| {
                            data.get_temp::<egui::Vec2>(egui::Id::new("test-surface-size"))
                        })
                        .unwrap_or(egui::vec2(1200.0, 1800.0)),
                )),
                events,
                ..Default::default()
            },
            |ui| {
                if context.data(|data| {
                    data.get_temp::<bool>(egui::Id::new("test-surface-companion"))
                        .unwrap_or(false)
                }) {
                    show_companion(ui, snapshot, state, out);
                } else if state.tab == Tab::Actions {
                    show_actions(ui, snapshot, state, out);
                } else {
                    show(ui, snapshot, state, out);
                }
            },
        );
        output.textures_delta.clear();
        output
    }
    fn click(
        context: &egui::Context,
        state: &mut WorkspaceState,
        snapshot: &WorkspaceSnapshot,
        out: &mut Vec<ShellEvent>,
        label: &str,
    ) {
        for theme in [egui::Theme::Light, egui::Theme::Dark] {
            context.style_mut_of(theme, |style| style.animation_time = 0.0);
        }
        let frame = paint(context, state, snapshot, out, vec![]);
        let center = frame
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::epaint::Shape::Text(text)
                    if text.galley.text() == label
                        || (label == "Send message" && text.galley.text() == "↑") =>
                {
                    Some(text.pos + text.galley.size() / 2.0)
                }
                _ => None,
            })
            .unwrap_or_else(|| panic!("Missing clickable label: {label}"));
        let size = context
            .data(|data| data.get_temp::<egui::Vec2>(egui::Id::new("test-surface-size")))
            .unwrap_or(egui::vec2(1200.0, 1800.0));
        assert!(
            egui::Rect::from_min_size(egui::Pos2::ZERO, size).contains(center),
            "Clickable {label} is outside {size:?}: {center:?}"
        );
        for pressed in [true, false] {
            paint(
                context,
                state,
                snapshot,
                out,
                vec![
                    egui::Event::PointerMoved(center),
                    egui::Event::PointerButton {
                        pos: center,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
            );
        }
    }
    #[test]
    fn provider_save_keeps_secret_on_failure_then_clears_only_on_success_ack() {
        let context = egui::Context::default();
        let mut snapshot = WorkspaceSnapshot::default();
        let mut state = WorkspaceState {
            tab: Tab::Assistant,
            provider_dirty: true,
            provider: ProviderDraft {
                kind: ProviderChoice::OpenAi,
                model: "synthetic-model".into(),
                api_key: "synthetic-secret".into(),
                ..Default::default()
            },
            ..Default::default()
        };
        let mut out = vec![];
        click(&context, &mut state, &snapshot, &mut out, "Save assistant");
        assert!(matches!(
            out.last(),
            Some(ShellEvent::Workspace(WorkspaceEvent::SaveProvider(_)))
        ));
        snapshot.notice = "Could not save".into();
        snapshot.revision += 1;
        paint(&context, &mut state, &snapshot, &mut out, vec![]);
        assert_eq!(state.provider.api_key, "synthetic-secret");
        assert!(state.provider_dirty);
        snapshot.provider = ProviderDraft {
            kind: ProviderChoice::OpenAi,
            model: "synthetic-model".into(),
            has_credential: true,
            ..Default::default()
        };
        snapshot.provider_revision += 1;
        paint(&context, &mut state, &snapshot, &mut out, vec![]);
        assert!(state.provider.api_key.is_empty());
        assert!(state.provider.has_credential);
        assert!(!state.provider_dirty);
    }
    #[test]
    fn action_save_keeps_secret_and_selects_generated_id_after_success() {
        let context = egui::Context::default();
        let mut snapshot = WorkspaceSnapshot::default();
        let mut state = WorkspaceState {
            tab: Tab::Actions,
            action_page: actions_view::ActionPage::Editor,
            editor_step: actions_view::EditorStep::Access,
            action_dirty: true,
            action: ActionDraft {
                name: "Notes".into(),
                method: "POST".into(),
                endpoint: "https://example.test/notes".into(),
                authorization: "Bearer synthetic-secret".into(),
                headers_json: r#"{"X-Token":"synthetic-header"}"#.into(),
                ..Default::default()
            },
            ..Default::default()
        };
        let mut out = vec![];
        click(&context, &mut state, &snapshot, &mut out, "Save action");
        assert!(matches!(
            out.last(),
            Some(ShellEvent::Workspace(WorkspaceEvent::SaveAction(_)))
        ));
        snapshot.notice = "Could not save".into();
        snapshot.revision += 1;
        paint(&context, &mut state, &snapshot, &mut out, vec![]);
        assert_eq!(state.action.authorization, "Bearer synthetic-secret");
        assert!(state.action_dirty);
        assert!(state.action.headers_json.contains("synthetic-header"));
        let mut saved = state.action.clone();
        saved.id = "generated-id".into();
        saved.authorization.clear();
        saved.headers_json.clear();
        saved.header_names = vec!["X-Token".into()];
        saved.has_credential = true;
        snapshot.actions.push(saved);
        snapshot.action_revision += 1;
        snapshot.last_saved_action = "generated-id".into();
        paint(&context, &mut state, &snapshot, &mut out, vec![]);
        assert_eq!(state.selected_action.as_deref(), Some("generated-id"));
        assert!(state.action.authorization.is_empty());
        assert!(!state.action_dirty);
        assert!(state.action.headers_json.is_empty());
        assert_eq!(state.action.header_names, vec!["X-Token"]);
    }
    #[test]
    fn chat_text_survives_failed_send_and_newer_text_survives_old_ack() {
        let context = egui::Context::default();
        companion_surface(&context, true);
        let mut snapshot = WorkspaceSnapshot::default();
        let mut state = WorkspaceState {
            chat: "Original question".into(),
            ..Default::default()
        };
        let mut out = vec![];
        click(&context, &mut state, &snapshot, &mut out, "Send message");
        assert_eq!(state.chat, "Original question");
        snapshot.notice = "Request rejected".into();
        snapshot.revision += 1;
        paint(&context, &mut state, &snapshot, &mut out, vec![]);
        assert_eq!(state.chat, "Original question");
        state.chat = "New question typed while pending".into();
        snapshot.chat_ack_id = state.chat_pending.as_ref().unwrap().0;
        paint(&context, &mut state, &snapshot, &mut out, vec![]);
        assert_eq!(state.chat, "New question typed while pending");
        click(&context, &mut state, &snapshot, &mut out, "Send message");
        snapshot.chat_ack_id = state.chat_pending.as_ref().unwrap().0;
        paint(&context, &mut state, &snapshot, &mut out, vec![]);
        assert!(state.chat.is_empty());
    }
    #[test]
    fn pending_chat_blocks_button_and_enter_and_old_ack_cannot_clear_new_draft() {
        let context = egui::Context::default();
        companion_surface(&context, true);
        let mut snapshot = WorkspaceSnapshot::default();
        let mut state = WorkspaceState {
            chat: "Question A".into(),
            ..Default::default()
        };
        let mut out = vec![];
        click(&context, &mut state, &snapshot, &mut out, "Send message");
        let first = state.chat_pending.as_ref().unwrap().0;
        state.chat = "Question B".into();
        click(&context, &mut state, &snapshot, &mut out, "Send message");
        click(&context, &mut state, &snapshot, &mut out, "Question B");
        paint(
            &context,
            &mut state,
            &snapshot,
            &mut out,
            vec![key(egui::Modifiers::NONE), key_release()],
        );
        assert_eq!(
            out.len(),
            1,
            "Local pending protects the window before the host snapshot arrives"
        );
        snapshot.chat_ack_id = first;
        paint(&context, &mut state, &snapshot, &mut out, vec![]);
        assert_eq!(state.chat, "Question B");
        assert!(state.chat_pending.is_none());
        paint(
            &context,
            &mut state,
            &snapshot,
            &mut out,
            vec![key(egui::Modifiers::NONE), key_release()],
        );
        assert_eq!(out.len(), 2);
        let second = state.chat_pending.as_ref().unwrap().0;
        assert!(second > first);
        snapshot.chat_revision += 5;
        paint(&context, &mut state, &snapshot, &mut out, vec![]);
        assert_eq!(state.chat, "Question B");
        assert_eq!(
            state.chat_pending.as_ref().unwrap().0,
            second,
            "An old ack and unrelated revision cannot acknowledge B"
        );
        snapshot.chat_ack_id = second;
        paint(&context, &mut state, &snapshot, &mut out, vec![]);
        assert!(state.chat.is_empty());
    }
    #[test]
    fn matching_chat_error_unlocks_retry_without_losing_text() {
        let mut snapshot = WorkspaceSnapshot {
            chat_ack_id: 40,
            chat_error_id: 39,
            ..Default::default()
        };
        let mut state = WorkspaceState {
            chat: "Keep this question".into(),
            ..Default::default()
        };
        let mut out = vec![];
        state.send_chat(&snapshot, &mut out);
        let first = state.chat_pending.as_ref().unwrap().0;
        assert_eq!(first, 41);
        snapshot.chat_error_id = 40;
        state.reconcile(&snapshot);
        assert!(state.chat_pending.is_some());
        snapshot.chat_error_id = first;
        state.reconcile(&snapshot);
        assert!(state.chat_pending.is_none());
        assert_eq!(state.chat, "Keep this question");
        state.send_chat(&snapshot, &mut out);
        assert_eq!(state.chat_pending.as_ref().unwrap().0, 42);
        assert_eq!(out.len(), 2);
    }
    #[test]
    fn action_ack_keeps_newer_edits_without_creating_a_second_action() {
        let mut snapshot = WorkspaceSnapshot::default();
        let mut state = WorkspaceState {
            action: ActionDraft {
                name: "First name".into(),
                ..Default::default()
            },
            ..Default::default()
        };
        let mut out = vec![];
        state.save_action(&snapshot, &mut out);
        let mut saved = state.action.clone();
        saved.id = "saved-identity".into();
        state.action.name = "Edited while saving".into();
        snapshot.actions.push(saved);
        snapshot.last_saved_action = "saved-identity".into();
        snapshot.action_revision += 1;
        state.reconcile(&snapshot);
        assert_eq!(state.action.name, "Edited while saving");
        assert_eq!(state.action.id, "saved-identity");
        assert!(state.action_dirty);
        state.save_action(&snapshot, &mut out);
        assert!(
            matches!(out.last(),Some(ShellEvent::Workspace(WorkspaceEvent::SaveAction(draft))) if draft.id=="saved-identity")
        );
    }
    #[test]
    fn navigation_and_unrelated_refresh_preserve_unsaved_drafts() {
        let context = egui::Context::default();
        let snapshot = WorkspaceSnapshot::default();
        let provider = ProviderDraft {
            model: "unsaved model".into(),
            api_key: "unsaved secret".into(),
            ..Default::default()
        };
        let action = ActionDraft {
            name: "Unsaved action".into(),
            authorization: "unsaved authorization".into(),
            ..Default::default()
        };
        let mut state = WorkspaceState {
            provider: provider.clone(),
            provider_dirty: true,
            action: action.clone(),
            action_dirty: true,
            chat: "Unsaved question".into(),
            ..Default::default()
        };
        let mut out = vec![];
        for label in ["Assistant", "Saved sessions", "Live session"] {
            click(&context, &mut state, &snapshot, &mut out, label);
        }
        assert_eq!(state.provider, provider);
        assert_eq!(state.action, action);
        assert_eq!(state.chat, "Unsaved question");
        assert!(out.is_empty());
    }
    #[test]
    fn failed_answer_retry_and_starting_note_stop_emit_explicit_intents() {
        let context = egui::Context::default();
        companion_surface(&context, true);
        let mut snapshot = WorkspaceSnapshot {
            messages: vec![ChatLine {
                role: "Assistant".into(),
                text: "partial".into(),
                status: "failed".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let mut state = WorkspaceState::default();
        let mut out = vec![];
        click(&context, &mut state, &snapshot, &mut out, "Retry answer");
        assert_eq!(
            out.last(),
            Some(&ShellEvent::Workspace(WorkspaceEvent::RetryAnswer))
        );
        state.show_actions();
        companion_surface(&context, false);
        snapshot.capture = CaptureState::Starting;
        click(&context, &mut state, &snapshot, &mut out, "Manual delivery");
        click(&context, &mut state, &snapshot, &mut out, "Stop recording");
        assert_eq!(
            out.last(),
            Some(&ShellEvent::Workspace(WorkspaceEvent::Stop))
        );
    }
    #[test]
    fn saved_transcript_paging_and_send_are_explicit() {
        let context = egui::Context::default();
        let snapshot = WorkspaceSnapshot {
            session_id: Some(7),
            total_segments: 250,
            saved_session: true,
            transcript_offset: 100,
            transcript: vec![MeetingLine {
                start_sample: 16000,
                end_sample: 32000,
                text: "synthetic segment".into(),
            }],
            ..Default::default()
        };
        let mut state = WorkspaceState {
            tab: Tab::Saved,
            saved_detail: Some(7),
            ..Default::default()
        };
        let mut out = vec![];
        click(&context, &mut state, &snapshot, &mut out, "Previous page");
        assert_eq!(
            out.last(),
            Some(&ShellEvent::Workspace(WorkspaceEvent::TranscriptPage(0)))
        );
        click(&context, &mut state, &snapshot, &mut out, "Next page");
        assert_eq!(
            out.last(),
            Some(&ShellEvent::Workspace(WorkspaceEvent::TranscriptPage(200)))
        );
        click(&context, &mut state, &snapshot, &mut out, "Send this page");
        assert_eq!(
            out.last(),
            Some(&ShellEvent::Workspace(WorkspaceEvent::SendTranscriptPage))
        );
    }
    #[test]
    fn source_changes_reset_device_selection_and_model_choices_paint() {
        let context = egui::Context::default();
        let snapshot = WorkspaceSnapshot {
            input_devices: vec!["Synthetic microphone".into()],
            output_devices: vec!["Synthetic speakers".into()],
            available_models: vec!["synthetic-local-model".into()],
            ..Default::default()
        };
        let mut state = WorkspaceState {
            device: "Synthetic speakers".into(),
            ..Default::default()
        };
        let mut out = vec![];
        click(
            &context,
            &mut state,
            &snapshot,
            &mut out,
            "Microphone\nYour voice only",
        );
        assert!(state.device.is_empty());
        assert_eq!(state.source, CaptureSource::Microphone);
        state.device = "Synthetic microphone".into();
        click(
            &context,
            &mut state,
            &snapshot,
            &mut out,
            "Computer audio\nYour meeting, video or call",
        );
        assert!(state.device.is_empty());
        click(&context, &mut state, &snapshot, &mut out, "Assistant");
        click(
            &context,
            &mut state,
            &snapshot,
            &mut out,
            "Choose an installed model",
        );
        click(
            &context,
            &mut state,
            &snapshot,
            &mut out,
            "synthetic-local-model",
        );
        assert_eq!(state.provider.model, "synthetic-local-model");
        assert!(state.provider_dirty);
        assert_eq!(
            out,
            vec![
                ShellEvent::Workspace(WorkspaceEvent::SaveCapturePreferences {
                    source: CaptureSource::Microphone,
                    device: None
                }),
                ShellEvent::Workspace(WorkspaceEvent::SaveCapturePreferences {
                    source: CaptureSource::SystemAudio,
                    device: None
                }),
            ]
        );
    }

    #[test]
    fn capture_preferences_load_on_restart_and_do_not_clobber_pending_edits() {
        let mut snapshot = WorkspaceSnapshot {
            meeting_source: CaptureSource::Microphone,
            meeting_device: Some("Saved mic".into()),
            ..Default::default()
        };
        let mut state = WorkspaceState::default();
        state.reconcile(&snapshot);
        assert_eq!(state.source, CaptureSource::Microphone);
        assert_eq!(state.device, "Saved mic");
        state.source = CaptureSource::SystemAudio;
        state.device = "New output".into();
        state.capture_preferences_pending = Some((state.source, device(&state)));
        state.reconcile(&snapshot);
        assert_eq!(state.device, "New output");
        snapshot.meeting_source = CaptureSource::SystemAudio;
        snapshot.meeting_device = Some("New output".into());
        state.reconcile(&snapshot);
        assert!(state.capture_preferences_pending.is_none());
        assert_eq!(state.device, "New output");
    }

    fn rendered_text(frame: &egui::FullOutput) -> String {
        frame
            .shapes
            .iter()
            .filter_map(|shape| match &shape.shape {
                egui::epaint::Shape::Text(text) => Some(text.galley.text()),
                _ => None,
            })
            .collect()
    }
    #[test]
    fn live_surface_is_a_control_room_not_transcript_chat_or_action_editor() {
        for capture in [CaptureState::Idle, CaptureState::Listening] {
            let context = egui::Context::default();
            let snapshot = WorkspaceSnapshot {
                capture,
                title: "Synthetic session".into(),
                transcript: vec![MeetingLine {
                    start_sample: 0,
                    end_sample: 10,
                    text: "PRIVATE LIVE TRANSCRIPT FIXTURE".into(),
                }],
                messages: vec![ChatLine {
                    role: "Assistant".into(),
                    text: "PRIVATE CHAT FIXTURE".into(),
                    status: "complete".into(),
                    ..Default::default()
                }],
                ..Default::default()
            };
            let mut state = WorkspaceState {
                chat: "PRIVATE COMPOSER DRAFT".into(),
                action: ActionDraft {
                    payload_template: "PRIVATE ACTION EDITOR".into(),
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut out = vec![];
            let frame = paint(&context, &mut state, &snapshot, &mut out, vec![]);
            let text = rendered_text(&frame);
            for forbidden in [
                "PRIVATE LIVE TRANSCRIPT FIXTURE",
                "PRIVATE CHAT FIXTURE",
                "PRIVATE COMPOSER DRAFT",
                "PRIVATE ACTION EDITOR",
                "Send message",
                "Save action",
            ] {
                assert!(
                    !text.contains(forbidden),
                    "Live leaked another surface: {forbidden}"
                );
            }
            assert!(text.contains(if capture == CaptureState::Idle {
                "Start listening"
            } else {
                "Open companion"
            }));
            assert!(out.is_empty());
        }
    }
    #[test]
    fn saved_detail_waits_for_the_matching_loaded_session_before_showing_text() {
        let context = egui::Context::default();
        let mut snapshot = WorkspaceSnapshot {
            session_id: Some(8),
            saved_session: true,
            sessions: vec![SavedMeeting {
                id: 7,
                title: "Requested session".into(),
                source: "system".into(),
            }],
            title: "Requested session".into(),
            transcript: vec![MeetingLine {
                start_sample: 0,
                end_sample: 10,
                text: "MATCHED TRANSCRIPT".into(),
            }],
            messages: vec![ChatLine {
                role: "Assistant".into(),
                text: "MATCHED CONVERSATION".into(),
                status: "complete".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let mut state = WorkspaceState {
            tab: Tab::Saved,
            saved_detail: Some(7),
            ..Default::default()
        };
        let mut out = vec![];
        let text = rendered_text(&paint(&context, &mut state, &snapshot, &mut out, vec![]));
        assert!(text.contains("Opening the conversation"));
        assert!(!text.contains("MATCHED"));
        snapshot.session_id = Some(7);
        snapshot.saved_session = false;
        assert!(
            !rendered_text(&paint(&context, &mut state, &snapshot, &mut out, vec![]))
                .contains("MATCHED")
        );
        snapshot.saved_session = true;
        let text = rendered_text(&paint(&context, &mut state, &snapshot, &mut out, vec![]));
        assert!(text.contains("MATCHED TRANSCRIPT"));
        assert!(!text.contains("MATCHED CONVERSATION"));
        click(&context, &mut state, &snapshot, &mut out, "Conversation");
        let text = rendered_text(&paint(&context, &mut state, &snapshot, &mut out, vec![]));
        assert!(text.contains("MATCHED CONVERSATION"));
        assert!(!text.contains("MATCHED TRANSCRIPT"));
        assert!(out.is_empty());
    }
    #[test]
    fn companion_hide_never_stops_capture() {
        let context = egui::Context::default();
        companion_surface(&context, true);
        let snapshot = WorkspaceSnapshot {
            capture: CaptureState::Listening,
            ..Default::default()
        };
        let mut state = WorkspaceState::default();
        let mut out = vec![];
        click(&context, &mut state, &snapshot, &mut out, "−");
        assert_eq!(
            out,
            vec![ShellEvent::Workspace(WorkspaceEvent::HideCompanion)]
        );
        assert_eq!(snapshot.capture, CaptureState::Listening);
    }
    #[test]
    fn companion_composer_remains_reachable_at_compact_and_minimum_sizes_with_long_content() {
        for size in [egui::vec2(430.0, 680.0), egui::vec2(360.0, 420.0)] {
            for theme in [
                crate::theme::ThemeMode::AuthoredDark,
                crate::theme::ThemeMode::AuthoredLight,
            ] {
                let context = egui::Context::default();
                companion_surface(&context, true);
                crate::theme::apply(&context, theme);
                context.data_mut(|data| data.insert_temp(egui::Id::new("test-surface-size"), size));
                let snapshot = WorkspaceSnapshot {
                    capture: CaptureState::Listening,
                    transcript: vec![MeetingLine {
                        start_sample: 0,
                        end_sample: 16000,
                        text: "Long transcript passage ".repeat(200),
                    }],
                    messages: (0..15)
                        .map(|_| ChatLine {
                            role: "Assistant".into(),
                            text: "Long assistant answer ".repeat(100),
                            status: "complete".into(),
                            ..Default::default()
                        })
                        .collect(),
                    ..Default::default()
                };
                let mut state = WorkspaceState {
                    chat: "Compact window question".into(),
                    transcript_expanded: true,
                    ..Default::default()
                };
                let mut out = vec![];
                let frame = paint(&context, &mut state, &snapshot, &mut out, vec![]);
                let rect = frame
                    .shapes
                    .iter()
                    .find_map(|shape| match &shape.shape {
                        egui::epaint::Shape::Text(text)
                            if matches!(text.galley.text(), "Send message" | "↑") =>
                        {
                            Some(egui::Rect::from_min_size(text.pos, text.galley.size()))
                        }
                        _ => None,
                    })
                    .unwrap_or_else(|| {
                        panic!("Composer send button did not render at {size:?} in {theme:?}")
                    });
                assert!(
                    rect.top() >= 0.0
                        && rect.bottom() <= size.y
                        && rect.left() >= 0.0
                        && rect.right() <= size.x,
                    "Composer is clipped at {size:?} in {theme:?}: {rect:?}"
                );
                click(&context, &mut state, &snapshot, &mut out, "Send message");
                assert!(matches!(
                    out.as_slice(),
                    [ShellEvent::Workspace(
                        WorkspaceEvent::SendChatRequest { .. }
                    )]
                ));
            }
        }
    }
    #[test]
    fn long_multiline_composer_draft_keeps_send_control_reachable() {
        let context = egui::Context::default();
        companion_surface(&context, true);
        crate::theme::apply(&context, crate::theme::ThemeMode::AuthoredDark);
        context.data_mut(|data| {
            data.insert_temp(egui::Id::new("test-surface-size"), egui::vec2(430.0, 680.0))
        });
        let snapshot = WorkspaceSnapshot {
            capture: CaptureState::Listening,
            ..Default::default()
        };
        let mut state = WorkspaceState {
            chat: "A multiline question with context\n".repeat(12),
            ..Default::default()
        };
        let mut out = vec![];
        click(&context, &mut state, &snapshot, &mut out, "Send message");
        assert!(matches!(
            out.as_slice(),
            [ShellEvent::Workspace(
                WorkspaceEvent::SendChatRequest { .. }
            )]
        ));
    }
}
