//! Approved picker-family companion and task-focused meeting workspace.
use super::*;
use crate::components::page_header;
use egui::{Align, Button, CursorIcon, Frame, Layout, Margin, Sense, Stroke, Vec2};

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
    let (eyebrow, title, subtitle) = match state.tab {
        Tab::Live => (
            "MEETINGS / LIVE ROOM",
            "The listening room.",
            "Every voice in its place. Every thought within reach.",
        ),
        Tab::Saved => (
            "MEETINGS / YOUR CONVERSATIONS",
            "The record.",
            "Find the moment, not just the date.",
        ),
        Tab::Settings => (
            "MEETINGS / YOUR RULES",
            "Set the room.",
            "A few decisions before the conversation begins. None during it.",
        ),
        #[cfg(test)]
        Tab::Actions => unreachable!(),
    };
    ui.add_space(Space::MD);
    ui.label(
        RichText::new(eyebrow)
            .size(10.0)
            .strong()
            .color(ui.tokens().secondary_text),
    );
    ui.add_space(Space::XS);
    ui.label(RichText::new(title).size(38.0).color(ui.tokens().text));
    muted(ui, subtitle);
    ui.add_space(Space::LG);
    tabs(
        ui,
        &mut state.tab,
        &[
            (Tab::Live, "Live session"),
            (Tab::Saved, "Saved sessions"),
            (Tab::Settings, "Meeting settings"),
        ],
    );
    if !s.notice.is_empty() {
        muted(ui, &s.notice);
        ui.add_space(Space::MD);
    }
    match state.tab {
        Tab::Live => live(ui, s, state, out),
        Tab::Saved => saved(ui, s, state, out),
        Tab::Settings => settings(ui, s, state, out),
        #[cfg(test)]
        Tab::Actions => unreachable!(),
    }
}

fn live(ui: &mut Ui, s: &WorkspaceSnapshot, state: &mut WorkspaceState, out: &mut Vec<ShellEvent>) {
    if s.capture != CaptureState::Idle {
        let tokens = ui.tokens();
        Frame::new()
            .fill(tokens.surface)
            .stroke(Stroke::new(1.0, tokens.edge))
            .inner_margin(Margin::symmetric(20, 14))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal_wrapped(|ui| {
                    ui.label(
                        RichText::new("SESSION IN PROGRESS")
                            .size(10.0)
                            .strong()
                            .color(tokens.verified),
                    );
                    ui.separator();
                    ui.label(
                        RichText::new(if s.title.is_empty() {
                            "Live conversation"
                        } else {
                            &s.title
                        })
                        .size(17.0),
                    );
                    muted(ui, timestamp(s.captured_samples));
                    if action(ui, "Open companion", ActionTone::Secondary).clicked() {
                        emit(out, WorkspaceEvent::OpenCompanion);
                    }
                    if matches!(s.capture, CaptureState::Starting | CaptureState::Listening)
                        && action(ui, "Stop recording", ActionTone::Primary).clicked()
                    {
                        emit(out, WorkspaceEvent::Stop);
                    }
                });
            });
        ui.add_space(Space::MD);
        muted(ui, "The live transcript and assistant stay in the separate companion window.");
        ui.add_space(Space::MD);
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
    ui.add_space(Space::MD);
    section(ui, "Ready for the conversation.");
    muted(
        ui,
        "Phorminx listens to your microphone and computer audio together.",
    );
    ui.add_space(Space::LG);
    Frame::new()
        .fill(ui.tokens().surface)
        .stroke(Stroke::new(1.0, ui.tokens().edge))
        .inner_margin(20)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            muted(ui, "01 / CAPTURE");
            ui.add_space(Space::SM);
            ui.label(RichText::new("Computer + microphone").size(19.0));
            muted(
                ui,
                format!(
                    "Computer: {} · Microphone: {}",
                    s.meeting_device.as_deref().unwrap_or("Windows default"),
                    s.meeting_microphone_device
                        .as_deref()
                        .unwrap_or("Windows default")
                ),
            );
            ui.add_space(Space::MD);
            ui.horizontal_wrapped(|ui| {
                if action(ui, "Start listening", ActionTone::Primary).clicked() {
                    emit(out, WorkspaceEvent::StartMeeting);
                    emit(out, WorkspaceEvent::OpenCompanion);
                }
                if action(ui, "Meeting settings", ActionTone::Quiet).clicked() {
                    state.tab = Tab::Settings;
                }
            });
        });
    ui.add_space(Space::LG);
    muted(ui, "Start listening opens the companion. Speech becomes text; audio is not saved.");
    if s.session_id.is_some() && !s.saved_session {
        ui.add_space(Space::MD);
        if action(ui, "Review this conversation", ActionTone::Quiet).clicked() {
            emit(out, WorkspaceEvent::OpenCompanion);
        }
    }
}

