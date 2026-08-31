use eframe::egui::{
    self, Align, Button, ComboBox, Layout, RichText, ScrollArea, Stroke, TextEdit, Ui, Vec2,
};

use crate::components::{
    self, ActionTone, action, empty_state, hairline, metadata, page_header, readiness_row,
    section_title, segmented,
};
use crate::model::{
    FormattingStrength, HistoryVariant, LexiconCasePolicy, LexiconDraft, OllamaLifecycle,
    ProfileDraft, ProfileInsertion, RecordingMode, Route, SettingsSnapshot, ShellEvent,
    ShellSnapshot,
};
use crate::theme::{Space, UiThemeExt};

#[derive(Clone, Debug)]
pub(crate) struct PageState {
    pub history_id: Option<i64>,
    pub history_variant: HistoryVariant,
    pub confirm_clear_history: bool,
    pub lexicon_id: Option<i64>,
    pub lexicon_draft: Option<LexiconDraft>,
    pub confirm_lexicon_delete: Option<i64>,
    pub profile_name: Option<String>,
    pub profile_draft: Option<ProfileDraft>,
    pub confirm_profile_delete: Option<String>,
    pub settings: SettingsSnapshot,
    pub settings_dirty: bool,
    pub confirm_model_download: bool,
}

impl PageState {
    pub fn from_snapshot(snapshot: &ShellSnapshot) -> Self {
        Self {
            history_id: snapshot.history.first().map(|item| item.id),
            history_variant: HistoryVariant::Output,
            confirm_clear_history: false,
            lexicon_id: snapshot.lexicon.first().map(|item| item.id),
            lexicon_draft: None,
            confirm_lexicon_delete: None,
            profile_name: snapshot
                .profiles
                .first()
                .map(|item| item.executable.clone()),
            profile_draft: None,
            confirm_profile_delete: None,
            settings: snapshot.settings.clone(),
            settings_dirty: false,
            confirm_model_download: false,
        }
    }

    pub fn reconcile(&mut self, snapshot: &ShellSnapshot) {
        if self
            .history_id
            .is_some_and(|id| !snapshot.history.iter().any(|item| item.id == id))
        {
            self.history_id = snapshot.history.first().map(|item| item.id);
        }
        if self
            .lexicon_id
            .is_some_and(|id| !snapshot.lexicon.iter().any(|item| item.id == id))
        {
            self.lexicon_id = snapshot.lexicon.first().map(|item| item.id);
        }
        if self.profile_name.as_ref().is_some_and(|name| {
            !snapshot
                .profiles
                .iter()
                .any(|profile| &profile.executable == name)
        }) {
            self.profile_name = snapshot
                .profiles
                .first()
                .map(|item| item.executable.clone());
        }
        if self.settings_dirty && self.settings == snapshot.settings {
            self.settings_dirty = false;
        }
        if !self.settings_dirty {
            self.settings = snapshot.settings.clone();
        }
    }
}

pub(crate) fn show(
    ui: &mut Ui,
    route: Route,
    snapshot: &ShellSnapshot,
    state: &mut PageState,
    outbox: &mut Vec<ShellEvent>,
) {
    match route {
        Route::Home => home(ui, snapshot, outbox),
        Route::History => history(ui, snapshot, state, outbox),
        Route::Lexicon => lexicon(ui, snapshot, state, outbox),
        Route::Profiles => profiles(ui, snapshot, state, outbox),
        Route::Models => models(ui, snapshot, state, outbox),
        Route::Settings => settings(ui, snapshot, state, outbox),
    }
}

