//! Approved picker-family companion and task-focused meeting workspace.
use super::*;
use crate::components::{page_header, tensioned_p};
use egui::{Align, Button, Frame, Layout, Margin, Sense, Stroke, Vec2};

fn muted(ui: &mut Ui, text: impl Into<String>) {
    ui.label(
        RichText::new(text.into())
            .size(12.0)
            .color(ui.tokens().secondary_text),
    );
}
fn section(ui: &mut Ui, text: &str) {
    ui.label(RichText::new(text).size(21.0));
    ui.add_space(Space::XS);
}
fn tabs<T: PartialEq + Copy>(ui: &mut Ui, selected: &mut T, choices: &[(T, &str)]) {
    let tokens = ui.tokens();
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 22.0;
        for &(value, label) in choices {
            let response = ui.add(
                Button::new(RichText::new(label).color(if *selected == value {
                    tokens.text
                } else {
                    tokens.secondary_text
                }))
                .frame(false)
                .min_size(Vec2::new(0.0, 34.0)),
            );
            if response.clicked() {
                *selected = value;
            }
            if *selected == value {
                let rect = response.rect;
                ui.painter().line_segment(
                    [rect.left_bottom(), rect.right_bottom()],
                    Stroke::new(2.0, tokens.accent),
                );
            }
        }
    });
    hairline(ui);
    ui.add_space(Space::LG);
}

pub(super) fn show(
    ui: &mut Ui,
    s: &WorkspaceSnapshot,
    state: &mut WorkspaceState,
    out: &mut Vec<ShellEvent>,
) {
    if state
        .saved_detail
        .is_some_and(|id| s.session_id != Some(id) && !s.sessions.iter().any(|row| row.id == id))
    {
        state.saved_detail = None;
        state.confirm_delete = None;
    }
    #[cfg(test)]
    if state.tab == Tab::Actions {
        actions_view::show(ui, s, state, out);
        return;
    }
    if state.tab == Tab::Saved && state.saved_detail.is_some() {
        detail(ui, s, state, out);
        return;
    }
    ui.spacing_mut().item_spacing.y = 4.0;
    muted(ui, "CONVERSATIONS");
    ui.label(RichText::new("Meetings").size(28.0));
    ui.add_space(Space::LG);
    tabs(
        ui,
        &mut state.tab,
        &[
            (Tab::Live, "Live session"),
            (Tab::Saved, "Saved sessions"),
            (Tab::Assistant, "Assistant"),
        ],
    );
    if !s.notice.is_empty() {
        muted(ui, &s.notice);
        ui.add_space(Space::MD);
    }
    match state.tab {
        Tab::Live => live(ui, s, state, out),
        Tab::Saved => saved(ui, s, state, out),
        Tab::Assistant => assistant(ui, s, state, out),
        #[cfg(test)]
        Tab::Actions => unreachable!(),
    }
}