fn settings(
    ui: &mut Ui,
    s: &WorkspaceSnapshot,
    state: &mut WorkspaceState,
    out: &mut Vec<ShellEvent>,
) {
    ui.add_space(Space::MD);
    ui.label(RichText::new("01 / CAPTURE").size(10.0).strong().color(ui.tokens().secondary_text));
    section(ui, "Capture");
    muted(
        ui,
        "Phorminx listens to your microphone and computer together. Pause either source from the live room.",
    );
    if s.capture != CaptureState::Idle {
        muted(
            ui,
            "Finish the current meeting to change audio, translation, or assistant settings.",
        );
        ui.disable();
    }
    if state.meeting_devices_pending.is_some() {
        ui.disable();
    }
    ui.add_space(Space::LG);
    muted(ui, "Computer audio device");
    let before = (device(state), microphone_device(state));
    egui::ComboBox::from_id_salt("meeting-audio-device")
        .width(ui.available_width().min(590.0))
        .selected_text(if state.device.is_empty() {
            "System default"
        } else {
            &state.device
        })
        .show_ui(ui, |ui| {
            ui.selectable_value(&mut state.device, String::new(), "System default");
            for name in &s.output_devices {
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
    ui.add_space(Space::MD);
    muted(ui, "Microphone input device");
    egui::ComboBox::from_id_salt("meeting-microphone-device")
        .width(ui.available_width().min(590.0))
        .selected_text(if state.microphone_device.is_empty() {
            "Windows default microphone"
        } else {
            &state.microphone_device
        })
        .show_ui(ui, |ui| {
            ui.selectable_value(
                &mut state.microphone_device,
                String::new(),
                "Windows default microphone",
            );
            for name in &s.input_devices {
                ui.selectable_value(&mut state.microphone_device, name.clone(), name);
            }
        });
    let after = (device(state), microphone_device(state));
    if before != after {
        state.meeting_devices_dirty = after
            != (
                s.meeting_device.clone(),
                s.meeting_microphone_device.clone(),
            );
    }
    ui.add_space(Space::MD);
    ui.add_enabled_ui(
        state.meeting_devices_dirty && state.meeting_devices_pending.is_none(),
        |ui| {
            if action(ui, "Save devices", ActionTone::Secondary).clicked() {
                state.save_meeting_devices(s, out);
            }
        },
    );
    if state.meeting_devices_pending.is_some() {
        muted(ui, "Saving audio devices…");
    } else if !s.meeting_devices_error.is_empty() {
        muted(
            ui,
            format!("Devices were not saved · {}", s.meeting_devices_error),
        );
    } else if state.meeting_devices_dirty {
        muted(ui, "Device changes are not saved yet.");
    } else if s.meeting_devices_revision > 0 {
        muted(ui, "Audio devices saved.");
    }
    muted(
        ui,
        "Headphones help avoid speaker audio reaching the microphone twice.",
    );
    ui.add_space(Space::LG);
    hairline(ui);
    ui.add_space(Space::LG);
    translation_settings(ui, s, state, out);
    ui.add_space(Space::LG);
    hairline(ui);
    ui.add_space(Space::LG);
    assistant(ui, s, state, out);
}

fn translation_settings(
    ui: &mut Ui,
    s: &WorkspaceSnapshot,
    state: &mut WorkspaceState,
    out: &mut Vec<ShellEvent>,
) {
    let current = (s.translation_language.clone(), s.translation_model.clone());
    if state.translation_seen.as_ref() != Some(&current) {
        state.translation_language = current.0.clone();
        state.translation_model = current.1.clone();
        state.translation_seen = Some(current);
    }
    ui.label(RichText::new("02 / TRANSLATION").size(10.0).strong().color(ui.tokens().secondary_text));
    section(ui, "Translation");
    muted(
        ui,
        "Local AI translates completed phrases. Originals are retained; translation never pauses recording.",
    );
    ui.add_space(Space::MD);
    muted(ui, "Target language · leave empty to turn off");
    ui.add(
        TextEdit::singleline(&mut state.translation_language)
            .hint_text("For example: Brazilian Portuguese")
            .desired_width(420.0),
    );
    muted(ui, "Local Ollama model");
    egui::ComboBox::from_id_salt("meeting-translation-model")
        .selected_text(if state.translation_model.is_empty() {
            "Choose a translation model"
        } else {
            &state.translation_model
        })
        .show_ui(ui, |ui| {
            for model in &s.available_models {
                ui.selectable_value(&mut state.translation_model, model.clone(), model);
            }
        });
    ui.add(
        TextEdit::singleline(&mut state.translation_model)
            .hint_text("Installed model name")
            .desired_width(420.0),
    );
    let language = state.translation_language.trim();
    let model = state.translation_model.trim();
    let valid =
        (language.is_empty() && model.is_empty()) || (!language.is_empty() && !model.is_empty());
    ui.add_enabled_ui(valid, |ui| {
        if action(ui, "Save translation", ActionTone::Quiet).clicked() {
            emit(
                out,
                WorkspaceEvent::SaveTranslation {
                    language: language.into(),
                    model: model.into(),
                },
            );
        }
    });
    if !valid {
        muted(
            ui,
            "Choose both a target language and an installed local model, or clear both to turn translation off.",
        );
    }
    if !s.translation_notice.is_empty() {
        muted(
            ui,
            format!("Translation unavailable · {}", s.translation_notice),
        );
    }
}

fn saved(
    ui: &mut Ui,
    s: &WorkspaceSnapshot,
    state: &mut WorkspaceState,
    out: &mut Vec<ShellEvent>,
) {
    ui.add_space(Space::MD);
    ui.horizontal_wrapped(|ui| {
        let response = ui.add(
            TextEdit::singleline(&mut state.query)
                .hint_text("Search a topic, a phrase, or a memory…")
                .desired_width(ui.available_width().min(610.0)),
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
    if !s.sessions.is_empty() {
        ui.label(
            RichText::new("RECENT SESSIONS")
                .size(10.0)
                .strong()
                .color(ui.tokens().secondary_text),
        );
        ui.add_space(Space::SM);
        hairline(ui);
    }
    for session in &s.sessions {
        ui.push_id(session.id, |ui| {
            ui.add_space(Space::MD);
            ui.horizontal(|ui| {
                ui.allocate_ui_with_layout(Vec2::new(108.0, 48.0), Layout::top_down(Align::Min), |ui| {
                    ui.label(
                        RichText::new(format!("SESSION {:03}", session.id))
                            .size(10.0)
                            .strong()
                            .color(ui.tokens().secondary_text),
                    );
                });
                ui.add_enabled_ui(s.capture == CaptureState::Idle, |ui| {
                    if ui
                        .add(
                            Button::new(RichText::new(&session.title).size(20.0))
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
                ui.label(RichText::new("↗").size(20.0).color(ui.tokens().accent));
            });
            hairline(ui);
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
    ui.label(RichText::new("03 / ASSISTANT").size(10.0).strong().color(ui.tokens().secondary_text));
    section(ui, "Assistant");
    muted(ui, "Choose the model that follows the discussion. Your questions take priority.");
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
        muted(ui, "Fallback model · optional");
        egui::ComboBox::from_id_salt("meeting-fallback-model")
            .width(ui.available_width().min(630.0))
            .selected_text(if state.provider.fallback_model.is_empty() {
                "No fallback"
            } else {
                &state.provider.fallback_model
            })
            .show_ui(ui, |ui| {
                ui.selectable_value(
                    &mut state.provider.fallback_model,
                    String::new(),
                    "No fallback",
                );
                for model in &s.available_models {
                    if *model != state.provider.model {
                        ui.selectable_value(
                            &mut state.provider.fallback_model,
                            model.clone(),
                            model,
                        );
                    }
                }
            });
        muted(
            ui,
            "If the local assistant fails before answering, Phorminx can try this installed model once.",
        );
        ui.add_space(Space::MD);
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
    ui.add_space(Space::LG);
    hairline(ui);
    ui.add_space(Space::LG);
    section(ui, "Automatic commentary");
    muted(
        ui,
        "A local Ollama model can comment when new conversation arrives. Your questions take priority.",
    );
    let label = if s.auto_commentary_enabled {
        "Turn off commentary"
    } else {
        "Turn on commentary"
    };
    if action(ui, label, ActionTone::Quiet).clicked() {
        emit(
            out,
            WorkspaceEvent::SetAutoCommentary(!s.auto_commentary_enabled),
        );
    }
    ui.horizontal_wrapped(|ui| {
        muted(ui, "Check for new context every");
        for interval in [10, 15, 30, 60] {
            let selected = s.auto_commentary_interval_secs == interval;
            if ui
                .selectable_label(selected, format!("{interval}s"))
                .clicked()
            {
                emit(out, WorkspaceEvent::SetAutoCommentaryInterval(interval));
            }
        }
    });
    if !s.auto_commentary_notice.is_empty() {
        muted(ui, &s.auto_commentary_notice);
    }
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
    ui.spacing_mut().item_spacing = Vec2::new(6.0, 3.0);
    ui.spacing_mut().button_padding = Vec2::new(7.0, 4.0);
    ui.spacing_mut().interact_size = Vec2::new(27.0, 27.0);
    ui.style_mut()
        .text_styles
        .insert(egui::TextStyle::Body, egui::FontId::proportional(14.0));
    Frame::new().fill(tokens.background).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.set_min_height(ui.available_height());
            companion_utility_bar(ui, s, state, out);
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
            companion_controls(ui, s, state, out);
            companion_content(ui, s, state, out);
        });
}

fn companion_utility_bar(
    ui: &mut Ui,
    s: &WorkspaceSnapshot,
    state: &mut WorkspaceState,
    out: &mut Vec<ShellEvent>,
) {
    let tokens = ui.tokens();
    let compact = ui.available_width() < 820.0;
    Frame::new()
        .fill(tokens.surface)
        .inner_margin(Margin::symmetric(16, 8))
        .show(ui, |ui| {
            if compact {
                ui.horizontal(|ui| {
                    companion_session_title(ui, s, (ui.available_width() - 110.0).max(150.0));
                    companion_status(ui, s);
                });
                ui.horizontal(|ui| {
                    if s.capture != CaptureState::Idle { source_controls(ui, s, out); }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        companion_window_actions(ui, s, state, out);
                    });
                });
            } else {
                let title_width = (ui.available_width() * 0.34).clamp(240.0, 310.0);
                ui.horizontal(|ui| {
                    companion_session_title(ui, s, title_width);
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        companion_window_actions(ui, s, state, out);
                        if s.capture != CaptureState::Idle { source_controls(ui, s, out); }
                        companion_status(ui, s);
                    });
                });
            }
        });
}

fn companion_session_title(ui: &mut Ui, s: &WorkspaceSnapshot, width: f32) {
    let tokens = ui.tokens();
    ui.allocate_ui_with_layout(
        Vec2::new(width, 36.0),
        Layout::top_down(Align::Min),
        |ui| {
            ui.label(RichText::new("MEETING").size(9.0).color(tokens.secondary_text));
            let title = if s.title.trim().is_empty() { "Live meeting" } else { &s.title };
            ui.add(egui::Label::new(RichText::new(title).size(14.0).strong()).truncate());
        },
    );
}

fn companion_status(ui: &mut Ui, s: &WorkspaceSnapshot) {
    let tokens = ui.tokens();
    let color = if matches!(s.capture, CaptureState::Starting | CaptureState::Listening) {
        tokens.verified
    } else {
        tokens.secondary_text
    };
    ui.label(RichText::new(format!(
        "●  {}   {}",
        capture_label(s.capture).to_uppercase(),
        timestamp(s.captured_samples)
    )).size(10.0).color(color));
}

fn companion_window_actions(
    ui: &mut Ui,
    s: &WorkspaceSnapshot,
    state: &mut WorkspaceState,
    out: &mut Vec<ShellEvent>,
) {
    let tokens = ui.tokens();
    let active = matches!(s.capture, CaptureState::Starting | CaptureState::Listening);
    if ui.add_enabled(active, Button::new(RichText::new("Finish").color(tokens.background)).fill(tokens.text).stroke(Stroke::NONE))
        .on_hover_text("Finish recording")
        .clicked()
    {
        emit(out, WorkspaceEvent::Stop);
    }
    if ui.add(Button::new("Hide").fill(tokens.background).stroke(Stroke::new(1.0, tokens.edge)))
        .on_hover_text("Hide companion · listening continues")
        .clicked()
    {
        emit(out, WorkspaceEvent::HideCompanion);
    }
    let picker = ui.add(Button::new("⋯").fill(tokens.background).stroke(Stroke::new(1.0, tokens.edge)));
    picker.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "Open action picker"));
    if picker.on_hover_text("Open action picker").clicked() {
        state.companion_picker = !state.companion_picker;
    }
}

fn companion_controls(
    ui: &mut Ui,
    s: &WorkspaceSnapshot,
    state: &mut WorkspaceState,
    out: &mut Vec<ShellEvent>,
) {
    let before_layout = (state.companion_view, state.translation_view);
    let tokens = ui.tokens();
    Frame::new()
        .fill(tokens.background)
        .inner_margin(Margin::symmetric(16, 5))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                let compact = ui.available_width() < 710.0;
                for (view, label) in [
                    (CompanionView::Transcript, "Transcript"),
                    (CompanionView::Split, "Together"),
                    (CompanionView::Assistant, "Assistant"),
                ] {
                    let selected = if compact && state.companion_view == CompanionView::Split {
                        view == CompanionView::Transcript
                    } else {
                        state.companion_view == view
                    };
                    let response = ui.add_enabled(
                        !compact || view != CompanionView::Split,
                        Button::new(RichText::new(label).size(11.0).color(if selected { tokens.text } else { tokens.secondary_text }))
                            .frame(false)
                            .min_size(Vec2::new(0.0, 28.0)),
                    ).on_hover_text(if compact && view == CompanionView::Split { "Widen the window for both panes" } else { "Change room view" });
                    if response.clicked() { state.companion_view = view; }
                    if selected {
                        ui.painter().line_segment(
                            [response.rect.left_bottom(), response.rect.right_bottom()],
                            Stroke::new(2.0, tokens.accent),
                        );
                    }
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let effective = if compact && state.companion_view == CompanionView::Split {
                        CompanionView::Transcript
                    } else { state.companion_view };
                    if effective != CompanionView::Transcript {
                        commentary_toggle(ui, s, out);
                    }
                    if effective != CompanionView::Assistant {
                        translation_controls(ui, s, state);
                    }
                });
            });
        });
    hairline(ui);
    if before_layout != (state.companion_view, state.translation_view) {
        state.save_companion_layout(out);
    }
}