fn home(ui: &mut Ui, snapshot: &ShellSnapshot, outbox: &mut Vec<ShellEvent>) {
    let tokens = ui.tokens();
    page_header(ui, Route::Home.title(), Route::Home.context(), None);
    ui.add_space(Space::LG);
    ui.horizontal(|ui| {
        ui.vertical(|ui| {
            metadata(ui, "Instrument state");
            ui.add_space(Space::SM);
            ui.label(
                RichText::new(snapshot.status.label())
                    .size(56.0)
                    .color(tokens.text),
            );
            ui.add_space(Space::XS);
            components::shortcut_chord(ui, &snapshot.shortcut);
            ui.add_space(Space::XL);
            if action(ui, "Test dictation", ActionTone::Primary).clicked() {
                outbox.push(ShellEvent::TestDictation);
            }
        });
        ui.with_layout(Layout::right_to_left(Align::TOP), |ui| {
            ui.set_width(390.0);
            ui.vertical(|ui| {
                metadata(ui, "Local systems");
                ui.add_space(Space::SM);
                for system in &snapshot.systems {
                    readiness_row(ui, &system.name, &system.detail, system.state);
                    hairline(ui);
                }
            });
        });
    });
    ui.add_space(Space::XXL);
    metadata(ui, "Recently held");
    ui.add_space(Space::SM);
    if !snapshot.history_enabled {
        ui.label(
            RichText::new("History is off. Nothing spoken is retained.")
                .color(tokens.secondary_text),
        );
    } else if snapshot.history.is_empty() {
        ui.label(RichText::new("No dictations held yet.").color(tokens.secondary_text));
    } else {
        for item in snapshot.history.iter().take(3) {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(&item.time)
                        .monospace()
                        .color(tokens.secondary_text),
                );
                if ui
                    .add(
                        Button::new(RichText::new(&item.output).color(tokens.text))
                            .fill(egui::Color32::TRANSPARENT)
                            .stroke(Stroke::NONE),
                    )
                    .clicked()
                {
                    outbox.push(ShellEvent::SelectHistory(item.id));
                    outbox.push(ShellEvent::Navigate(Route::History));
                }
            });
            hairline(ui);
        }
    }
}

fn history(
    ui: &mut Ui,
    snapshot: &ShellSnapshot,
    state: &mut PageState,
    outbox: &mut Vec<ShellEvent>,
) {
    let tokens = ui.tokens();
    page_header(ui, Route::History.title(), Route::History.context(), None);
    if !snapshot.history.is_empty() {
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let label = if state.confirm_clear_history {
                "Delete every retained dictation"
            } else {
                "Clear history"
            };
            if action(ui, label, ActionTone::Destructive).clicked() {
                if state.confirm_clear_history {
                    outbox.push(ShellEvent::ClearHistory);
                    state.confirm_clear_history = false;
                } else {
                    state.confirm_clear_history = true;
                }
            }
            if state.confirm_clear_history {
                ui.label(
                    RichText::new("This permanently removes all retained transcripts.")
                        .color(tokens.secondary_text),
                );
            }
        });
        ui.add_space(Space::MD);
    }
    if snapshot.history.is_empty() {
        empty_state(
            ui,
            "Nothing held",
            if snapshot.history_enabled {
                "Completed dictations will gather here."
            } else {
                "History is off. Nothing spoken is retained."
            },
            None,
        );
        return;
    }

    ui.columns(2, |columns| {
        columns[0].set_width(300.0);
        metadata(&mut columns[0], "Chronology");
        columns[0].add_space(Space::XS);
        ScrollArea::vertical()
            .id_salt("history-list")
            .show(&mut columns[0], |ui| {
                for item in &snapshot.history {
                    let selected = state.history_id == Some(item.id);
                    let response = ui.add(
                        Button::new(
                            RichText::new(format!(
                                "{}  {}\n{}",
                                item.time, item.application, item.output
                            ))
                            .color(if selected {
                                tokens.text
                            } else {
                                tokens.secondary_text
                            }),
                        )
                        .selected(selected)
                        .fill(if selected {
                            tokens.raised
                        } else {
                            tokens.surface
                        })
                        .stroke(Stroke::NONE)
                        .min_size(Vec2::new(ui.available_width(), 68.0)),
                    );
                    if response.clicked() {
                        state.history_id = Some(item.id);
                        state.history_variant = HistoryVariant::Output;
                        outbox.push(ShellEvent::SelectHistory(item.id));
                    }
                    hairline(ui);
                }
            });

        columns[1].add_space(Space::XXS);
        if let Some(item) = state
            .history_id
            .and_then(|id| snapshot.history.iter().find(|entry| entry.id == id))
        {
            let available = HistoryVariant::ALL
                .into_iter()
                .filter(|variant| item.text_for(*variant).is_some());
            if let Some(variant) = segmented(
                &mut columns[1],
                available,
                &mut state.history_variant,
                HistoryVariant::label,
            ) {
                outbox.push(ShellEvent::SelectHistoryVariant(variant));
            }
            columns[1].add_space(Space::LG);
            let text = item.text_for(state.history_variant).unwrap_or(&item.output);
            columns[1].label(
                RichText::new(text)
                    .size(21.0)
                    .line_height(Some(29.0))
                    .color(tokens.text),
            );
            columns[1].add_space(Space::XL);
            hairline(&mut columns[1]);
            columns[1].add_space(Space::MD);
            columns[1].horizontal(|ui| {
                if action(ui, "Copy output", ActionTone::Primary).clicked() {
                    outbox.push(ShellEvent::CopyHistory {
                        id: item.id,
                        variant: HistoryVariant::Output,
                    });
                }
                if item.raw.is_some() && action(ui, "Copy raw", ActionTone::Quiet).clicked() {
                    outbox.push(ShellEvent::CopyHistory {
                        id: item.id,
                        variant: HistoryVariant::Raw,
                    });
                }
            });
            columns[1].add_space(Space::XL);
            metadata(&mut columns[1], "Provenance");
            columns[1].label(
                RichText::new(format!(
                    "{} · {} · {}",
                    item.application, item.language, item.latency
                ))
                .monospace()
                .color(tokens.secondary_text),
            );
            if let Some(warning) = &item.warning {
                columns[1].label(RichText::new(warning).color(tokens.accent_focus));
            }
        }
    });
}

