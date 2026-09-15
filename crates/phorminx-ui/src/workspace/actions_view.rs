//! Separate the arsenal, its configuration, and deliberate delivery.
//! Navigation never starts recording or sends a request.
use super::*;
use crate::components::page_header;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) enum ActionPage {
    #[default]
    Library,
    Editor,
    Delivery,
}
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) enum EditorStep {
    #[default]
    Identity,
    Destination,
    Payload,
    Access,
}
impl EditorStep {
    fn label(self) -> &'static str {
        match self {
            Self::Identity => "01  Identity",
            Self::Destination => "02  Destination",
            Self::Payload => "03  Payload",
            Self::Access => "04  Access",
        }
    }
    fn next(self) -> Self {
        match self {
            Self::Identity => Self::Destination,
            Self::Destination => Self::Payload,
            Self::Payload | Self::Access => Self::Access,
        }
    }
}
pub(super) fn show(
    ui: &mut Ui,
    s: &WorkspaceSnapshot,
    state: &mut WorkspaceState,
    out: &mut Vec<ShellEvent>,
) {
    state.reconcile(s);
    state.reconcile_action_pages(s);
    match state.action_page {
        ActionPage::Library => library(ui, s, state),
        ActionPage::Editor => editor(ui, s, state, out),
        ActionPage::Delivery => delivery(ui, s, state, out),
    }
}
impl WorkspaceState {
    fn reconcile_action_pages(&mut self, s: &WorkspaceSnapshot) {
        if self
            .action_delete_pending
            .as_ref()
            .is_some_and(|id| !s.actions.iter().any(|a| &a.id == id))
        {
            self.action_delete_pending = None;
            self.discard_action_edits();
        }
        if self
            .delivery_review
            .as_ref()
            .is_some_and(|(revision, target, note)| {
                *revision != s.action_revision
                    || self.selected_action.as_ref() != Some(&target.id)
                    || &self.note != note
                    || s.actions.iter().find(|a| a.id == target.id) != Some(target)
            })
        {
            self.delivery_review = None;
            self.confirm_send = false;
        }
    }
    fn edit_action(&mut self, saved: Option<&ActionDraft>) {
        self.action = saved.cloned().unwrap_or_else(|| ActionDraft {
            method: "POST".into(),
            payload_template: "{\n  \"note\": \"{{text}}\",\n  \"source\": \"phorminx\"\n}".into(),
            ..Default::default()
        });
        self.selected_action = saved.map(|a| a.id.clone());
        self.action_dirty = saved.is_none();
        self.action_pending = None;
        self.action_delete_pending = None;
        self.action_discard_pending = false;
        self.confirm_delete_action = false;
        self.confirm_send = false;
        self.delivery_review = None;
        self.action_page = ActionPage::Editor;
        self.editor_step = EditorStep::Identity;
    }
    fn leave_action_editor(&mut self) {
        if self.action_dirty || self.action_pending.is_some() {
            self.action_discard_pending = true;
        } else {
            self.action_page = ActionPage::Library;
            self.confirm_delete_action = false;
        }
    }
    fn discard_action_edits(&mut self) {
        self.action = ActionDraft::default();
        self.action_dirty = false;
        self.action_pending = None;
        self.selected_action = None;
        self.action_discard_pending = false;
        self.confirm_delete_action = false;
        self.action_page = ActionPage::Library;
    }
    fn review_delivery(&mut self, s: &WorkspaceSnapshot) {
        if s.delivery_busy || s.capture != CaptureState::Idle || self.note.trim().is_empty() {
            return;
        }
        if let Some(target) = s
            .actions
            .iter()
            .find(|a| Some(&a.id) == self.selected_action.as_ref())
        {
            self.delivery_review = Some((s.action_revision, target.clone(), self.note.clone()));
            self.confirm_send = true;
        }
    }
    fn confirm_delivery(&mut self, s: &WorkspaceSnapshot, out: &mut Vec<ShellEvent>) {
        self.reconcile_action_pages(s);
        if s.delivery_busy || s.capture != CaptureState::Idle {
            return;
        }
        if let Some((_, target, note)) = self.delivery_review.take() {
            emit(
                out,
                WorkspaceEvent::SendAction {
                    id: target.id,
                    text: note,
                },
            );
        }
        self.confirm_send = false;
    }
}
fn quiet(ui: &mut Ui, text: impl Into<String>) {
    ui.label(RichText::new(text).color(ui.tokens().secondary_text));
}
fn section_title(ui: &mut Ui, title: &str, detail: &str) {
    ui.label(RichText::new(title).size(22.0).color(ui.tokens().text));
    ui.add_space(Space::SM);
    quiet(ui, detail);
    ui.add_space(Space::LG);
}
fn field_label(ui: &mut Ui, text: &str) {
    ui.label(
        RichText::new(text)
            .size(12.0)
            .color(ui.tokens().secondary_text),
    );
    ui.add_space(Space::XS);
}
fn notice(ui: &mut Ui, s: &WorkspaceSnapshot) {
    if !s.notice.is_empty() {
        ui.add_space(Space::MD);
        ui.label(RichText::new(&s.notice).color(ui.tokens().accent_focus));
    }
}
fn library(ui: &mut Ui, s: &WorkspaceSnapshot, state: &mut WorkspaceState) {
    field_label(ui, "YOUR INSTRUMENTS");
    if page_header(
        ui,
        "Actions",
        "A destination for your next thought.",
        Some("New action"),
    ) {
        state.edit_action(None);
    }
    if s.actions.is_empty() {
        ui.add_space(Space::XL);
        section_title(
            ui,
            "Your next thought, delivered.",
            "Create an action once. Give it a number. Speak when you need it.",
        );
        ui.add_space(Space::XL);
    }
    for saved in &s.actions {
        if library_row(ui, saved).clicked() {
            state.edit_action(Some(saved));
        }
        hairline(ui);
    }
    ui.add_space(Space::LG);
    ui.horizontal_wrapped(|ui| {
        quiet(
            ui,
            format!(
                "{} saved actions · Launcher → number → dictate → stop & send",
                s.actions.len()
            ),
        );
        if action(ui, "Manual delivery", ActionTone::Quiet).clicked() {
            state.action_page = ActionPage::Delivery;
            state.confirm_send = false;
            state.delivery_review = None;
        }
    });
    notice(ui, s);
}