fn source_controls(ui: &mut Ui, s: &WorkspaceSnapshot, out: &mut Vec<ShellEvent>) {
    let tokens = ui.tokens();
    let mic_label = if s.microphone_muted { "□ Mic paused" } else { "▪ Mic on" };
    if ui.add(Button::new(mic_label).small().fill(tokens.background).stroke(Stroke::new(1.0, tokens.edge)))
        .on_hover_text(if s.microphone_muted { "Resume my transcription" } else { "Pause my transcription" })
        .clicked()
    {
        emit(out, WorkspaceEvent::SetMeetingSourceMuted { source: CaptureSource::Microphone, muted: !s.microphone_muted });
    }
    let computer_label = if s.computer_audio_muted { "□ Computer paused" } else { "▪ Computer on" };
    if ui.add(Button::new(computer_label).small().fill(tokens.background).stroke(Stroke::new(1.0, tokens.edge)))
        .on_hover_text(if s.computer_audio_muted { "Resume computer transcription" } else { "Pause computer transcription" })
        .clicked()
    {
        emit(out, WorkspaceEvent::SetMeetingSourceMuted { source: CaptureSource::SystemAudio, muted: !s.computer_audio_muted });
    }
}

fn translation_controls(ui: &mut Ui, s: &WorkspaceSnapshot, state: &mut WorkspaceState) {
    let tokens = ui.tokens();
    let target = if s.translation_language.eq_ignore_ascii_case("Brazilian Portuguese")
        || s.translation_language.eq_ignore_ascii_case("pt-BR")
    { "Português" } else if s.translation_language.len() <= 13 && !s.translation_language.is_empty() {
        s.translation_language.as_str()
    } else { "Translation" };
    for (view, label) in [
        (TranslationView::Translation, target),
        (TranslationView::Both, "Both"),
        (TranslationView::Original, "Original"),
    ] {
        let selected = state.translation_view == view;
        if ui.add(Button::new(RichText::new(label).size(10.0).color(if selected { tokens.background } else { tokens.secondary_text }))
            .fill(if selected { tokens.text } else { tokens.surface })
            .stroke(Stroke::new(1.0, tokens.edge)))
            .on_hover_text(if view == TranslationView::Translation && !s.translation_language.is_empty() {
                s.translation_language.as_str()
            } else { label })
            .clicked()
        {
            state.translation_view = view;
        }
    }
}

