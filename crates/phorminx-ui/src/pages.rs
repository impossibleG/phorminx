use eframe::egui::{
    self, Align, Button, ComboBox, Layout, RichText, ScrollArea, Stroke, TextEdit, Ui, Vec2,
};

use crate::components::{
    self, ActionTone, action, empty_state, hairline, metadata, page_header, readiness_row,
    section_title, segmented,
};
use crate::model::{
    FormattingStrength, HistoryVariant, RecordingMode, Route, SettingsSnapshot, ShellEvent,
    ShellSnapshot,
};
use crate::theme::{Colors, Space};

#[derive(Clone, Debug)]
pub(crate) struct PageState {
    pub history_id: Option<i64>,
    pub history_variant: HistoryVariant,
    pub lexicon_id: Option<i64>,
    pub profile_name: Option<String>,
    pub settings: SettingsSnapshot,
    pub settings_dirty: bool,
}

impl PageState {
    pub fn from_snapshot(snapshot: &ShellSnapshot) -> Self {
        Self {
            history_id: snapshot.history.first().map(|item| item.id),
            history_variant: HistoryVariant::Output,
            lexicon_id: snapshot.lexicon.first().map(|item| item.id),
            profile_name: snapshot
                .profiles
                .first()
                .map(|item| item.executable.clone()),
            settings: snapshot.settings.clone(),
            settings_dirty: false,
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
        Route::Models => models(ui, snapshot, outbox),
        Route::Settings => settings(ui, snapshot, state, outbox),
    }
}

fn home(ui: &mut Ui, snapshot: &ShellSnapshot, outbox: &mut Vec<ShellEvent>) {
    page_header(ui, Route::Home.title(), Route::Home.context(), None);
    ui.add_space(Space::LG);
    ui.horizontal(|ui| {
        ui.vertical(|ui| {
            metadata(ui, "Instrument state");
            ui.add_space(Space::SM);
            ui.label(
                RichText::new(snapshot.status.label())
                    .size(56.0)
                    .color(Colors::LIMESTONE),
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
        ui.label(RichText::new("History is off. Nothing spoken is retained.").color(Colors::ASH));
    } else if snapshot.history.is_empty() {
        ui.label(RichText::new("No dictations held yet.").color(Colors::ASH));
    } else {
        for item in snapshot.history.iter().take(3) {
            ui.horizontal(|ui| {
                ui.label(RichText::new(&item.time).monospace().color(Colors::ASH));
                if ui
                    .add(
                        Button::new(RichText::new(&item.output).color(Colors::LIMESTONE))
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
    page_header(ui, Route::History.title(), Route::History.context(), None);
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
                                Colors::LIMESTONE
                            } else {
                                Colors::ASH
                            }),
                        )
                        .selected(selected)
                        .fill(if selected {
                            Colors::TEMPERED
                        } else {
                            Colors::IRON
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
                    .color(Colors::LIMESTONE),
            );
            columns[1].add_space(Space::XL);
            hairline(&mut columns[1]);
            columns[1].add_space(Space::MD);
            columns[1].horizontal(|ui| {
                if action(ui, "Copy output", ActionTone::Primary).clicked() {
                    outbox.push(ShellEvent::CopyHistory {
                        id: item.id,
                        variant: state.history_variant,
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
                .color(Colors::ASH),
            );
            if let Some(warning) = &item.warning {
                columns[1].label(RichText::new(warning).color(Colors::BRONZE_LIGHT));
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
    if page_header(
        ui,
        Route::Lexicon.title(),
        Route::Lexicon.context(),
        Some("New entry"),
    ) {
        outbox.push(ShellEvent::NewLexiconEntry);
    }
    if snapshot.lexicon.is_empty() {
        if empty_state(
            ui,
            "No private vocabulary",
            "Teach exact replacements without training a model.",
            Some("New entry"),
        ) {
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
                            Colors::LIMESTONE
                        } else {
                            Colors::ASH
                        },
                    ),
                )
                .selected(selected)
                .fill(if selected {
                    Colors::TEMPERED
                } else {
                    Colors::IRON
                })
                .stroke(Stroke::NONE)
                .min_size(Vec2::new(columns[0].available_width(), 44.0)),
            );
            if response.clicked() {
                state.lexicon_id = Some(entry.id);
                outbox.push(ShellEvent::EditLexicon(entry.id));
            }
            hairline(&mut columns[0]);
        }
        if let Some(entry) = state
            .lexicon_id
            .and_then(|id| snapshot.lexicon.iter().find(|entry| entry.id == id))
        {
            metadata(&mut columns[1], "Entry");
            columns[1].add_space(Space::MD);
            labeled_value(&mut columns[1], "Spoken alias", &entry.spoken, false);
            labeled_value(&mut columns[1], "Written form", &entry.written, false);
            labeled_value(&mut columns[1], "Language", &entry.language, false);
            labeled_value(&mut columns[1], "Scope", &entry.scope, true);
            columns[1].add_space(Space::MD);
            columns[1].label(
                RichText::new(format!(
                    "Preview: I said “{}”; Phorminx kept “{}”.",
                    entry.spoken, entry.written
                ))
                .color(Colors::ASH),
            );
            columns[1].add_space(Space::LG);
            columns[1].horizontal(|ui| {
                if action(ui, "Edit", ActionTone::Primary).clicked() {
                    outbox.push(ShellEvent::EditLexicon(entry.id));
                }
                if action(ui, "Delete", ActionTone::Destructive).clicked() {
                    outbox.push(ShellEvent::DeleteLexicon(entry.id));
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
    if page_header(
        ui,
        Route::Profiles.title(),
        Route::Profiles.context(),
        Some("New profile"),
    ) {
        outbox.push(ShellEvent::NewProfile);
    }
    if snapshot.profiles.is_empty() {
        if empty_state(
            ui,
            "No contextual policy",
            "Global settings apply everywhere that dictation is allowed.",
            Some("New profile"),
        ) {
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
                            Colors::LIMESTONE
                        } else {
                            Colors::ASH
                        },
                    ))
                    .selected(selected)
                    .fill(if selected {
                        Colors::TEMPERED
                    } else {
                        Colors::IRON
                    })
                    .stroke(Stroke::NONE)
                    .min_size(Vec2::new(columns[0].available_width(), 44.0)),
                )
                .clicked()
            {
                state.profile_name = Some(profile.executable.clone());
                outbox.push(ShellEvent::EditProfile(profile.executable.clone()));
            }
            hairline(&mut columns[0]);
        }
        if let Some(profile) = state.profile_name.as_ref().and_then(|name| {
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
                    .color(Colors::LIMESTONE),
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
                        .color(Colors::OXBLOOD),
                );
            }
            columns[1].add_space(Space::XL);
            columns[1].horizontal(|ui| {
                if action(ui, "Edit policy", ActionTone::Primary).clicked() {
                    outbox.push(ShellEvent::EditProfile(profile.executable.clone()));
                }
                if action(ui, "Remove", ActionTone::Destructive).clicked() {
                    outbox.push(ShellEvent::RemoveProfile(profile.executable.clone()));
                }
            });
        }
    });
}

fn models(ui: &mut Ui, snapshot: &ShellSnapshot, outbox: &mut Vec<ShellEvent>) {
    if page_header(
        ui,
        Route::Models.title(),
        Route::Models.context(),
        Some("Verify all"),
    ) {
        outbox.push(ShellEvent::VerifyModels);
    }
    model_system(ui, "01", &snapshot.whisper, true, outbox);
    ui.add_space(Space::XL);
    hairline(ui);
    ui.add_space(Space::XL);
    model_system(ui, "02", &snapshot.ollama, false, outbox);
}

fn model_system(
    ui: &mut Ui,
    index: &str,
    system: &crate::model::ModelSystem,
    whisper: bool,
    outbox: &mut Vec<ShellEvent>,
) {
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(index)
                .monospace()
                .size(12.0)
                .color(Colors::BRONZE_LIGHT),
        );
        ui.vertical(|ui| {
            ui.label(
                RichText::new(&system.name)
                    .size(24.0)
                    .color(Colors::LIMESTONE),
            );
            ui.label(RichText::new(&system.detail).color(Colors::ASH));
        });
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            readiness_row(ui, "", "", system.state);
        });
    });
    ui.add_space(Space::LG);
    ui.horizontal(|ui| {
        ui.label(RichText::new("Selected").color(Colors::ASH));
        ui.label(
            RichText::new(system.selected.as_deref().unwrap_or("None"))
                .monospace()
                .color(Colors::LIMESTONE),
        );
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if whisper {
                if action(ui, "Change model", ActionTone::Secondary).clicked() {
                    outbox.push(ShellEvent::ChangeWhisperModel);
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
    _snapshot: &ShellSnapshot,
    state: &mut PageState,
    outbox: &mut Vec<ShellEvent>,
) {
    if page_header(
        ui,
        Route::Settings.title(),
        Route::Settings.context(),
        state.settings_dirty.then_some("Save changes"),
    ) {
        outbox.push(ShellEvent::SaveSettings(state.settings.clone()));
        state.settings_dirty = false;
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
                        for language in ["English", "Português (Brasil)", "Automatic"] {
                            ui.selectable_value(
                                &mut state.settings.language,
                                language.to_owned(),
                                language,
                            );
                        }
                    });
            });
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
                            ui.selectable_value(
                                &mut state.settings.formatting,
                                value,
                                formatting_label(value),
                            );
                        }
                    });
            });
            if state.settings.formatting == FormattingStrength::Custom {
                ui.add(
                    TextEdit::multiline(&mut state.settings.custom_instruction)
                        .hint_text("State the transformation. Protected tokens remain exact.")
                        .desired_rows(3)
                        .desired_width(f32::INFINITY),
                );
            }
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
            setting_row(
                ui,
                "Reduced motion",
                "Make state changes immediate.",
                |ui| {
                    ui.checkbox(&mut state.settings.reduced_motion, "Enabled");
                },
            );
        });
        state.settings_dirty |= original != state.settings;
    });
}