fn lexicon(
    ui: &mut Ui,
    snapshot: &ShellSnapshot,
    state: &mut PageState,
    outbox: &mut Vec<ShellEvent>,
) {
    let tokens = ui.tokens();
    if page_header(
        ui,
        Route::Lexicon.title(),
        Route::Lexicon.context(),
        Some("New entry"),
    ) {
        state.lexicon_draft = Some(LexiconDraft::default());
        outbox.push(ShellEvent::NewLexiconEntry);
    }
    if snapshot.lexicon.is_empty() {
        if let Some(draft) = state.lexicon_draft.as_mut() {
            lexicon_editor(ui, draft, outbox);
        } else if empty_state(
            ui,
            "No private vocabulary",
            "Teach exact replacements without training a model.",
            Some("New entry"),
        ) {
            state.lexicon_draft = Some(LexiconDraft::default());
            outbox.push(ShellEvent::NewLexiconEntry);
        }
        return;
    }
    ui.columns(2, |columns| {
        columns[0].set_width(510.0);
        metadata(&mut columns[0], "Spoken  →  Written");
        columns[0].add_space(Space::XS);
        for entry in &snapshot.lexicon {
            let selected = state.lexicon_id == Some(entry.id);
            let response = columns[0].add(
                Button::new(
                    RichText::new(format!("{}  →  {}", entry.spoken, entry.written)).color(
                        if selected {
                            tokens.text
                        } else {
                            tokens.secondary_text
                        },
                    ),
                )
                .selected(selected)
                .fill(if selected {
                    tokens.raised
                } else {
                    tokens.surface
                })
                .stroke(Stroke::NONE)
                .min_size(Vec2::new(columns[0].available_width(), 44.0)),
            );
            if response.clicked() {
                state.lexicon_id = Some(entry.id);
                state.lexicon_draft = None;
                state.confirm_lexicon_delete = None;
            }
            hairline(&mut columns[0]);
        }
        if let Some(draft) = state.lexicon_draft.as_mut() {
            lexicon_editor(&mut columns[1], draft, outbox);
        } else if let Some(entry) = state
            .lexicon_id
            .and_then(|id| snapshot.lexicon.iter().find(|entry| entry.id == id))
        {
            metadata(&mut columns[1], "Entry");
            columns[1].add_space(Space::MD);
            labeled_value(&mut columns[1], "Spoken alias", &entry.spoken, false);
            labeled_value(&mut columns[1], "Written form", &entry.written, false);
            labeled_value(&mut columns[1], "Language", &entry.language, false);
            labeled_value(&mut columns[1], "Scope", &entry.scope, true);
            labeled_value(&mut columns[1], "Case", entry.case_policy.label(), false);
            columns[1].add_space(Space::MD);
            columns[1].label(
                RichText::new(format!(
                    "Preview: I said “{}”; Phorminx kept “{}”.",
                    entry.spoken, entry.written
                ))
                .color(tokens.secondary_text),
            );
            columns[1].add_space(Space::LG);
            columns[1].horizontal(|ui| {
                if action(ui, "Edit", ActionTone::Primary).clicked() {
                    state.lexicon_draft = Some(LexiconDraft {
                        id: Some(entry.id),
                        spoken: entry.spoken.clone(),
                        written: entry.written.clone(),
                        language: if entry.language != "Every language" {
                            entry.language.clone()
                        } else {
                            String::new()
                        },
                        scope: if entry.scope != "Everywhere" {
                            entry.scope.clone()
                        } else {
                            String::new()
                        },
                        case_policy: entry.case_policy,
                        enabled: entry.enabled,
                    });
                    outbox.push(ShellEvent::EditLexicon(entry.id));
                }
                let delete_label = if state.confirm_lexicon_delete == Some(entry.id) {
                    "Confirm delete"
                } else {
                    "Delete"
                };
                if action(ui, delete_label, ActionTone::Destructive).clicked() {
                    if state.confirm_lexicon_delete == Some(entry.id) {
                        outbox.push(ShellEvent::DeleteLexicon(entry.id));
                        state.confirm_lexicon_delete = None;
                    } else {
                        state.confirm_lexicon_delete = Some(entry.id);
                    }
                }
            });
        }
    });
}