fn commentary_toggle(ui: &mut Ui, s: &WorkspaceSnapshot, out: &mut Vec<ShellEvent>) {
    let tokens = ui.tokens();
    let (rect, response) = ui.allocate_exact_size(Vec2::new(31.0, 18.0), Sense::click());
    let fill = if s.auto_commentary_enabled { tokens.verified } else { tokens.edge };
    ui.painter().rect_filled(rect, 9.0, fill);
    let center_x = if s.auto_commentary_enabled { rect.right() - 9.0 } else { rect.left() + 9.0 };
    ui.painter().circle_filled(egui::pos2(center_x, rect.center().y), 6.0, tokens.background);
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "Toggle commentary"));
    if response.on_hover_text("Commentary on new conversation; your questions take priority").clicked() {
        emit(out, WorkspaceEvent::SetAutoCommentary(!s.auto_commentary_enabled));
    }
    ui.label(RichText::new("Commentary").size(10.0).color(tokens.secondary_text));
}

fn companion_content(
    ui: &mut Ui,
    s: &WorkspaceSnapshot,
    state: &mut WorkspaceState,
    out: &mut Vec<ShellEvent>,
) {
    let height = ui.available_height().max(80.0);
    let width = ui.available_width();
    let origin = ui.next_widget_position();
    let mut layout_drag_stopped = false;
    let effective_view = if width < 710.0 && state.companion_view == CompanionView::Split {
        CompanionView::Transcript
    } else {
        state.companion_view
    };
    match effective_view {
        CompanionView::Transcript => transcript_panel(ui, s, state, out, height),
        CompanionView::Assistant => assistant_panel(ui, s, state, out, height),
        CompanionView::Split if width >= 710.0 => {
            let fraction = if state.split_fraction == 0.0 {
                0.57
            } else {
                state.split_fraction.clamp(0.28, 0.72)
            };
            ui.horizontal(|ui| {
                let left = (width * fraction - 8.0).max(180.0);
                ui.allocate_ui_with_layout(
                    Vec2::new(left, height),
                    Layout::top_down(Align::Min),
                    |ui| transcript_panel(ui, s, state, out, height),
                );
                let (rect, drag) = ui.allocate_exact_size(Vec2::new(12.0, height), Sense::drag());
                ui.painter().line_segment(
                    [rect.center_top(), rect.center_bottom()],
                    Stroke::new(1.0, ui.tokens().edge),
                );
                if drag.dragged()
                    && let Some(pointer) = ui.input(|input| input.pointer.interact_pos())
                {
                    state.split_fraction = ((pointer.x - origin.x) / width).clamp(0.28, 0.72);
                }
                if drag.double_clicked() { state.split_fraction = 0.57; layout_drag_stopped = true; }
                layout_drag_stopped |= drag.drag_stopped();
                drag.on_hover_cursor(CursorIcon::ResizeHorizontal).on_hover_text("Drag this divider to resize; double-click to balance");
                ui.allocate_ui_with_layout(
                    Vec2::new((width - left - 20.0).max(180.0), height),
                    Layout::top_down(Align::Min),
                    |ui| assistant_panel(ui, s, state, out, height),
                );
            });
        }
        CompanionView::Split => unreachable!("wide split handled above"),
    }
    if layout_drag_stopped {
        state.save_companion_layout(out);
    }
}