fn live(ui: &mut Ui, s: &WorkspaceSnapshot, state: &mut WorkspaceState, out: &mut Vec<ShellEvent>) {
    if s.capture != CaptureState::Idle {
        ui.add_space(Space::LG);
        let tokens = ui.tokens();
        Frame::new()
            .fill(tokens.surface)
            .stroke(Stroke::new(1.0, tokens.edge))
            .corner_radius(8)
            .inner_margin(24)
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                muted(
                    ui,
                    format!(
                        "{} · {}",
                        capture_label(s.capture),
                        timestamp(s.captured_samples)
                    ),
                );
                ui.add_space(Space::MD);
                section(
                    ui,
                    if s.title.is_empty() {
                        "The conversation is in motion."
                    } else {
                        &s.title
                    },
                );
                muted(
                    ui,
                    "Your transcript and assistant live in the floating companion.",
                );
                ui.add_space(Space::LG);
                ui.horizontal_wrapped(|ui| {
                    if action(ui, "Open companion", ActionTone::Primary).clicked() {
                        emit(out, WorkspaceEvent::OpenCompanion);
                    }
                    if matches!(s.capture, CaptureState::Starting | CaptureState::Listening)
                        && action(ui, "Stop recording", ActionTone::Quiet).clicked()
                    {
                        emit(out, WorkspaceEvent::Stop);
                    }
                });
            });
        ui.add_space(Space::LG);
        muted(
            ui,
            if s.history_enabled {
                "Text stays on this device. Audio is not saved."
            } else {
                "History is off. This session is temporary; audio is not saved."
            },
        );
        return;
    }
    ui.add_space(Space::LG);
    section(ui, "What should Phorminx listen to?");
    muted(ui, "Choose once. Launcher 2 remembers.");
    ui.add_space(Space::LG);
    let before = (state.source, device(state));
    ui.horizontal_wrapped(|ui| {
        for (source, title, subtitle) in [
            (
                CaptureSource::SystemAudio,
                "Computer audio",
                "Your meeting, video or call",
            ),
            (CaptureSource::Microphone, "Microphone", "Your voice only"),
        ] {
            let tokens = ui.tokens();
            let label = format!("{title}\n{subtitle}");
            let selected = state.source == source;
            if ui
                .add(
                    Button::new(label)
                        .fill(if selected {
                            tokens.raised
                        } else {
                            tokens.surface
                        })
                        .stroke(Stroke::new(
                            1.0,
                            if selected { tokens.accent } else { tokens.edge },
                        ))
                        .corner_radius(7)
                        .min_size(Vec2::new(220.0_f32.min(ui.available_width()), 76.0)),
                )
                .clicked()
            {
                if state.source != source {
                    state.device.clear();
                }
                state.source = source;
            }
        }
    });
    ui.add_space(Space::LG);
    muted(ui, "Audio device");
    egui::ComboBox::from_id_salt("meeting-audio-device")
        .width(ui.available_width().min(590.0))
        .selected_text(if state.device.is_empty() {
            "System default"
        } else {
            &state.device
        })
        .show_ui(ui, |ui| {
            ui.selectable_value(&mut state.device, String::new(), "System default");
            for name in if state.source == CaptureSource::SystemAudio {
                &s.output_devices
            } else {
                &s.input_devices
            } {
                ui.selectable_value(&mut state.device, name.clone(), name);
            }
        });
    ui.collapsing("Enter a device name", |ui| {
        ui.add(
            TextEdit::singleline(&mut state.device)
                .desired_width(ui.available_width().min(590.0))
                .hint_text("Exact device name · optional"),
        );
    });
    let after = (state.source, device(state));
    if before != after {
        state.capture_preferences_pending = Some(after.clone());
        emit(
            out,
            WorkspaceEvent::SaveCapturePreferences {
                source: after.0,
                device: after.1,
            },
        );
    }
    ui.add_space(Space::LG);
    ui.horizontal_wrapped(|ui| {
        muted(
            ui,
            if s.provider.model.is_empty() {
                "Assistant not configured".to_owned()
            } else {
                format!("{} · {}", s.provider.model, s.provider.kind.label())
            },
        );
        if action(ui, "Change assistant", ActionTone::Quiet).clicked() {
            state.tab = Tab::Assistant;
        }
    });
    ui.add_space(Space::LG);
    hairline(ui);
    ui.add_space(Space::MD);
    ui.horizontal_wrapped(|ui| {
        muted(ui, "Opens the floating companion.");
        if action(ui, "Start listening", ActionTone::Primary).clicked() {
            emit(
                out,
                WorkspaceEvent::Start {
                    source: state.source,
                    device: device(state),
                    title: String::new(),
                    note: false,
                },
            );
            emit(out, WorkspaceEvent::OpenCompanion);
        }
    });
    if s.session_id.is_some() && !s.saved_session {
        ui.add_space(Space::MD);
        if action(ui, "Review this conversation", ActionTone::Quiet).clicked() {
            emit(out, WorkspaceEvent::OpenCompanion);
        }
    }
}