fn profiles(
    ui: &mut Ui,
    snapshot: &ShellSnapshot,
    state: &mut PageState,
    outbox: &mut Vec<ShellEvent>,
) {
    let tokens = ui.tokens();
    if page_header(
        ui,
        Route::Profiles.title(),
        Route::Profiles.context(),
        Some("New profile"),
    ) {
        state.profile_draft = Some(ProfileDraft::default());
        outbox.push(ShellEvent::NewProfile);
    }
    if snapshot.profiles.is_empty() {
        if let Some(draft) = state.profile_draft.as_mut() {
            profile_editor(ui, draft, outbox);
        } else if empty_state(
            ui,
            "No contextual policy",
            "Global settings apply everywhere that dictation is allowed.",
            Some("New profile"),
        ) {
            state.profile_draft = Some(ProfileDraft::default());
            outbox.push(ShellEvent::NewProfile);
        }
        return;
    }
    ui.columns(2, |columns| {
        columns[0].set_width(300.0);
        metadata(&mut columns[0], "Applications");
        columns[0].add_space(Space::XS);
        for profile in &snapshot.profiles {
            let selected = state.profile_name.as_deref() == Some(&profile.executable);
            if columns[0]
                .add(
                    Button::new(RichText::new(&profile.executable).monospace().color(
                        if selected {
                            tokens.text
                        } else {
                            tokens.secondary_text
                        },
                    ))
                    .selected(selected)
                    .fill(if selected {
                        tokens.raised
                    } else {
                        tokens.surface
                    })
                    .stroke(Stroke::NONE)
                    .min_size(Vec2::new(columns[0].available_width(), 44.0)),
                )
                .clicked()
            {
                state.profile_name = Some(profile.executable.clone());
                state.profile_draft = None;
                state.confirm_profile_delete = None;
            }
            hairline(&mut columns[0]);
        }
        if let Some(draft) = state.profile_draft.as_mut() {
            profile_editor(&mut columns[1], draft, outbox);
        } else if let Some(profile) = state.profile_name.as_ref().and_then(|name| {
            snapshot
                .profiles
                .iter()
                .find(|profile| &profile.executable == name)
        }) {
            metadata(&mut columns[1], "Policy");
            columns[1].add_space(Space::SM);
            columns[1].label(
                RichText::new(profile.summary())
                    .size(19.0)
                    .color(tokens.text),
            );
            columns[1].add_space(Space::XL);
            section_title(
                &mut columns[1],
                "01",
                "Formatting",
                &format!(
                    "{} preserves this application’s register.",
                    profile.formatting
                ),
            );
            columns[1].add_space(Space::LG);
            section_title(
                &mut columns[1],
                "02",
                "Recognition",
                &format!("{} is preferred here.", profile.language),
            );
            columns[1].add_space(Space::LG);
            section_title(&mut columns[1], "03", "Insertion", &profile.insertion);
            if profile.blocked {
                columns[1].add_space(Space::XL);
                columns[1].label(
                    RichText::new("Dictation is blocked in this application.")
                        .color(tokens.destructive),
                );
            }
            columns[1].add_space(Space::XL);
            columns[1].horizontal(|ui| {
                if action(ui, "Edit policy", ActionTone::Primary).clicked() {
                    state.profile_draft = Some(ProfileDraft {
                        original_executable: Some(profile.executable.clone()),
                        executable: profile.executable.clone(),
                        formatting: parse_formatting(&profile.formatting),
                        custom_instruction: profile.custom_instruction.clone(),
                        language: if profile.language != "Default language" {
                            profile.language.clone()
                        } else {
                            String::new()
                        },
                        insertion: parse_insertion(&profile.insertion),
                        blocked: profile.blocked,
                    });
                    outbox.push(ShellEvent::EditProfile(profile.executable.clone()));
                }
                let delete_label =
                    if state.confirm_profile_delete.as_deref() == Some(&profile.executable) {
                        "Confirm remove"
                    } else {
                        "Remove"
                    };
                if action(ui, delete_label, ActionTone::Destructive).clicked() {
                    if state.confirm_profile_delete.as_deref() == Some(&profile.executable) {
                        outbox.push(ShellEvent::RemoveProfile(profile.executable.clone()));
                        state.confirm_profile_delete = None;
                    } else {
                        state.confirm_profile_delete = Some(profile.executable.clone());
                    }
                }
            });
        }
    });
}