fn transcript_panel(
    ui: &mut Ui,
    s: &WorkspaceSnapshot,
    state: &mut WorkspaceState,
    _out: &mut Vec<ShellEvent>,
    height: f32,
) {
    let tokens = ui.tokens();
    Frame::new()
        .fill(tokens.background)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.set_min_height(height.max(80.0));
            let view = state.translation_view;
            let notice = if view == TranslationView::Original {
                None
            } else if !s.translation_notice.is_empty() {
                Some(s.translation_notice.as_str())
            } else if s.translation_language.is_empty() {
                Some("Translation is off · set a language and model in Meeting settings")
            } else { None };
            if let Some(notice) = notice {
                Frame::new().fill(tokens.surface).inner_margin(Margin::symmetric(16, 7)).show(ui, |ui| {
                    ui.label(RichText::new(notice).size(11.0).color(tokens.secondary_text));
                });
                hairline(ui);
            }
            ScrollArea::vertical()
                .id_salt("companion-transcript")
                .max_height((height - if notice.is_some() { 34.0 } else { 0.0 }).max(24.0))
                .auto_shrink([false, false])
                .stick_to_bottom(s.capture != CaptureState::Idle)
                .show(ui, |ui| {
                    if view != TranslationView::Translation {
                        for line in &s.transcript {
                            phrase_row(ui, s, line, view == TranslationView::Both);
                        }
                        if !s.provisional_text.is_empty() {
                            Frame::new().inner_margin(Margin::symmetric(18, 12)).show(ui, |ui| {
                                ui.label(RichText::new(&s.provisional_text).italics().color(tokens.secondary_text));
                            });
                        }
                    } else {
                        for line in &s.translations {
                            Frame::new().inner_margin(Margin::symmetric(18, 14)).show(ui, |ui| {
                                muted(ui, format!("{}  /  {}", timestamp(line.start_sample), line.language));
                                ui.label(RichText::new(&line.text).size(13.0).color(tokens.text));
                            });
                            hairline(ui);
                        }
                    }
                    if s.transcript.is_empty()
                        && s.provisional_text.is_empty()
                        && view != TranslationView::Translation
                    {
                        muted(
                            ui,
                            if s.capture == CaptureState::Idle {
                                "No transcript yet."
                            } else {
                                "Listening…"
                            },
                        );
                    }
                    if view == TranslationView::Translation && s.translations.is_empty() {
                        muted(
                            ui,
                            if s.translation_language.is_empty() {
                                "Set a language and local model in Meeting settings."
                            } else {
                                "Waiting for translated phrases…"
                            },
                        );
                    }
                });
        });
}