fn library_row(ui: &mut Ui, saved: &ActionDraft) -> egui::Response {
    let tokens = ui.tokens();
    let (row, response) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 76.0), egui::Sense::click());
    if response.hovered() {
        ui.painter().rect_filled(row, 4, tokens.surface);
    }
    // Allocate actual geometry, not a Frame in a row whose cross-axis expands.
    let badge = egui::Rect::from_center_size(
        egui::pos2(row.left() + 16.0, row.center().y),
        egui::Vec2::splat(32.0),
    );
    ui.painter().rect_stroke(
        badge,
        4,
        egui::Stroke::new(1.0, tokens.edge),
        egui::StrokeKind::Inside,
    );
    ui.painter().text(
        badge.center(),
        egui::Align2::CENTER_CENTER,
        saved
            .launcher_slot
            .map_or_else(|| "—".into(), |slot| slot.to_string()),
        egui::FontId::monospace(13.0),
        tokens.secondary_text,
    );
    let metadata_width = if row.width() > 560.0 { 125.0 } else { 24.0 };
    let text_rect = egui::Rect::from_min_max(
        egui::pos2(row.left() + 50.0, row.center().y - 21.0),
        egui::pos2(row.right() - metadata_width - Space::MD, row.bottom()),
    );
    ui.scope_builder(
        egui::UiBuilder::new()
            .max_rect(text_rect)
            .layout(egui::Layout::top_down(egui::Align::Min)),
        |ui| {
            // allocate_ui's requested width is only a maximum. Reserve the column
            // explicitly so short action names cannot pull metadata toward them.
            ui.set_width(text_rect.width());
            ui.add(
                egui::Label::new(RichText::new(&saved.name).size(16.0).color(tokens.text))
                    .truncate(),
            );
            ui.add_space(Space::XS);
            ui.add(
                egui::Label::new(
                    RichText::new(saved.endpoint.trim_start_matches("https://"))
                        .size(12.0)
                        .color(tokens.secondary_text),
                )
                .truncate(),
            );
        },
    );
    let metadata = if row.width() > 560.0 {
        if saved.payload_mode == ActionPayloadMode::AiJson {
            "Local AI JSON   →"
        } else {
            "Template   →"
        }
    } else {
        "→"
    };
    ui.painter().text(
        egui::pos2(row.right() - 2.0, row.center().y),
        egui::Align2::RIGHT_CENTER,
        metadata,
        egui::FontId::proportional(12.0),
        tokens.secondary_text,
    );
    response.widget_info(|| {
        egui::WidgetInfo::labeled(
            egui::WidgetType::Button,
            ui.is_enabled(),
            format!("Edit {}", saved.name),
        )
    });
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}
fn editor(
    ui: &mut Ui,
    s: &WorkspaceSnapshot,
    state: &mut WorkspaceState,
    out: &mut Vec<ShellEvent>,
) {
    if action(ui, "← Actions", ActionTone::Quiet).clicked() {
        state.leave_action_editor();
    }
    ui.add_space(Space::MD);
    page_header(
        ui,
        if state.action.id.is_empty() {
            "New action"
        } else {
            "Edit action"
        },
        &state.action.name,
        None,
    );
    if state.action_discard_pending {
        quiet(ui, "Leave without saving these edits?");
        if state.action_pending.is_some() {
            quiet(
                ui,
                "A save already submitted may still complete. Leaving does not undo it.",
            );
        }
        ui.horizontal_wrapped(|ui| {
            if action(ui, "Keep editing", ActionTone::Primary).clicked() {
                state.action_discard_pending = false;
            }
            if action(ui, "Discard edits", ActionTone::Destructive).clicked() {
                state.discard_action_edits();
            }
        });
        ui.add_space(Space::LG);
        hairline(ui);
    }
    let before = state.action.clone();
    let width = ui.available_width();
    let steps = [
        EditorStep::Identity,
        EditorStep::Destination,
        EditorStep::Payload,
        EditorStep::Access,
    ];
    if width >= 680.0 {
        ui.horizontal_top(|ui| {
            ui.allocate_ui_with_layout(
                egui::vec2(145.0, 320.0),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    ui.set_width(145.0);
                    for step in steps {
                        step_button(ui, state, step);
                        ui.add_space(Space::SM);
                    }
                },
            );
            ui.add_space(Space::LG);
            ui.vertical(|ui| {
                ui.set_width((width - 177.0).min(700.0));
                ui.set_min_height(340.0);
                editor_fields(ui, s, state);
            });
        });
    } else {
        ui.horizontal_wrapped(|ui| {
            for step in steps {
                step_button(ui, state, step);
            }
        });
        ui.add_space(Space::LG);
        ui.set_min_height(400.0);
        editor_fields(ui, s, state);
    }
    if before != state.action {
        state.action_dirty = true;
        state.confirm_delete_action = false;
    }
    notice(ui, s);
    ui.add_space(Space::LG);
    hairline(ui);
    ui.add_space(Space::MD);
    quiet(
        ui,
        "Saving configures one deliberate delivery: trigger, record, then stop to send. Nothing is sent while editing.",
    );
    ui.horizontal_wrapped(|ui| {
        if action(ui, "Cancel", ActionTone::Quiet).clicked() {
            state.leave_action_editor();
        }
        if state.editor_step != EditorStep::Access
            && action(ui, "Continue →", ActionTone::Secondary).clicked()
        {
            state.editor_step = state.editor_step.next();
        }
        // Failed saves have no revision acknowledgement: Save stays retryable.
        if action(ui, "Save action", ActionTone::Primary).clicked() {
            state.save_action(s, out);
        }
        if !state.action_dirty
            && state.action_pending.is_none()
            && !state.action.id.is_empty()
            && action(ui, "Use this action", ActionTone::Quiet).clicked()
        {
            state.action_page = ActionPage::Delivery;
        }
    });
    if !state.action.id.is_empty() {
        ui.add_space(Space::LG);
        ui.add_enabled_ui(s.capture == CaptureState::Idle && !s.delivery_busy, |ui| {
            if action(ui, "Delete action", ActionTone::Quiet).clicked() {
                state.confirm_delete_action = true;
            }
            if state.confirm_delete_action {
                quiet(
                    ui,
                    "Delete this saved action? Meeting transcripts and other actions are kept.",
                );
                ui.horizontal_wrapped(|ui| {
                    if action(ui, "Keep action", ActionTone::Secondary).clicked() {
                        state.confirm_delete_action = false;
                    }
                    if action(ui, "Confirm delete action", ActionTone::Destructive).clicked() {
                        state.action_delete_pending = Some(state.action.id.clone());
                        emit(out, WorkspaceEvent::DeleteAction(state.action.id.clone()));
                    }
                });
            }
        });
    }
}
fn step_button(ui: &mut Ui, state: &mut WorkspaceState, step: EditorStep) {
    let color = if state.editor_step == step {
        ui.tokens().accent_focus
    } else {
        ui.tokens().secondary_text
    };
    if ui
        .selectable_label(
            state.editor_step == step,
            RichText::new(step.label()).color(color),
        )
        .clicked()
    {
        state.editor_step = step;
        state.confirm_delete_action = false;
    }
}