fn lexicon_editor(ui: &mut Ui, draft: &mut LexiconDraft, outbox: &mut Vec<ShellEvent>) {
    let tokens = ui.tokens();
    metadata(
        ui,
        if draft.id.is_some() {
            "Edit entry"
        } else {
            "New entry"
        },
    );
    ui.add_space(Space::SM);
    ui.label(RichText::new("Spoken alias").color(tokens.secondary_text));
    ui.add(TextEdit::singleline(&mut draft.spoken).desired_width(f32::INFINITY));
    ui.label(RichText::new("Written form").color(tokens.secondary_text));
    ui.add(TextEdit::singleline(&mut draft.written).desired_width(f32::INFINITY));
    ui.label(RichText::new("Language tag · optional").color(tokens.secondary_text));
    ui.add(TextEdit::singleline(&mut draft.language).hint_text("en or pt-br"));
    ui.label(RichText::new("Application basename · optional").color(tokens.secondary_text));
    ui.add(TextEdit::singleline(&mut draft.scope).hint_text("code.exe"));
    ui.label(RichText::new("Case policy").color(tokens.secondary_text));
    ComboBox::from_id_salt("lexicon-case-policy")
        .selected_text(draft.case_policy.label())
        .show_ui(ui, |ui| {
            for value in [
                LexiconCasePolicy::PreserveInput,
                LexiconCasePolicy::UseCanonical,
                LexiconCasePolicy::Lowercase,
                LexiconCasePolicy::Uppercase,
            ] {
                ui.selectable_value(&mut draft.case_policy, value, value.label());
            }
        });
    ui.checkbox(&mut draft.enabled, "Enabled");
    ui.add_space(Space::LG);
    ui.horizontal(|ui| {
        if action(ui, "Save entry", ActionTone::Primary).clicked() {
            outbox.push(ShellEvent::SaveLexicon(draft.clone()));
        }
        if action(ui, "Cancel", ActionTone::Quiet).clicked() {
            outbox.push(ShellEvent::CancelLexiconEdit);
        }
    });
}

fn profile_editor(ui: &mut Ui, draft: &mut ProfileDraft, outbox: &mut Vec<ShellEvent>) {
    let tokens = ui.tokens();
    metadata(
        ui,
        if draft.original_executable.is_some() {
            "Edit policy"
        } else {
            "New policy"
        },
    );
    ui.add_space(Space::SM);
    ui.label(RichText::new("Application basename").color(tokens.secondary_text));
    ui.add(TextEdit::singleline(&mut draft.executable).hint_text("code.exe"));
    ui.label(RichText::new("Formatting").color(tokens.secondary_text));
    ComboBox::from_id_salt("profile-formatting")
        .selected_text(formatting_label(draft.formatting))
        .show_ui(ui, |ui| {
            for value in [
                FormattingStrength::Raw,
                FormattingStrength::Light,
                FormattingStrength::Balanced,
                FormattingStrength::Strong,
                FormattingStrength::Custom,
            ] {
                ui.selectable_value(&mut draft.formatting, value, formatting_label(value));
            }
        });
    if draft.formatting == FormattingStrength::Custom {
        ui.add(
            TextEdit::multiline(&mut draft.custom_instruction)
                .hint_text("Application-specific transformation.")
                .desired_rows(3)
                .desired_width(f32::INFINITY),
        );
    }
    ui.label(RichText::new("Language tag · optional").color(tokens.secondary_text));
    ui.add(TextEdit::singleline(&mut draft.language).hint_text("en or pt-br"));
    ui.label(RichText::new("Insertion").color(tokens.secondary_text));
    ComboBox::from_id_salt("profile-insertion")
        .selected_text(insertion_label(draft.insertion))
        .show_ui(ui, |ui| {
            for value in [
                ProfileInsertion::Automatic,
                ProfileInsertion::Direct,
                ProfileInsertion::Clipboard,
            ] {
                ui.selectable_value(&mut draft.insertion, value, insertion_label(value));
            }
        });
    ui.checkbox(&mut draft.blocked, "Block dictation in this application");
    ui.add_space(Space::LG);
    ui.horizontal(|ui| {
        if action(ui, "Save policy", ActionTone::Primary).clicked() {
            outbox.push(ShellEvent::SaveProfile(draft.clone()));
        }
        if action(ui, "Cancel", ActionTone::Quiet).clicked() {
            outbox.push(ShellEvent::CancelProfileEdit);
        }
    });
}