fn phrase_row(ui: &mut Ui, s: &WorkspaceSnapshot, line: &MeetingLine, with_translation: bool) {
    let tokens = ui.tokens();
    let (time, source, text) = if inline_source_label(&line.text) {
        let (prefix, content) = line.text.split_once("] ").expect("validated source label");
        if let Some((time, source)) = prefix.strip_prefix('[').and_then(|p| p.split_once(" · ")) {
            (time.to_owned(), source, content)
        } else {
            (timestamp(line.start_sample), "CONVERSATION", line.text.as_str())
        }
    } else {
        (timestamp(line.start_sample), "CONVERSATION", line.text.as_str())
    };
    Frame::new().inner_margin(Margin::symmetric(18, 14)).show(ui, |ui| {
        if ui.available_width() >= 300.0 {
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.set_width(85.0);
                    muted(ui, &time);
                    ui.label(RichText::new(source.to_uppercase()).size(9.0).strong().color(tokens.secondary_text));
                });
                ui.vertical(|ui| {
                    ui.set_width(ui.available_width());
                    ui.label(RichText::new(text).size(13.0));
                    if with_translation { phrase_translation(ui, s, line); }
                });
            });
        } else {
            muted(ui, format!("{}  /  {}", time, source.to_uppercase()));
            ui.label(RichText::new(text).size(13.0));
            if with_translation { phrase_translation(ui, s, line); }
        }
    });
    hairline(ui);
}