fn editor_fields(ui: &mut Ui, s: &WorkspaceSnapshot, state: &mut WorkspaceState) {
    match state.editor_step {
        EditorStep::Identity => {
            section_title(
                ui,
                "Name this instrument.",
                "A short name you’ll recognize in the picker.",
            );
            field_label(ui, "Action name");
            ui.add(
                TextEdit::singleline(&mut state.action.name)
                    .hint_text("Capture a note")
                    .desired_width(f32::INFINITY)
                    .char_limit(120),
            );
            ui.add_space(Space::LG);
            field_label(ui, "Launcher number");
            egui::ComboBox::from_id_salt("action-launcher-slot")
                .selected_text(
                    state
                        .action
                        .launcher_slot
                        .map_or_else(|| "No shortcut".into(), |slot| slot.to_string()),
                )
                .width(220.0)
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut state.action.launcher_slot, None, "No shortcut");
                    for slot in 3..=9 {
                        let used = s
                            .actions
                            .iter()
                            .any(|a| a.id != state.action.id && a.launcher_slot == Some(slot));
                        ui.add_enabled_ui(!used, |ui| {
                            ui.selectable_value(
                                &mut state.action.launcher_slot,
                                Some(slot),
                                if used {
                                    format!("{slot} · already assigned")
                                } else {
                                    slot.to_string()
                                },
                            );
                        });
                    }
                });
            ui.add_space(Space::MD);
            quiet(ui, "1 is Dictate. 2 starts a meeting.");
        }
        EditorStep::Destination => {
            section_title(
                ui,
                "Where should your words go?",
                "This destination stays fixed, including in AI mode.",
            );
            field_label(ui, "Method");
            egui::ComboBox::from_id_salt("action-http-method")
                .selected_text(&state.action.method)
                .width(160.0)
                .show_ui(ui, |ui| {
                    for method in ["POST", "PUT", "PATCH", "GET", "DELETE"] {
                        ui.selectable_value(&mut state.action.method, method.into(), method);
                    }
                });
            ui.add_space(Space::LG);
            field_label(ui, "Endpoint");
            ui.add(
                TextEdit::singleline(&mut state.action.endpoint)
                    .hint_text("https://notes.example.com/inbox")
                    .desired_width(f32::INFINITY)
                    .char_limit(4096),
            );
            ui.add_space(Space::MD);
            quiet(
                ui,
                "Recording ends when you stop it. Then this action sends once. Uncertain requests are never retried automatically.",
            );
            if state.action.method == "GET" {
                quiet(
                    ui,
                    "GET puts your transcript in the URL query. Use a JSON-body method for private notes.",
                );
            }
        }
        EditorStep::Payload => {
            section_title(
                ui,
                "Turn speech into a request.",
                "Choose how the recorded transcript becomes JSON.",
            );
            ui.horizontal_wrapped(|ui| {
                ui.selectable_value(
                    &mut state.action.payload_mode,
                    ActionPayloadMode::Template,
                    "Template",
                );
                ui.selectable_value(
                    &mut state.action.payload_mode,
                    ActionPayloadMode::AiJson,
                    "Local AI",
                );
            });
            ui.add_space(Space::LG);
            if state.action.payload_mode == ActionPayloadMode::Template {
                field_label(ui, "JSON body");
                ui.add(
                    TextEdit::multiline(&mut state.action.payload_template)
                        .code_editor()
                        .desired_rows(7)
                        .desired_width(f32::INFINITY)
                        .char_limit(32000),
                );
                ui.add_space(Space::MD);
                quiet(
                    ui,
                    "{{text}} is replaced with your transcript inside JSON strings. Quotes and line breaks are escaped; {{delivery_id}} adds the attempt identifier.",
                );
            } else {
                field_label(ui, "Local model");
                egui::ComboBox::from_id_salt("action-payload-model")
                    .selected_text(if state.action.payload_model.is_empty() {
                        "Installed models"
                    } else {
                        &state.action.payload_model
                    })
                    .width(ui.available_width().min(400.0))
                    .show_ui(ui, |ui| {
                        for model in &s.available_models {
                            ui.selectable_value(
                                &mut state.action.payload_model,
                                model.clone(),
                                model,
                            );
                        }
                    });
                ui.add(
                    TextEdit::singleline(&mut state.action.payload_model)
                        .hint_text("Local model identifier")
                        .desired_width(f32::INFINITY)
                        .char_limit(256),
                );
                ui.add_space(Space::LG);
                field_label(ui, "Payload instructions");
                ui.add(TextEdit::multiline(&mut state.action.payload_prompt).hint_text("Return a JSON object with title, note and tags. Keep the note faithful to the transcript. Return JSON only.").desired_rows(6).desired_width(f32::INFINITY).char_limit(16000));
                ui.add_space(Space::MD);
                quiet(
                    ui,
                    "Invalid JSON is never sent. The model cannot change your destination or credentials.",
                );
                if state.action.method == "GET" {
                    ui.label(
                        RichText::new(
                            "Choose POST, PUT, PATCH or DELETE for an AI-generated JSON body.",
                        )
                        .color(ui.tokens().accent_focus),
                    );
                }
            }
        }
        EditorStep::Access => {
            section_title(
                ui,
                "Access, kept separate.",
                "Credentials are not included in model prompts.",
            );
            field_label(ui, "Authorization · optional");
            ui.add(
                TextEdit::singleline(&mut state.action.authorization)
                    .password(true)
                    .hint_text(if state.action.has_credential {
                        "Stored securely · leave blank to keep"
                    } else {
                        "Bearer …"
                    })
                    .desired_width(f32::INFINITY),
            );
            ui.checkbox(
                &mut state.action.clear_credential,
                "Remove saved authorization",
            );
            ui.add_space(Space::LG);
            field_label(ui, "Additional headers · JSON object");
            if !state.action.header_names.is_empty() {
                quiet(
                    ui,
                    format!("Saved names: {}", state.action.header_names.join(", ")),
                );
            }
            ui.add(
                TextEdit::singleline(&mut state.action.headers_json)
                    .password(true)
                    .hint_text("Replacement values · leave blank to keep")
                    .desired_width(f32::INFINITY)
                    .char_limit(32000),
            );
            ui.checkbox(
                &mut state.action.clear_headers,
                "Remove saved custom headers",
            );
            ui.add_space(Space::MD);
            quiet(
                ui,
                "Header values are protected locally. Use a JSON object such as {\"X-Workspace\":\"personal\"}.",
            );
            if s.actions
                .iter()
                .find(|a| Some(&a.id) == state.selected_action.as_ref())
                .is_some_and(|a| a.endpoint != state.action.endpoint)
            {
                ui.label(RichText::new("Destination changed. Saved authorization and custom headers will not transfer; enter new values if needed.").color(ui.tokens().accent_focus));
            }
        }
    }
}