fn parse_formatting(value: &str) -> FormattingStrength {
    match value {
        "Raw" => FormattingStrength::Raw,
        "Light" => FormattingStrength::Light,
        "Strong" => FormattingStrength::Strong,
        "Custom" => FormattingStrength::Custom,
        _ => FormattingStrength::Balanced,
    }
}

fn parse_insertion(value: &str) -> ProfileInsertion {
    if value.contains("Clipboard") {
        ProfileInsertion::Clipboard
    } else if value.contains("Direct") {
        ProfileInsertion::Direct
    } else {
        ProfileInsertion::Automatic
    }
}

const fn insertion_label(value: ProfileInsertion) -> &'static str {
    match value {
        ProfileInsertion::Automatic => "Automatic",
        ProfileInsertion::Direct => "Direct",
        ProfileInsertion::Clipboard => "Clipboard only",
    }
}

fn models(
    ui: &mut Ui,
    snapshot: &ShellSnapshot,
    state: &mut PageState,
    outbox: &mut Vec<ShellEvent>,
) {
    if page_header(
        ui,
        Route::Models.title(),
        Route::Models.context(),
        Some("Verify all"),
    ) {
        outbox.push(ShellEvent::VerifyModels);
    }
    let tokens = ui.tokens();
    model_system(ui, "01", &snapshot.whisper, true, state, outbox);
    if state.confirm_model_download {
        ui.add_space(Space::MD);
        ui.label(
            RichText::new(
                "Download the pinned 141 MiB English Whisper model. The file stays local and replaces the active model only after SHA-256 verification.",
            )
            .color(tokens.secondary_text),
        );
        ui.add_space(Space::SM);
        ui.horizontal(|ui| {
            if action(ui, "Download and verify", ActionTone::Primary).clicked() {
                state.confirm_model_download = false;
                outbox.push(ShellEvent::ChangeWhisperModel);
            }
            if action(ui, "Cancel", ActionTone::Quiet).clicked() {
                state.confirm_model_download = false;
            }
        });
    }
    ui.add_space(Space::XL);
    hairline(ui);
    ui.add_space(Space::XL);
    model_system(ui, "02", &snapshot.ollama, false, state, outbox);
}

fn model_system(
    ui: &mut Ui,
    index: &str,
    system: &crate::model::ModelSystem,
    whisper: bool,
    state: &mut PageState,
    outbox: &mut Vec<ShellEvent>,
) {
    let tokens = ui.tokens();
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(index)
                .monospace()
                .size(12.0)
                .color(tokens.accent_focus),
        );
        ui.vertical(|ui| {
            ui.label(RichText::new(&system.name).size(24.0).color(tokens.text));
            ui.label(RichText::new(&system.detail).color(tokens.secondary_text));
        });
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            readiness_row(ui, "", "", system.state);
        });
    });
    ui.add_space(Space::LG);
    ui.horizontal(|ui| {
        ui.label(RichText::new("Selected").color(tokens.secondary_text));
        ui.label(
            RichText::new(system.selected.as_deref().unwrap_or("None"))
                .monospace()
                .color(tokens.text),
        );
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if whisper {
                if action(
                    ui,
                    "Download recommended · base.en · 141 MiB",
                    ActionTone::Secondary,
                )
                .clicked()
                {
                    state.confirm_model_download = true;
                }
            } else {
                for model in &system.installed {
                    if action(ui, model, ActionTone::Secondary).clicked() {
                        outbox.push(ShellEvent::SelectOllamaModel(model.clone()));
                    }
                }
            }
        });
    });
}