fn inline_source_label(text: &str) -> bool {
    let Some((prefix, _)) = text.split_once("] ") else {
        return false;
    };
    prefix.starts_with('[')
        && (prefix.ends_with(" · Computer audio") || prefix.ends_with(" · You / microphone"))
}

fn phrase_translation(ui: &mut Ui, s: &WorkspaceSnapshot, line: &MeetingLine) {
    for translation in s.translations.iter().filter(|translation| {
        translation.start_sample == line.start_sample && translation.end_sample == line.end_sample
    }) {
        ui.add_space(Space::XS);
        hairline(ui);
        ui.add_space(Space::XS);
        ui.label(RichText::new(&translation.text).size(13.0).color(ui.tokens().accent));
    }
}

fn assistant_panel(
    ui: &mut Ui,
    s: &WorkspaceSnapshot,
    state: &mut WorkspaceState,
    out: &mut Vec<ShellEvent>,
    height: f32,
) {
    Frame::new()
        .fill(ui.tokens().surface)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.set_min_height(height.max(80.0));
            egui::Panel::bottom(ui.id().with("assistant-composer"))
                .exact_size(if answer_retry_available(s) { 94.0 } else { 58.0 })
                .show_separator_line(true)
                .frame(Frame::new().fill(ui.tokens().background).inner_margin(Margin::symmetric(16, 8)))
                .show(ui, |ui| companion_composer(ui, s, state, out));
            ui.set_clip_rect(ui.clip_rect().intersect(ui.available_rect_before_wrap()));
            if !s.notice.is_empty() {
                muted(ui, &s.notice);
            }
            if !s.auto_commentary_notice.is_empty() {
                muted(ui, &s.auto_commentary_notice);
            }
            conversation_messages(ui, s, state, out, ui.available_height(), true);
        });
}