fn saved(
    ui: &mut Ui,
    s: &WorkspaceSnapshot,
    state: &mut WorkspaceState,
    out: &mut Vec<ShellEvent>,
) {
    ui.horizontal_wrapped(|ui| {
        let response = ui.add(
            TextEdit::singleline(&mut state.query)
                .hint_text("Find a topic, not just a word…")
                .desired_width(ui.available_width().min(480.0)),
        );
        let enter = response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        if action(ui, "Search", ActionTone::Secondary).clicked() || enter {
            emit(out, WorkspaceEvent::Search(state.query.clone()));
        }
        if action(ui, "Refresh", ActionTone::Quiet).clicked() {
            emit(out, WorkspaceEvent::Refresh);
        }
    });
    if !s.search_notice.is_empty() {
        muted(ui, &s.search_notice);
    }
    if !s.history_enabled {
        muted(
            ui,
            "Enable history in Settings → Privacy to keep transcripts.",
        );
    }
    ui.add_space(Space::LG);
    if s.sessions.is_empty() {
        section(ui, "A quiet archive.");
        muted(
            ui,
            "Saved conversations appear here. Search by what you remember.",
        );
    }
    for session in &s.sessions {
        ui.push_id(session.id, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.add_enabled_ui(s.capture == CaptureState::Idle, |ui| {
                    if ui
                        .add(
                            Button::new(RichText::new(&session.title).size(16.0))
                                .frame(false)
                                .min_size(Vec2::new(0.0, 48.0)),
                        )
                        .clicked()
                    {
                        state.saved_detail = Some(session.id);
                        state.detail_conversation = false;
                        state.confirm_delete = None;
                        emit(out, WorkspaceEvent::Select(session.id));
                    }
                });
                muted(ui, &session.source);
            });
            hairline(ui);
            ui.add_space(Space::SM);
        });
    }
    if s.capture != CaptureState::Idle {
        muted(
            ui,
            "Finish the live session before opening a saved conversation.",
        );
    }
    ui.add_space(Space::LG);
    muted(ui, "Titles and transcripts stay on this device.");
}

fn detail(
    ui: &mut Ui,
    s: &WorkspaceSnapshot,
    state: &mut WorkspaceState,
    out: &mut Vec<ShellEvent>,
) {
    if action(ui, "← Saved sessions", ActionTone::Quiet).clicked() {
        state.saved_detail = None;
        state.confirm_delete = None;
        return;
    }
    ui.add_space(Space::LG);
    let requested = state.saved_detail;
    if requested != s.session_id || !s.saved_session {
        section(ui, "Opening the conversation…");
        if !s.notice.is_empty() {
            muted(ui, &s.notice);
        }
        return;
    }
    page_header(
        ui,
        if s.title.is_empty() {
            "Saved conversation"
        } else {
            &s.title
        },
        "Saved on this device",
        None,
    );
    tabs(
        ui,
        &mut state.detail_conversation,
        &[(false, "Transcript"), (true, "Conversation")],
    );
    if state.detail_conversation {
        conversation(ui, s, state, out);
    } else {
        transcript(ui, s, out);
    }
    ui.add_space(Space::LG);
    if let Some(id) = requested {
        ui.add_enabled_ui(s.capture == CaptureState::Idle, |ui| {
            if action(ui, "Delete session", ActionTone::Quiet).clicked() {
                state.confirm_delete = Some(id);
            }
            if state.confirm_delete == Some(id) {
                muted(
                    ui,
                    "Delete this session and its chat? This cannot be undone.",
                );
                ui.horizontal_wrapped(|ui| {
                    if action(ui, "Confirm delete session", ActionTone::Destructive).clicked() {
                        emit(out, WorkspaceEvent::Delete(id));
                        state.confirm_delete = None;
                    }
                    if action(ui, "Keep session", ActionTone::Quiet).clicked() {
                        state.confirm_delete = None;
                    }
                });
            }
        });
    }
    if !s.notice.is_empty() {
        muted(ui, &s.notice);
    }
}