fn settings(
    ui: &mut Ui,
    snapshot: &ShellSnapshot,
    state: &mut PageState,
    outbox: &mut Vec<ShellEvent>,
) {
    let tokens = ui.tokens();
    let ollama_ready = snapshot.ollama.state == crate::model::Readiness::Ready
        && snapshot.ollama.selected.as_ref().is_some_and(|selected| {
            snapshot
                .ollama
                .installed
                .iter()
                .any(|installed| installed == selected)
        });
    if page_header(
        ui,
        Route::Settings.title(),
        Route::Settings.context(),
        state.settings_dirty.then_some("Save changes"),
    ) {
        outbox.push(ShellEvent::SaveSettings(state.settings.clone()));
    }
    ScrollArea::vertical().id_salt("settings").show(ui, |ui| {
        let original = state.settings.clone();
        setting_section(ui, "01", "Input", |ui| {
            setting_row(ui, "Microphone", "The source Phorminx listens to.", |ui| {
                ComboBox::from_id_salt("microphone")
                    .selected_text(&state.settings.microphone)
                    .show_ui(ui, |ui| {
                        for microphone in &state.settings.microphones {
                            ui.selectable_value(
                                &mut state.settings.microphone,
                                microphone.clone(),
                                microphone,
                            );
                        }
                    });
            });
            setting_row(
                ui,
                "Recording mode",
                "Hold or toggle the global shortcut.",
                |ui| {
                    ui.selectable_value(
                        &mut state.settings.recording_mode,
                        RecordingMode::Hold,
                        "Hold",
                    );
                    ui.selectable_value(
                        &mut state.settings.recording_mode,
                        RecordingMode::Toggle,
                        "Toggle",
                    );
                },
            );
        });
        setting_section(ui, "02", "Recognition", |ui| {
            setting_row(ui, "Language", "Prefer a recognition language.", |ui| {
                ComboBox::from_id_salt("language")
                    .selected_text(&state.settings.language)
                    .show_ui(ui, |ui| {
                        for language in ["English", "Português (Brasil)"] {
                            ui.selectable_value(
                                &mut state.settings.language,
                                language.to_owned(),
                                language,
                            );
                        }
                    });
            });
            setting_row(
                ui,
                "Minimum speech level",
                "Reject recordings below this RMS threshold (0–1).",
                |ui| {
                    ui.add(
                        TextEdit::singleline(&mut state.settings.minimum_rms).desired_width(100.0),
                    );
                },
            );
        });
        setting_section(ui, "03", "Formatting", |ui| {
            setting_row(ui, "Strength", "How much phrasing may change.", |ui| {
                ComboBox::from_id_salt("formatting")
                    .selected_text(formatting_label(state.settings.formatting))
                    .show_ui(ui, |ui| {
                        for value in [
                            FormattingStrength::Raw,
                            FormattingStrength::Light,
                            FormattingStrength::Balanced,
                            FormattingStrength::Strong,
                            FormattingStrength::Custom,
                        ] {
                            let available = matches!(
                                value,
                                FormattingStrength::Raw | FormattingStrength::Light
                            ) || ollama_ready;
                            ui.add_enabled_ui(available, |ui| {
                                ui.selectable_value(
                                    &mut state.settings.formatting,
                                    value,
                                    formatting_label(value),
                                );
                            });
                        }
                    });
            });
            if !ollama_ready
                && !matches!(
                    state.settings.formatting,
                    FormattingStrength::Raw | FormattingStrength::Light
                )
            {
                ui.label(
                    RichText::new(
                        "Select an installed Ollama model before saving this formatting strength.",
                    )
                    .color(tokens.accent_focus),
                );
            }
            if state.settings.formatting == FormattingStrength::Custom {
                ui.add(
                    TextEdit::multiline(&mut state.settings.custom_instruction)
                        .hint_text("State the transformation. Protected tokens remain exact.")
                        .desired_rows(3)
                        .desired_width(f32::INFINITY),
                );
            }
            setting_row(
                ui,
                "Model residency",
                "How long Ollama remains ready in memory.",
                |ui| {
                    ComboBox::from_id_salt("ollama-lifecycle")
                        .selected_text(lifecycle_label(state.settings.ollama_lifecycle))
                        .show_ui(ui, |ui| {
                            for value in [
                                OllamaLifecycle::Instant,
                                OllamaLifecycle::Balanced,
                                OllamaLifecycle::MemorySaver,
                            ] {
                                ui.selectable_value(
                                    &mut state.settings.ollama_lifecycle,
                                    value,
                                    lifecycle_label(value),
                                );
                            }
                        });
                },
            );
        });
        setting_section(ui, "04", "Privacy", |ui| {
            setting_row(ui, "History", "Retain completed local dictations.", |ui| {
                ComboBox::from_id_salt("history-retention")
                    .selected_text(&state.settings.history_retention)
                    .show_ui(ui, |ui| {
                        for value in ["Off", "1 day", "7 days", "30 days", "Indefinitely"] {
                            ui.selectable_value(
                                &mut state.settings.history_retention,
                                value.to_owned(),
                                value,
                            );
                        }
                    });
            });
        });
        setting_section(ui, "05", "Startup", |ui| {
            setting_row(
                ui,
                "Launch at login",
                "Ready before the first word.",
                |ui| {
                    ui.checkbox(&mut state.settings.launch_at_login, "Enabled");
                },
            );
        });
        setting_section(ui, "06", "Advanced", |ui| {
            setting_row(
                ui,
                "Whisper model path",
                "Local file used for speech recognition.",
                |ui| {
                    ui.add(
                        TextEdit::singleline(&mut state.settings.model_path).desired_width(360.0),
                    );
                },
            );
            setting_row(
                ui,
                "Local diagnostics",
                "Refresh content-free readiness checks.",
                |ui| {
                    if action(ui, "Refresh checks", ActionTone::Secondary).clicked() {
                        outbox.push(ShellEvent::VerifyModels);
                    }
                },
            );
        });
        state.settings_dirty |= original != state.settings;
    });
}