fn companion_composer(
    ui: &mut Ui,
    s: &WorkspaceSnapshot,
    state: &mut WorkspaceState,
    out: &mut Vec<ShellEvent>,
) {
    let tokens = ui.tokens();
    let edit_id = ui.make_persistent_id("meeting-chat-composer");
    let enter = composer_enter(ui, edit_id);
    let can_send = !s.send_pending && state.chat_pending.is_none() && !state.chat.trim().is_empty();
    let width = ui.available_width();
    let mut clicked = false;
    ui.horizontal(|ui| {
        let input_width = (width - 43.0).max(100.0);
        if state.chat.contains('\n') || state.chat.len() > 100 {
            ui.allocate_ui_with_layout(
                Vec2::new(input_width, 35.0),
                Layout::top_down(Align::Min),
                |ui| {
                    ScrollArea::vertical()
                        .id_salt("companion-composer-scroll")
                        .max_height(35.0)
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            ui.add_sized(
                                [ui.available_width(), 35.0],
                                TextEdit::multiline(&mut state.chat)
                                    .id(edit_id)
                                    .frame(Frame::new().fill(tokens.surface).stroke(Stroke::new(1.0, tokens.edge)))
                                    .hint_text("Ask about this meeting…")
                                    .return_key(egui::KeyboardShortcut::new(egui::Modifiers::SHIFT, egui::Key::Enter))
                                    .desired_rows(1)
                                    .char_limit(32000),
                            );
                        });
                },
            );
        } else {
            ui.add_sized(
                [input_width, 35.0],
                TextEdit::multiline(&mut state.chat)
                    .id(edit_id)
                    .frame(Frame::new().fill(tokens.surface).stroke(Stroke::new(1.0, tokens.edge)))
                    .hint_text("Ask about this meeting…")
                    .return_key(egui::KeyboardShortcut::new(egui::Modifiers::SHIFT, egui::Key::Enter))
                    .desired_rows(1)
                    .char_limit(32000),
            );
        }
        let response = ui.add_enabled(
            can_send,
            Button::new(RichText::new("↗").color(tokens.accent))
                .fill(tokens.surface)
                .stroke(Stroke::new(1.0, tokens.edge))
                .min_size(Vec2::new(35.0, 35.0)),
        );
        response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "Send message"));
        clicked = response.on_hover_text("Send message · Enter").clicked();
    });
    if (clicked || enter) && can_send {
        state.send_chat(s, out);
    }
    if answer_retry_available(s) && ui.button("Retry answer").clicked() {
        emit(out, WorkspaceEvent::RetryAnswer);
    }
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
    #[test]
    fn dual_source_labels_already_include_the_original_timestamp() {
        assert!(inline_source_label(
            "[12:00.000 · Computer audio] A client question"
        ));
        assert!(inline_source_label(
            "[12:01.120 · You / microphone] My answer"
        ));
        assert!(!inline_source_label("[Reminder] Bring the design file"));
        assert!(!inline_source_label("A plain single-source transcript"));
    }
}