fn assistant(
    ui: &mut Ui,
    s: &WorkspaceSnapshot,
    state: &mut WorkspaceState,
    out: &mut Vec<ShellEvent>,
) {
    let before = state.provider.clone();
    section(ui, "Who answers. How they help.");
    muted(ui, "Used by the companion when you ask a question.");
    ui.add_space(Space::LG);
    muted(ui, "Provider");
    egui::ComboBox::from_id_salt("meeting-provider")
        .width(ui.available_width().min(630.0))
        .selected_text(state.provider.kind.label())
        .show_ui(ui, |ui| {
            for kind in [
                ProviderChoice::Ollama,
                ProviderChoice::OpenAi,
                ProviderChoice::Anthropic,
            ] {
                ui.selectable_value(&mut state.provider.kind, kind, kind.label());
            }
        });
    ui.add_space(Space::MD);
    muted(ui, "Model");
    if state.provider.kind == ProviderChoice::Ollama && !s.available_models.is_empty() {
        egui::ComboBox::from_id_salt("meeting-installed-model")
            .width(ui.available_width().min(630.0))
            .selected_text(if state.provider.model.is_empty() {
                "Choose an installed model"
            } else {
                &state.provider.model
            })
            .show_ui(ui, |ui| {
                for model in &s.available_models {
                    ui.selectable_value(&mut state.provider.model, model.clone(), model);
                }
            });
        ui.collapsing("Custom model identifier", |ui| {
            ui.add(
                TextEdit::singleline(&mut state.provider.model).desired_width(ui.available_width()),
            );
        });
    } else {
        ui.add(
            TextEdit::singleline(&mut state.provider.model)
                .hint_text("Model identifier")
                .desired_width(ui.available_width().min(630.0)),
        );
    }
    ui.add_space(Space::MD);
    if state.provider.kind == ProviderChoice::Ollama {
        ui.collapsing("Connection settings", |ui| {
            muted(ui, "Local Ollama address");
            ui.add(
                TextEdit::singleline(&mut state.provider.endpoint)
                    .hint_text("http://127.0.0.1:11434")
                    .desired_width(ui.available_width().min(630.0)),
            );
        });
    } else {
        muted(
            ui,
            "Submitted context is sent to this provider. Charges and its retention policy may apply. No automatic provider fallback.",
        );
        ui.collapsing("API key", |ui| {
            ui.add(
                TextEdit::singleline(&mut state.provider.api_key)
                    .password(true)
                    .hint_text(
                        if s.provider.has_credential && state.provider.kind == s.provider.kind {
                            "Stored key · leave blank to keep"
                        } else {
                            "API key"
                        },
                    )
                    .desired_width(ui.available_width().min(630.0)),
            );
            ui.checkbox(&mut state.provider.clear_credential, "Remove stored key");
            if state.provider.kind != s.provider.kind {
                muted(
                    ui,
                    "Changing providers does not transfer the previous provider’s key.",
                );
            }
        });
    }
    ui.add_space(Space::LG);
    hairline(ui);
    ui.add_space(Space::LG);
    muted(ui, "Meeting preset");
    egui::ComboBox::from_id_salt("meeting-preset").width(ui.available_width().min(630.0)).selected_text("Choose instructions").show_ui(ui, |ui| {
        for (name,prompt) in [("Client questions","Help me answer the client's questions. Use the conversation provided; distinguish facts from suggestions. Keep replies concise."),("Meeting notes","Summarize decisions, open questions and action items from the submitted conversation. Do not invent commitments."),("Critical review","Identify assumptions, unclear requirements and useful follow-up questions in the submitted conversation.")] {
            if ui.selectable_label(state.provider.preset == prompt, name).clicked() { state.provider.preset = prompt.into(); }
        }
    });
    ui.add_space(Space::MD);
    muted(ui, "Instructions");
    ui.add(
        TextEdit::multiline(&mut state.provider.preset)
            .desired_rows(5)
            .desired_width(ui.available_width().min(630.0))
            .char_limit(16000),
    );
    state.provider_dirty |= before != state.provider;
    ui.add_space(Space::LG);
    hairline(ui);
    ui.add_space(Space::MD);
    ui.horizontal_wrapped(|ui| {
        muted(ui, "Only requested context is sent.");
        if action(ui, "Save assistant", ActionTone::Primary).clicked() {
            state.save_provider(s, out);
        }
    });
}

fn timestamp(samples: u64) -> String {
    format!("{:02}:{:02}", samples / 960000, (samples / 16000) % 60)
}
fn capture_label(state: CaptureState) -> &'static str {
    match state {
        CaptureState::Idle => "Ready",
        CaptureState::Starting => "Starting",
        CaptureState::Listening => "Listening",
        CaptureState::Stopping => "Finishing transcript",
    }
}