fn delivery(
    ui: &mut Ui,
    s: &WorkspaceSnapshot,
    state: &mut WorkspaceState,
    out: &mut Vec<ShellEvent>,
) {
    if action(ui, "← Actions", ActionTone::Quiet).clicked() {
        state.action_page = ActionPage::Library;
        state.confirm_send = false;
        state.delivery_review = None;
    }
    ui.add_space(Space::MD);
    page_header(
        ui,
        "Manual delivery",
        "Choose a saved destination. Write, or record your next thought.",
        None,
    );
    field_label(ui, "Saved action");
    let selected_name = s
        .actions
        .iter()
        .find(|a| Some(&a.id) == state.selected_action.as_ref())
        .map_or("Choose an action", |a| a.name.as_str());
    egui::ComboBox::from_id_salt("manual-delivery-action")
        .selected_text(selected_name)
        .width(ui.available_width().min(480.0))
        .show_ui(ui, |ui| {
            for saved in &s.actions {
                ui.selectable_value(
                    &mut state.selected_action,
                    Some(saved.id.clone()),
                    &saved.name,
                );
            }
        });
    if let Some(target) = s
        .actions
        .iter()
        .find(|a| Some(&a.id) == state.selected_action.as_ref())
    {
        ui.add_space(Space::SM);
        quiet(ui, format!("{} · {}", target.method, target.endpoint));
        ui.add_space(Space::LG);
        ui.add_enabled_ui(s.capture == CaptureState::Idle && !s.delivery_busy, |ui| {
            if action(ui, "Record & execute", ActionTone::Primary).clicked() {
                emit(out, WorkspaceEvent::TriggerAction(target.id.clone()));
            }
        });
        quiet(
            ui,
            "This recording sends automatically when you stop. For a reviewed delivery, write below instead.",
        );
    }
    ui.add_space(Space::LG);
    hairline(ui);
    ui.add_space(Space::LG);
    field_label(ui, "Your note");
    if ui
        .add(
            TextEdit::multiline(&mut state.note)
                .hint_text("Write a note to review before sending…")
                .desired_rows(7)
                .desired_width(f32::INFINITY)
                .char_limit(64000),
        )
        .changed()
    {
        state.delivery_review = None;
        state.confirm_send = false;
    }
    ui.add_space(Space::MD);
    ui.horizontal_wrapped(|ui| {
        if s.capture == CaptureState::Idle
            && !s.delivery_busy
            && action(ui, "Dictate note", ActionTone::Secondary).clicked()
        {
            emit(
                out,
                WorkspaceEvent::Start {
                    source: CaptureSource::Microphone,
                    device: None,
                    title: "Voice note".into(),
                    note: true,
                },
            );
        }
        if matches!(s.capture, CaptureState::Starting | CaptureState::Listening) {
            if action(
                ui,
                if s.capture_action.is_some() {
                    "Stop & execute"
                } else {
                    "Stop recording"
                },
                ActionTone::Secondary,
            )
            .clicked()
            {
                emit(out, WorkspaceEvent::Stop);
            }
            if s.capture_action.is_some()
                && action(ui, "Cancel recording", ActionTone::Quiet).clicked()
            {
                emit(out, WorkspaceEvent::CancelActionCapture);
            }
        }
        ui.add_enabled_ui(
            s.capture == CaptureState::Idle
                && !s.delivery_busy
                && !state.note.trim().is_empty()
                && state.selected_action.is_some(),
            |ui| {
                if action(ui, "Review delivery", ActionTone::Primary).clicked() {
                    state.review_delivery(s);
                }
            },
        );
    });
    state.reconcile_action_pages(s);
    if let Some((_, target, _)) = &state.delivery_review {
        ui.add_space(Space::LG);
        quiet(
            ui,
            format!(
                "Send this note to {} using {}?",
                target.endpoint, target.method
            ),
        );
        ui.horizontal_wrapped(|ui| {
            ui.add_enabled_ui(!s.delivery_busy && s.capture == CaptureState::Idle, |ui| {
                if action(ui, "Send note", ActionTone::Primary).clicked() {
                    state.confirm_delivery(s, out);
                }
            });
            if action(ui, "Cancel delivery", ActionTone::Quiet).clicked() {
                state.delivery_review = None;
                state.confirm_send = false;
            }
        });
    }
    if s.delivery_busy {
        ui.add_space(Space::MD);
        quiet(ui, "Delivering once. No automatic retries.");
    }
    notice(ui, s);
}