fn setting_section(ui: &mut Ui, index: &str, title: &str, content: impl FnOnce(&mut Ui)) {
    ui.horizontal(|ui| {
        ui.set_min_width(130.0);
        ui.label(RichText::new(index).monospace().color(Colors::BRONZE_LIGHT));
        ui.label(RichText::new(title).size(18.0).color(Colors::LIMESTONE));
    });
    ui.add_space(Space::SM);
    content(ui);
    ui.add_space(Space::LG);
    hairline(ui);
    ui.add_space(Space::LG);
}

fn setting_row(ui: &mut Ui, title: &str, detail: &str, control: impl FnOnce(&mut Ui)) {
    ui.horizontal(|ui| {
        ui.set_min_height(52.0);
        ui.vertical(|ui| {
            ui.label(RichText::new(title).color(Colors::LIMESTONE));
            ui.label(RichText::new(detail).size(12.0).color(Colors::ASH));
        });
        ui.with_layout(Layout::right_to_left(Align::Center), control);
    });
}

fn labeled_value(ui: &mut Ui, label: &str, value: &str, monospace: bool) {
    metadata(ui, label);
    let text = if monospace {
        RichText::new(value).monospace()
    } else {
        RichText::new(value)
    };
    ui.label(text.color(Colors::LIMESTONE));
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
}