pub(super) fn companion(
    ui: &mut Ui,
    s: &WorkspaceSnapshot,
    state: &mut WorkspaceState,
    out: &mut Vec<ShellEvent>,
) {
    let tokens = ui.tokens();
    let panel_height = ui.available_height();
    ui.spacing_mut().item_spacing = Vec2::new(8.0, 4.0);
    ui.spacing_mut().button_padding = Vec2::new(8.0, 5.0);
    ui.spacing_mut().interact_size = Vec2::new(30.0, 30.0);
    ui.style_mut()
        .text_styles
        .insert(egui::TextStyle::Body, egui::FontId::proportional(14.0));
    Frame::new()
        .fill(tokens.background)
        .stroke(Stroke::new(1.0, tokens.edge))
        .corner_radius(12)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.set_min_height((panel_height - 2.0).max(0.0));
            if !state.companion_picker && s.capture_action.is_none() {
                let retry = answer_retry_available(s);
                egui::Panel::bottom(ui.id().with("anchored-composer"))
                    .exact_size(if retry { 168.0 } else { 124.0 })
                    .show_separator_line(false)
                    .frame(Frame::new().inner_margin(Margin::symmetric(16, 12)))
                    .show(ui, |ui| {
                        Frame::new()
                            .inner_margin(12)
                            .fill(tokens.surface)
                            .stroke(Stroke::new(1.0, tokens.edge))
                            .corner_radius(7)
                            .show(ui, |ui| {
                                composer(ui, s, state, out);
                            });
                    });
                // Content can scroll/clip above the composer, never cover it.
                ui.set_clip_rect(ui.clip_rect().intersect(ui.available_rect_before_wrap()));
            }
            Frame::new()
                .inner_margin(Margin::symmetric(18, 14))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        let mark = tensioned_p(ui, 28.0);
                        let mark = ui.interact(
                            mark.rect,
                            ui.id().with("companion-picker"),
                            Sense::click(),
                        );
                        mark.widget_info(|| {
                            egui::WidgetInfo::labeled(
                                egui::WidgetType::Button,
                                true,
                                "Open action picker",
                            )
                        });
                        if mark.on_hover_text("Open action picker").clicked() {
                            state.companion_picker = !state.companion_picker;
                        }
                        ui.add_space(Space::XS);
                        let width = (ui.available_width() - 85.0).max(70.0);
                        let response = ui
                            .allocate_ui_with_layout(
                                Vec2::new(width, 42.0),
                                Layout::top_down(Align::Min),
                                |ui| {
                                    ui.set_width(width);
                                    ui.label(RichText::new("PHORMINX").size(11.0));
                                    muted(
                                        ui,
                                        format!(
                                            "{} · {}",
                                            capture_label(s.capture),
                                            timestamp(s.captured_samples)
                                        ),
                                    );
                                },
                            )
                            .response;
                        if ui
                            .interact(response.rect, ui.id().with("companion-drag"), Sense::drag())
                            .drag_started()
                        {
                            ui.ctx().send_viewport_cmd(egui::ViewportCommand::StartDrag);
                        }
                        if ui
                            .add(Button::new("−").frame(false).min_size(Vec2::splat(30.0)))
                            .on_hover_text("Hide companion · listening continues")
                            .clicked()
                        {
                            emit(out, WorkspaceEvent::HideCompanion);
                        }
                        if matches!(s.capture, CaptureState::Starting | CaptureState::Listening)
                            && ui
                                .add(Button::new("■").frame(false).min_size(Vec2::splat(30.0)))
                                .on_hover_text(if s.capture_action.is_some() {
                                    "Stop and execute action"
                                } else {
                                    "Stop listening"
                                })
                                .clicked()
                        {
                            emit(out, WorkspaceEvent::Stop);
                        }
                    });
                });
            hairline(ui);
            if state.companion_picker {
                picker(ui, s, state, out);
                return;
            }
            if s.capture_action.is_some() {
                ScrollArea::vertical()
                    .id_salt("voice-action-companion")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        Frame::new()
                            .inner_margin(20)
                            .show(ui, |ui| action_companion(ui, s, state, out));
                    });
                return;
            }
            Frame::new()
                .fill(tokens.surface)
                .inner_margin(Margin::symmetric(20, 14))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        muted(ui, "LIVE TRANSCRIPT");
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            let label = if state.transcript_expanded {
                                "Less"
                            } else {
                                "Expand"
                            };
                            if ui.add(Button::new(label).frame(false)).clicked() {
                                state.transcript_expanded = !state.transcript_expanded;
                            }
                        });
                    });
                    ui.add_space(Space::XS);
                    let height = if state.transcript_expanded {
                        (panel_height * 0.22).clamp(38.0, 150.0)
                    } else {
                        (panel_height * 0.10).clamp(32.0, 68.0)
                    };
                    ScrollArea::vertical()
                        .id_salt("companion-transcript")
                        .max_height(height)
                        .stick_to_bottom(true)
                        .show(ui, |ui| {
                            let skip = if state.transcript_expanded {
                                s.transcript.len().saturating_sub(12)
                            } else {
                                s.transcript.len().saturating_sub(1)
                            };
                            for line in s.transcript.iter().skip(skip) {
                                ui.label(&line.text);
                                if state.transcript_expanded {
                                    muted(ui, timestamp(line.end_sample));
                                }
                            }
                            if !s.provisional_text.is_empty() {
                                ui.label(
                                    RichText::new(&s.provisional_text)
                                        .italics()
                                        .color(tokens.secondary_text),
                                );
                            }
                            if s.transcript.is_empty() && s.provisional_text.is_empty() {
                                muted(
                                    ui,
                                    if s.capture == CaptureState::Idle {
                                        "Start listening when you’re ready."
                                    } else {
                                        "Listening for the conversation…"
                                    },
                                );
                            }
                        });
                    ui.add_space(Space::XS);
                    muted(
                        ui,
                        format!(
                            "{} · {}",
                            timestamp(s.committed_samples),
                            if s.meeting_source == CaptureSource::SystemAudio {
                                "Computer audio"
                            } else {
                                "Microphone"
                            }
                        ),
                    );
                });
            hairline(ui);
            // Only the conversation scrolls. Header, transcript and composer stay in place.
            Frame::new()
                .inner_margin(Margin::symmetric(20, 16))
                .show(ui, |ui| {
                    if !s.notice.is_empty() {
                        muted(ui, &s.notice);
                    }
                    let status_height = if s.ai_busy || s.send_pending {
                        48.0
                    } else {
                        8.0
                    };
                    let height = (ui.available_height() - status_height).max(0.0);
                    conversation_messages(ui, s, state, out, height);
                });
        });
}