#[cfg(test)]
mod tests {
    use super::*;
    fn text_rect(output: &egui::FullOutput, label: &str) -> Option<egui::Rect> {
        output.shapes.iter().find_map(|shape| match &shape.shape {
            egui::epaint::Shape::Text(text) if text.galley.text() == label => {
                Some(egui::Rect::from_min_size(text.pos, text.galley.size()))
            }
            _ => None,
        })
    }

    #[test]
    fn rendered_library_badge_is_square_and_metadata_anchors_to_right_edge() {
        for width in [360.0, 600.0, 830.0] {
            let ctx = egui::Context::default();
            let mut row = egui::Rect::NOTHING;
            let mut item = saved();
            item.launcher_slot = Some(7);
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(width, 180.0),
                    )),
                    ..Default::default()
                },
                |ui| {
                    row = library_row(ui, &item).rect;
                },
            );
            output.textures_delta.clear();
            let badge = output
                .shapes
                .iter()
                .find_map(|shape| match &shape.shape {
                    egui::epaint::Shape::Rect(rect)
                        if (rect.rect.width() - 32.0).abs() < 0.1
                            && (rect.rect.height() - 32.0).abs() < 0.1 =>
                    {
                        Some(rect.rect)
                    }
                    _ => None,
                })
                .expect("Production row must paint an actual 32 × 32 key");
            assert!((badge.center().y - row.center().y).abs() < 0.1);
            let label = if width > 560.0 {
                "Template   →"
            } else {
                "→"
            };
            let metadata = text_rect(&output, label).unwrap();
            assert!(
                (metadata.right() - (row.right() - 2.0)).abs() < 1.0,
                "Metadata must stay at the far edge for width {width}"
            );
            assert!(text_rect(&output, "Notes").unwrap().left() >= badge.right() + 16.0);
        }
    }

    fn editor_frame(
        ctx: &egui::Context,
        state: &mut WorkspaceState,
        width: f32,
        offset: f32,
        events: Vec<egui::Event>,
        out: &mut Vec<ShellEvent>,
    ) -> (egui::FullOutput, egui::Vec2) {
        let snapshot = WorkspaceSnapshot {
            actions: vec![saved()],
            ..Default::default()
        };
        let mut content = egui::Vec2::ZERO;
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(width, 680.0),
                )),
                events,
                ..Default::default()
            },
            |ui| {
                content = ScrollArea::vertical()
                    .id_salt("actions-editor-render-qa")
                    .auto_shrink([false, false])
                    .vertical_scroll_offset(offset)
                    .show(ui, |ui| show(ui, &snapshot, state, out))
                    .content_size;
            },
        );
        output.textures_delta.clear();
        (output, content)
    }

    #[test]
    fn editor_sections_fit_width_and_save_is_reachable_through_outer_scroll() {
        for width in [600.0, 830.0] {
            for step in [
                EditorStep::Identity,
                EditorStep::Destination,
                EditorStep::Payload,
                EditorStep::Access,
            ] {
                let ctx = egui::Context::default();
                let mut state = WorkspaceState::default();
                state.edit_action(Some(&saved()));
                state.editor_step = step;
                let mut events = vec![];
                let (top, content) =
                    editor_frame(&ctx, &mut state, width, 0.0, vec![], &mut events);
                assert!(
                    text_rect(&top, step.label()).is_some(),
                    "Section navigation must be visible"
                );
                assert!(
                    content.x <= width + 1.0,
                    "Editor must not overflow horizontally at {width}: {}",
                    content.x
                );
                let (bottom, _) = editor_frame(
                    &ctx,
                    &mut state,
                    width,
                    (content.y - 680.0).max(0.0),
                    vec![],
                    &mut events,
                );
                assert!(
                    text_rect(&bottom, "Save action").is_some(),
                    "Save must be reachable on {step:?} at {width}"
                );
                assert!(
                    events.is_empty(),
                    "Rendering or scrolling must never execute an action"
                );
            }
        }
    }

    #[test]
    fn rendered_step_navigation_changes_only_section_and_save_emits_only_configuration() {
        for width in [600.0, 830.0] {
            let ctx = egui::Context::default();
            let mut state = WorkspaceState::default();
            state.edit_action(Some(&saved()));
            let mut out = vec![];
            let (frame, _) = editor_frame(&ctx, &mut state, width, 0.0, vec![], &mut out);
            let target = text_rect(&frame, EditorStep::Destination.label())
                .unwrap()
                .center();
            for pressed in [true, false] {
                editor_frame(
                    &ctx,
                    &mut state,
                    width,
                    0.0,
                    vec![
                        egui::Event::PointerMoved(target),
                        egui::Event::PointerButton {
                            pos: target,
                            button: egui::PointerButton::Primary,
                            pressed,
                            modifiers: egui::Modifiers::NONE,
                        },
                    ],
                    &mut out,
                );
            }
            assert_eq!(state.editor_step, EditorStep::Destination);
            assert!(out.is_empty());
            let (_, content) = editor_frame(&ctx, &mut state, width, 0.0, vec![], &mut out);
            let offset = (content.y - 680.0).max(0.0);
            let (frame, _) = editor_frame(&ctx, &mut state, width, offset, vec![], &mut out);
            let target = text_rect(&frame, "Save action").unwrap().center();
            for pressed in [true, false] {
                editor_frame(
                    &ctx,
                    &mut state,
                    width,
                    offset,
                    vec![
                        egui::Event::PointerMoved(target),
                        egui::Event::PointerButton {
                            pos: target,
                            button: egui::PointerButton::Primary,
                            pressed,
                            modifiers: egui::Modifiers::NONE,
                        },
                    ],
                    &mut out,
                );
            }
            assert!(matches!(
                out.as_slice(),
                [ShellEvent::Workspace(WorkspaceEvent::SaveAction(_))]
            ));
            assert_eq!(state.action_page, ActionPage::Editor);
        }
    }
    fn saved() -> ActionDraft {
        ActionDraft {
            id: "notes".into(),
            name: "Notes".into(),
            endpoint: "https://example.test/notes".into(),
            method: "POST".into(),
            payload_template: "{\"note\":\"{{text}}\"}".into(),
            ..Default::default()
        }
    }
    #[test]
    fn editor_navigation_preserves_unsaved_draft_until_explicit_discard() {
        let mut state = WorkspaceState::default();
        assert_eq!(state.action_page, ActionPage::Library);
        state.edit_action(None);
        state.action.name = "Unfinished note action".into();
        state.editor_step = EditorStep::Payload;
        state.leave_action_editor();
        assert!(state.action_discard_pending);
        assert_eq!(state.action_page, ActionPage::Editor);
        assert_eq!(state.action.name, "Unfinished note action");
        state.discard_action_edits();
        assert_eq!(state.action_page, ActionPage::Library);
        assert!(state.action.name.is_empty());
    }
    #[test]
    fn save_ack_stays_in_editor_and_does_not_replace_newer_edits() {
        let mut state = WorkspaceState::default();
        state.edit_action(None);
        state.action.name = "Submitted".into();
        let mut out = vec![];
        let mut snapshot = WorkspaceSnapshot::default();
        state.save_action(&snapshot, &mut out);
        state.action.name = "Typed while saving".into();
        snapshot.action_revision = 1;
        snapshot.last_saved_action = "assigned-id".into();
        snapshot.actions = vec![ActionDraft {
            id: "assigned-id".into(),
            name: "Submitted".into(),
            ..saved()
        }];
        state.reconcile(&snapshot);
        assert_eq!(state.action_page, ActionPage::Editor);
        assert_eq!(state.action.name, "Typed while saving");
        assert_eq!(state.action.id, "assigned-id");
        assert!(state.action_dirty);
    }
    #[test]
    fn review_requires_current_note_destination_and_save_revision() {
        for change in ["note", "destination", "credential-revision", "selection"] {
            let mut snapshot = WorkspaceSnapshot {
                actions: vec![saved()],
                ..Default::default()
            };
            let mut state = WorkspaceState {
                selected_action: Some("notes".into()),
                note: "Synthetic note".into(),
                ..Default::default()
            };
            state.review_delivery(&snapshot);
            assert!(state.delivery_review.is_some());
            match change {
                "note" => state.note.push('!'),
                "destination" => snapshot.actions[0].endpoint.push_str("/changed"),
                "credential-revision" => snapshot.action_revision += 1,
                _ => state.selected_action = None,
            }
            let mut out = vec![];
            state.confirm_delivery(&snapshot, &mut out);
            assert!(out.is_empty(), "{change}");
            assert!(state.delivery_review.is_none());
        }
    }
    #[test]
    fn confirmed_delivery_emits_once_and_busy_state_cannot_send() {
        let mut snapshot = WorkspaceSnapshot {
            actions: vec![saved()],
            ..Default::default()
        };
        let mut state = WorkspaceState {
            selected_action: Some("notes".into()),
            note: "Synthetic note".into(),
            ..Default::default()
        };
        state.review_delivery(&snapshot);
        snapshot.delivery_busy = true;
        let mut out = vec![];
        state.confirm_delivery(&snapshot, &mut out);
        assert!(out.is_empty());
        snapshot.delivery_busy = false;
        state.confirm_delivery(&snapshot, &mut out);
        state.confirm_delivery(&snapshot, &mut out);
        assert_eq!(
            out,
            vec![ShellEvent::Workspace(WorkspaceEvent::SendAction {
                id: "notes".into(),
                text: "Synthetic note".into()
            })]
        );
    }
    #[test]
    fn delete_waits_for_snapshot_confirmation() {
        let mut state = WorkspaceState::default();
        let mut snapshot = WorkspaceSnapshot {
            actions: vec![saved()],
            ..Default::default()
        };
        state.edit_action(Some(&snapshot.actions[0]));
        state.action_delete_pending = Some("notes".into());
        state.reconcile_action_pages(&snapshot);
        assert_eq!(state.action_page, ActionPage::Editor);
        snapshot.actions.clear();
        state.reconcile_action_pages(&snapshot);
        assert_eq!(state.action_page, ActionPage::Library);
        assert!(state.selected_action.is_none());
    }
}