fn setting_section(ui: &mut Ui, index: &str, title: &str, content: impl FnOnce(&mut Ui)) {
    let tokens = ui.tokens();
    ui.horizontal(|ui| {
        ui.set_min_width(130.0);
        ui.label(RichText::new(index).monospace().color(tokens.accent_focus));
        ui.label(RichText::new(title).size(18.0).color(tokens.text));
    });
    ui.add_space(Space::SM);
    content(ui);
    ui.add_space(Space::LG);
    hairline(ui);
    ui.add_space(Space::LG);
}

fn setting_row(ui: &mut Ui, title: &str, detail: &str, control: impl FnOnce(&mut Ui)) {
    let tokens = ui.tokens();
    ui.horizontal(|ui| {
        ui.set_min_height(52.0);
        ui.vertical(|ui| {
            ui.label(RichText::new(title).color(tokens.text));
            ui.label(
                RichText::new(detail)
                    .size(12.0)
                    .color(tokens.secondary_text),
            );
        });
        ui.with_layout(Layout::right_to_left(Align::Center), control);
    });
}

fn labeled_value(ui: &mut Ui, label: &str, value: &str, monospace: bool) {
    let tokens = ui.tokens();
    metadata(ui, label);
    let text = if monospace {
        RichText::new(value).monospace()
    } else {
        RichText::new(value)
    };
    ui.label(text.color(tokens.text));
    ui.add_space(Space::MD);
}

const fn formatting_label(value: FormattingStrength) -> &'static str {
    match value {
        FormattingStrength::Raw => "Raw",
        FormattingStrength::Light => "Light",
        FormattingStrength::Balanced => "Balanced",
        FormattingStrength::Strong => "Strong",
        FormattingStrength::Custom => "Custom",
    }
}

const fn lifecycle_label(value: OllamaLifecycle) -> &'static str {
    match value {
        OllamaLifecycle::Instant => "Instant",
        OllamaLifecycle::Balanced => "Balanced",
        OllamaLifecycle::MemorySaver => "Memory saver",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::GalleryScenario;

    #[test]
    fn every_route_renders_all_gallery_scenarios() {
        egui::__run_test_ui(|ui| {
            for scenario in GalleryScenario::ALL {
                let snapshot = ShellSnapshot::gallery(scenario);
                for route in Route::ALL {
                    let mut state = PageState::from_snapshot(&snapshot);
                    let mut events = Vec::new();
                    show(ui, route, &snapshot, &mut state, &mut events);
                }
            }
        });
    }

    #[test]
    fn state_reconciles_removed_selections() {
        let populated = ShellSnapshot::gallery(GalleryScenario::Populated);
        let empty = ShellSnapshot::gallery(GalleryScenario::Empty);
        let mut state = PageState::from_snapshot(&populated);
        state.reconcile(&empty);
        assert_eq!(state.history_id, None);
        assert_eq!(state.lexicon_id, None);
        assert_eq!(state.profile_name, None);
    }

    #[test]
    fn empty_collections_render_active_creation_editors() {
        let snapshot = ShellSnapshot::gallery(GalleryScenario::Empty);
        egui::__run_test_ui(|ui| {
            let mut state = PageState::from_snapshot(&snapshot);
            state.lexicon_draft = Some(LexiconDraft::default());
            let mut events = Vec::new();
            lexicon(ui, &snapshot, &mut state, &mut events);
            assert!(state.lexicon_draft.is_some());

            state.profile_draft = Some(ProfileDraft::default());
            profiles(ui, &snapshot, &mut state, &mut events);
            assert!(state.profile_draft.is_some());
        });
    }
}