fn picker(
    ui: &mut Ui,
    s: &WorkspaceSnapshot,
    state: &mut WorkspaceState,
    out: &mut Vec<ShellEvent>,
) {
    let choice = ui.input_mut(|input| {
        [
            (1, egui::Key::Num1),
            (2, egui::Key::Num2),
            (3, egui::Key::Num3),
            (4, egui::Key::Num4),
            (5, egui::Key::Num5),
            (6, egui::Key::Num6),
            (7, egui::Key::Num7),
            (8, egui::Key::Num8),
            (9, egui::Key::Num9),
        ]
        .into_iter()
        .find_map(|(slot, key)| {
            let explicit_press = input.events.iter().any(|event| {
                matches!(event,
                egui::Event::Key { key: pressed_key, pressed: true, repeat: false, modifiers, .. }
                if *pressed_key == key && modifiers.matches_exact(egui::Modifiers::NONE))
            });
            (explicit_press && input.consume_key(egui::Modifiers::NONE, key)).then_some(slot)
        })
    });
    if let Some(slot) = choice {
        if slot == 2 {
            state.companion_picker = false;
            if s.capture == CaptureState::Idle {
                emit(out, WorkspaceEvent::StartMeeting);
            }
        } else if s.capture == CaptureState::Idle && !s.delivery_busy {
            if slot == 1 {
                state.companion_picker = false;
                out.push(ShellEvent::TestDictation);
            } else if let Some(target) = s.actions.iter().find(|a| a.launcher_slot == Some(slot)) {
                state.companion_picker = false;
                emit(out, WorkspaceEvent::TriggerAction(target.id.clone()));
            }
        }
    }
    if !state.companion_picker {
        return;
    }
    Frame::new().inner_margin(16).show(ui, |ui| {
        if action(ui, "Return to companion", ActionTone::Quiet).clicked() {
            state.companion_picker = false;
        }
        ui.add_space(Space::SM);
        ui.add_enabled_ui(s.capture == CaptureState::Idle && !s.delivery_busy, |ui| {
            if action(ui, "1   Dictate", ActionTone::Quiet).clicked() {
                state.companion_picker = false;
                out.push(ShellEvent::TestDictation);
            }
        });
        if action(
            ui,
            if s.capture == CaptureState::Idle {
                "2   Start meeting"
            } else {
                "2   Return to meeting"
            },
            ActionTone::Secondary,
        )
        .clicked()
        {
            state.companion_picker = false;
            if s.capture == CaptureState::Idle {
                emit(out, WorkspaceEvent::StartMeeting);
            }
        }
        for target in &s.actions {
            if let Some(slot) = target.launcher_slot {
                ui.add_enabled_ui(s.capture == CaptureState::Idle && !s.delivery_busy, |ui| {
                    if action(ui, &format!("{slot}   {}", target.name), ActionTone::Quiet).clicked()
                    {
                        state.companion_picker = false;
                        emit(out, WorkspaceEvent::TriggerAction(target.id.clone()));
                    }
                });
            }
        }
        ui.add_space(Space::LG);
        if action(ui, "Open Meetings workspace", ActionTone::Quiet).clicked() {
            state.companion_picker = false;
            out.push(ShellEvent::Navigate(crate::model::Route::Meetings));
        }
        if ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
            state.companion_picker = false;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    fn press(
        key: egui::Key,
        capture: CaptureState,
        modifiers: egui::Modifiers,
        repeat: bool,
    ) -> Vec<ShellEvent> {
        let ctx = egui::Context::default();
        let mut state = WorkspaceState {
            companion_picker: true,
            ..Default::default()
        };
        let s = WorkspaceSnapshot {
            capture,
            actions: vec![ActionDraft {
                id: "notes".into(),
                launcher_slot: Some(7),
                ..Default::default()
            }],
            ..Default::default()
        };
        let mut out = vec![];
        if repeat {
            // egui derives repeat from its held-key set, not the supplied flag.
            let mut warm = ctx.run_ui(
                egui::RawInput {
                    events: vec![egui::Event::Key {
                        key,
                        physical_key: None,
                        pressed: true,
                        repeat: false,
                        modifiers,
                    }],
                    ..Default::default()
                },
                |_| {},
            );
            warm.textures_delta.clear();
        }
        let mut result = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(430.0, 680.0),
                )),
                events: vec![egui::Event::Key {
                    key,
                    physical_key: None,
                    pressed: true,
                    repeat,
                    modifiers,
                }],
                ..Default::default()
            },
            |ui| picker(ui, &s, &mut state, &mut out),
        );
        result.textures_delta.clear();
        out
    }
    #[test]
    fn picker_numbers_dispatch_only_explicit_available_choices() {
        let idle = CaptureState::Idle;
        let none = egui::Modifiers::NONE;
        assert_eq!(
            press(egui::Key::Num7, idle, none, false),
            vec![ShellEvent::Workspace(WorkspaceEvent::TriggerAction(
                "notes".into()
            ))]
        );
        assert_eq!(
            press(egui::Key::Num2, idle, none, false),
            vec![ShellEvent::Workspace(WorkspaceEvent::StartMeeting)]
        );
        assert!(press(egui::Key::Num8, idle, none, false).is_empty());
        assert!(press(egui::Key::Num7, CaptureState::Listening, none, false).is_empty());
        assert!(press(egui::Key::Num2, CaptureState::Listening, none, false).is_empty());
        assert!(press(egui::Key::Num7, idle, egui::Modifiers::SHIFT, false).is_empty());
        assert!(press(egui::Key::Num7, idle, none, true).is_empty());
    }
}
