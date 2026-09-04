use eframe::egui::{
    self, Align, Button, ComboBox, Layout, RichText, ScrollArea, Stroke, TextEdit, Ui, Vec2,
};

use crate::components::{
    self, ActionTone, action, empty_state, hairline, metadata, page_header, readiness_row,
    section_title, segmented,
};
use crate::model::{
    AccurateBackend, AccurateModel, AppearancePreference, CalibrationCaptureState,
    FormattingStrength, HistoryVariant, LexiconCasePolicy, LexiconDraft, OllamaLifecycle,
    OllamaOperationState, OllamaSetupState, PerformancePreference, PerformanceRollbackState,
    PerformanceRunState, ProfileDraft, ProfileInsertion, RecognitionMode, RecordingMode, Route,
    SettingsSnapshot, ShellEvent, ShellSnapshot,
};
use crate::theme::{Space, UiThemeExt};

#[derive(Clone, Debug)]
pub(crate) struct PageState {
    pub history_id: Option<i64>,
    pub history_variant: HistoryVariant,
    pub history_page: usize,
    pub confirm_clear_history: bool,
    pub lexicon_id: Option<i64>,
    pub lexicon_draft: Option<LexiconDraft>,
    pub confirm_lexicon_delete: Option<i64>,
    pub profile_name: Option<String>,
    pub profile_draft: Option<ProfileDraft>,
    pub confirm_profile_delete: Option<String>,
    pub settings: SettingsSnapshot,
    pub settings_dirty: bool,
    pub confirm_model_download: Option<AccurateModel>,
    pub confirm_setup_action: Option<String>,
    pub ollama_search: String,
    pub confirm_ollama_page: bool,
    pub confirm_ollama_pull: Option<String>,
    pub confirm_ollama_activation: Option<String>,
    pub confirm_performance_apply: Option<String>,
    pub confirm_performance_revert: bool,
    pub confirm_performance_discard: bool,
}

impl PageState {
    pub fn from_snapshot(snapshot: &ShellSnapshot) -> Self {
        let first_history = snapshot.history.first();
        Self {
            history_id: first_history.map(|item| item.id),
            history_variant: first_history
                .and_then(super::model::HistoryItem::first_available_variant)
                .unwrap_or(HistoryVariant::Output),
            history_page: 0,
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
            confirm_model_download: None,
            confirm_setup_action: None,
            ollama_search: String::new(),
            confirm_ollama_page: false,
            confirm_ollama_pull: None,
            confirm_ollama_activation: None,
            confirm_performance_apply: None,
            confirm_performance_revert: false,
            confirm_performance_discard: false,
        }
    }

    pub fn reconcile(&mut self, snapshot: &ShellSnapshot) {
        if self
            .confirm_setup_action
            .as_ref()
            .is_some_and(|id| !snapshot.setup.actions.iter().any(|action| &action.id == id))
        {
            self.confirm_setup_action = None;
        }
        if self.confirm_ollama_pull.as_ref().is_some_and(|id| {
            !snapshot
                .setup
                .ollama
                .models
                .iter()
                .any(|model| &model.id == id)
        }) {
            self.confirm_ollama_pull = None;
        }
        if self.confirm_performance_apply.as_ref().is_some_and(|id| {
            snapshot
                .setup
                .performance
                .recommendation
                .as_ref()
                .is_none_or(|item| &item.id != id || !item.can_apply)
        }) {
            self.confirm_performance_apply = None;
        }
        if self
            .history_id
            .is_none_or(|id| !snapshot.history.iter().any(|item| item.id == id))
        {
            self.history_id = snapshot.history.first().map(|item| item.id);
            self.history_variant = snapshot
                .history
                .first()
                .and_then(super::model::HistoryItem::first_available_variant)
                .unwrap_or(HistoryVariant::Output);
            self.history_page = 0;
        }
        if self.history_id.and_then(|id| {
            snapshot
                .history
                .iter()
                .find(|item| item.id == id)
                .map(|item| item.has_variant(self.history_variant))
        }) == Some(false)
        {
            self.history_variant = self
                .history_id
                .and_then(|id| snapshot.history.iter().find(|item| item.id == id))
                .and_then(super::model::HistoryItem::first_available_variant)
                .unwrap_or(HistoryVariant::Output);
            self.history_page = 0;
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
        Route::Setup => setup(ui, snapshot, state, outbox),
        Route::History => history(ui, snapshot, state, outbox),
        Route::Lexicon => lexicon(ui, snapshot, state, outbox),
        Route::Profiles => profiles(ui, snapshot, state, outbox),
        Route::Models => models(ui, snapshot, state, outbox),
        Route::Settings => settings(ui, snapshot, state, outbox),
    }
}

fn setup(
    ui: &mut Ui,
    snapshot: &ShellSnapshot,
    state: &mut PageState,
    outbox: &mut Vec<ShellEvent>,
) {
    let tokens = ui.tokens();
    page_header(ui, Route::Setup.title(), Route::Setup.context(), None);
    ui.add_space(Space::LG);
    metadata(ui, "Commissioning state");
    ui.add_space(Space::XS);
    ui.label(
        RichText::new(snapshot.setup.stage.label())
            .size(32.0)
            .color(tokens.text),
    );
    ui.label(RichText::new(&snapshot.setup.summary).color(tokens.secondary_text));
    ui.add_space(Space::MD);
    if action(ui, "Inspect again", ActionTone::Secondary).clicked() {
        outbox.push(ShellEvent::RefreshSetup);
    }

    ui.add_space(Space::XL);
    section_title(
        ui,
        "01",
        "Local capabilities",
        "Observed evidence, not assumed readiness.",
    );
    for capability in &snapshot.setup.capabilities {
        readiness_row(ui, &capability.name, &capability.detail, capability.state);
        if let Some(remedy) = &capability.remedy {
            ui.horizontal(|ui| {
                ui.add_space(Space::MD);
                ui.label(
                    RichText::new(remedy)
                        .size(12.0)
                        .color(tokens.secondary_text),
                );
            });
        }
        hairline(ui);
    }

    if !snapshot.setup.actions.is_empty() {
        ui.add_space(Space::XL);
        section_title(
            ui,
            "02",
            "Plan",
            "Every side effect is disclosed before it begins.",
        );
        for planned in &snapshot.setup.actions {
            ui.label(RichText::new(&planned.title).strong().color(tokens.text));
            ui.label(RichText::new(&planned.detail).color(tokens.secondary_text));
            for consent in &planned.consent {
                ui.label(
                    RichText::new(format!("Consent · {consent}"))
                        .size(12.0)
                        .color(tokens.accent),
                );
            }
            if let Some(progress) = planned.progress_percent {
                ui.add(egui::ProgressBar::new(f32::from(progress) / 100.0).show_percentage());
            }
            ui.horizontal(|ui| {
                if planned.complete {
                    metadata(ui, "Complete");
                } else if !planned.available {
                    metadata(
                        ui,
                        "Unavailable until its verified prerequisite is satisfied",
                    );
                } else if planned.running {
                    if action(ui, "Cancel", ActionTone::Secondary).clicked() {
                        outbox.push(ShellEvent::CancelSetupAction(planned.id.clone()));
                    }
                } else if state.confirm_setup_action.as_deref() == Some(&planned.id) {
                    if action(ui, "I consent — begin", ActionTone::Primary).clicked() {
                        state.confirm_setup_action = None;
                        outbox.push(if planned.can_retry {
                            ShellEvent::RetrySetupAction(planned.id.clone())
                        } else {
                            ShellEvent::StartSetupAction(planned.id.clone())
                        });
                    }
                    if action(ui, "Not now", ActionTone::Secondary).clicked() {
                        state.confirm_setup_action = None;
                    }
                } else if planned.can_retry {
                    if planned.consent.is_empty() {
                        if action(ui, "Retry", ActionTone::Primary).clicked() {
                            outbox.push(ShellEvent::RetrySetupAction(planned.id.clone()));
                        }
                    } else if action(ui, "Review retry consent", ActionTone::Primary).clicked() {
                        state.confirm_setup_action = Some(planned.id.clone());
                    }
                } else if planned.consent.is_empty() {
                    if action(ui, "Continue", ActionTone::Primary).clicked() {
                        outbox.push(ShellEvent::StartSetupAction(planned.id.clone()));
                    }
                } else if action(ui, "Review consent", ActionTone::Primary).clicked() {
                    state.confirm_setup_action = Some(planned.id.clone());
                }
            });
            if state.confirm_setup_action.as_deref() == Some(&planned.id) {
                ui.label(
                    RichText::new("Only the disclosed operations above will be authorized. Confirm once to begin this action.")
                        .size(12.0)
                        .color(tokens.secondary_text),
                );
            }
            hairline(ui);
        }
    }

    if let Some(recommendation) = &snapshot.setup.recommendation {
        ui.add_space(Space::XL);
        section_title(
            ui,
            "03",
            "Measured recommendation",
            "Evidence remains on this machine.",
        );
        ui.label(
            RichText::new(&recommendation.title)
                .size(22.0)
                .color(tokens.text),
        );
        ui.label(RichText::new(&recommendation.rationale).color(tokens.secondary_text));
        for evidence in &recommendation.evidence {
            ui.label(
                RichText::new(evidence)
                    .monospace()
                    .color(tokens.secondary_text),
            );
        }
        if recommendation.can_apply {
            ui.add_space(Space::SM);
            if action(ui, "Apply recommendation", ActionTone::Primary).clicked() {
                outbox.push(ShellEvent::ApplySetupRecommendation(
                    recommendation.id.clone(),
                ));
            }
        }
    }

    ui.add_space(Space::XL);
    ollama_commissioning(ui, snapshot, state, outbox);
    ui.add_space(Space::XL);
    performance_commissioning(ui, snapshot, state, outbox);
}

fn ollama_commissioning(
    ui: &mut Ui,
    snapshot: &ShellSnapshot,
    state: &mut PageState,
    outbox: &mut Vec<ShellEvent>,
) {
    let tokens = ui.tokens();
    let ollama = &snapshot.setup.ollama;
    if !ollama.controls_enabled {
        state.confirm_ollama_pull = None;
        state.confirm_ollama_activation = None;
    }
    if !ollama.can_inspect {
        state.confirm_ollama_page = false;
    }
    section_title(
        ui,
        "03",
        "Local refinement",
        "A fixed loopback service and a small, reviewed model catalog.",
    );
    ui.add_space(Space::SM);
    ui.horizontal(|ui| {
        metadata(ui, ollama_state_label(ollama.state));
        if let Some(version) = &ollama.version {
            ui.label(
                RichText::new(format!("Version {version}"))
                    .monospace()
                    .color(tokens.secondary_text),
            );
        }
    });
    ui.label(RichText::new(&ollama.detail).color(tokens.secondary_text));
    ui.horizontal(|ui| {
        ui.add_enabled_ui(ollama.can_inspect, |ui| {
            if action(ui, "Inspect Ollama", ActionTone::Secondary).clicked() {
                outbox.push(ShellEvent::InspectOllama);
            }
        });
        if matches!(
            ollama.state,
            OllamaSetupState::Missing | OllamaSetupState::Incompatible
        ) {
            ui.add_enabled_ui(ollama.can_inspect, |ui| {
                if state.confirm_ollama_page {
                    if action(ui, "Open ollama.com", ActionTone::Primary).clicked() {
                        state.confirm_ollama_page = false;
                        outbox.push(ShellEvent::OpenOfficialOllamaDownload);
                    }
                    if action(ui, "Not now", ActionTone::Quiet).clicked() {
                        state.confirm_ollama_page = false;
                    }
                } else if action(ui, "Review manual install", ActionTone::Secondary).clicked() {
                    state.confirm_ollama_page = true;
                }
            });
        }
    });
    if state.confirm_ollama_page {
        ui.label(
            RichText::new("Consent · Open only https://ollama.com/download/windows. Phorminx will not download, execute, elevate, or enable autostart.")
                .size(12.0)
                .color(tokens.accent),
        );
    }
    if let Some(detail) = &ollama.operation_detail {
        ui.add_space(Space::SM);
        ui.label(RichText::new(detail).color(tokens.secondary_text));
    }
    if let Some(percent) = ollama.progress_percent {
        ui.add(egui::ProgressBar::new(f32::from(percent) / 100.0).show_percentage());
    }
    if matches!(
        ollama.operation,
        OllamaOperationState::Pulling | OllamaOperationState::Cancelling
    ) {
        ui.add_enabled_ui(ollama.operation == OllamaOperationState::Pulling, |ui| {
            if action(
                ui,
                if ollama.operation == OllamaOperationState::Cancelling {
                    "Cancelling…"
                } else {
                    "Cancel model acquisition"
                },
                ActionTone::Secondary,
            )
            .clicked()
            {
                outbox.push(ShellEvent::CancelOllamaPull);
            }
        });
    }

    if ollama.state != OllamaSetupState::Ready {
        return;
    }
    ui.add_space(Space::MD);
    ui.add(
        TextEdit::singleline(&mut state.ollama_search)
            .hint_text("Search the reviewed local catalog")
            .desired_width(360.0),
    );
    let terms = state.ollama_search.to_ascii_lowercase();
    for model in ollama.models.iter().filter(|model| {
        terms.split_whitespace().all(|term| {
            format!(
                "{} {} {}",
                model.display_name, model.exact_name, model.summary
            )
            .to_ascii_lowercase()
            .contains(term)
        })
    }) {
        ui.add_space(Space::SM);
        ui.label(
            RichText::new(&model.display_name)
                .size(17.0)
                .strong()
                .color(tokens.text),
        );
        ui.label(
            RichText::new(format!("{} · {}", model.exact_name, model.languages))
                .monospace()
                .color(tokens.secondary_text),
        );
        ui.label(RichText::new(&model.summary).color(tokens.secondary_text));
        ui.label(
            RichText::new(format!(
                "Model {} · free disk required {} · recommended RAM {}",
                format_bytes(model.model_bytes),
                format_bytes(model.minimum_free_disk_bytes),
                format_bytes(model.recommended_ram_bytes),
            ))
            .size(12.0)
            .color(tokens.secondary_text),
        );
        if !model.identity_matches {
            ui.label(RichText::new("A mutable tag with this name exists, but its exact digest or size is different. It cannot be selected.").color(tokens.destructive));
        } else if model.installed {
            if model.selected {
                metadata(ui, "Selected · exact identity verified");
            } else if state.confirm_ollama_activation.as_deref() == Some(&model.id) {
                ui.add_enabled_ui(ollama.controls_enabled, |ui| {
                    ui.horizontal(|ui| {
                        if action(ui, "Use this exact model", ActionTone::Primary).clicked() {
                            state.confirm_ollama_activation = None;
                            outbox.push(ShellEvent::ActivateCuratedOllamaModel(model.id.clone()));
                        }
                        if action(ui, "Not now", ActionTone::Quiet).clicked() {
                            state.confirm_ollama_activation = None;
                        }
                    })
                });
                ui.label(RichText::new("Consent · Re-verify the compiled digest and size, then atomically change only the local model selection.").size(12.0).color(tokens.accent));
            } else {
                ui.add_enabled_ui(ollama.controls_enabled, |ui| {
                    if action(ui, "Review selection", ActionTone::Secondary).clicked() {
                        state.confirm_ollama_activation = Some(model.id.clone());
                    }
                });
            }
        } else if state.confirm_ollama_pull.as_deref() == Some(&model.id) {
            ui.add_enabled_ui(ollama.controls_enabled, |ui| {
                ui.horizontal(|ui| {
                    if action(ui, "I consent — download", ActionTone::Primary).clicked() {
                        state.confirm_ollama_pull = None;
                        outbox.push(ShellEvent::PullCuratedOllamaModel(model.id.clone()));
                    }
                    if action(ui, "Not now", ActionTone::Quiet).clicked() {
                        state.confirm_ollama_pull = None;
                    }
                })
            });
            ui.label(RichText::new(format!("Consent · Ask the fixed loopback Ollama service to acquire exactly {}. Network transfer may leave a resumable cache if cancelled.", model.exact_name)).size(12.0).color(tokens.accent));
        } else {
            ui.add_enabled_ui(ollama.controls_enabled, |ui| {
                if action(ui, "Review acquisition", ActionTone::Secondary).clicked() {
                    state.confirm_ollama_pull = Some(model.id.clone());
                }
            });
        }
        hairline(ui);
    }
}

fn performance_commissioning(
    ui: &mut Ui,
    snapshot: &ShellSnapshot,
    state: &mut PageState,
    outbox: &mut Vec<ShellEvent>,
) {
    let tokens = ui.tokens();
    let performance = &snapshot.setup.performance;
    if !performance.controls_enabled {
        state.confirm_performance_apply = None;
    }
    if !performance.can_revert_after_restart {
        state.confirm_performance_revert = false;
    }
    if !performance.can_discard_rollback {
        state.confirm_performance_discard = false;
    }
    section_title(
        ui,
        "04",
        "Measured performance",
        "Four one-use captures. No insertion, clipboard, history, transcript, or audio persistence.",
    );
    ui.add_space(Space::SM);
    ui.label(
        RichText::new(format!("Pinned calibration · {}", performance.language))
            .monospace()
            .color(tokens.secondary_text),
    );
    ui.label(RichText::new(&performance.detail).color(tokens.secondary_text));

    ui.add_space(Space::MD);
    metadata(ui, "Optimize for");
    let mut preference = performance.preference;
    ui.add_enabled_ui(performance.controls_enabled, |ui| {
        if let Some(selected) = segmented(
            ui,
            [
                PerformancePreference::Fastest,
                PerformancePreference::Balanced,
                PerformancePreference::Quality,
            ],
            &mut preference,
            preference_label,
        ) {
            outbox.push(ShellEvent::SetPerformancePreference(selected));
        }
    });

    if !performance.candidates.is_empty() {
        ui.add_space(Space::MD);
        metadata(ui, "Verified candidates");
        for candidate in &performance.candidates {
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.label(RichText::new(&candidate.title).strong().color(tokens.text));
                    ui.label(
                        RichText::new(&candidate.detail)
                            .size(12.0)
                            .color(tokens.secondary_text),
                    );
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if candidate.selected {
                        metadata(ui, "Selected");
                    } else {
                        ui.add_enabled_ui(performance.controls_enabled, |ui| {
                            if action(ui, "Measure", ActionTone::Secondary).clicked() {
                                outbox.push(ShellEvent::SelectBenchmarkCandidate(
                                    candidate.id.clone(),
                                ));
                            }
                        });
                    }
                });
            });
            hairline(ui);
        }
    }
    for unavailable in &performance.unavailable {
        ui.label(
            RichText::new(format!(
                "Unavailable · {} · {}",
                unavailable.title, unavailable.reason
            ))
            .size(12.0)
            .color(tokens.secondary_text),
        );
    }

    if !performance.prompts.is_empty() {
        ui.add_space(Space::MD);
        metadata(ui, "Transient calibration set");
        for prompt in &performance.prompts {
            ui.add_space(Space::SM);
            ui.label(
                RichText::new(format!("{:02} · {}", prompt.ordinal, prompt.kind))
                    .monospace()
                    .color(tokens.accent_focus),
            );
            if prompt.kind == "Silence" {
                ui.label(RichText::new("Remain silent for about two seconds.").color(tokens.text));
            } else {
                ui.label(RichText::new(&prompt.text).color(tokens.text));
            }
            ui.horizontal(|ui| match prompt.capture {
                CalibrationCaptureState::Empty | CalibrationCaptureState::Failed => {
                    ui.add_enabled_ui(performance.controls_enabled, |ui| {
                        if action(ui, "Record", ActionTone::Primary).clicked() {
                            outbox.push(ShellEvent::StartCalibrationCapture(prompt.id.clone()));
                        }
                    });
                }
                CalibrationCaptureState::Starting => metadata(ui, "Opening microphone…"),
                CalibrationCaptureState::Recording => {
                    metadata(ui, "Recording in memory");
                    if action(ui, "Stop", ActionTone::Primary).clicked() {
                        outbox.push(ShellEvent::StopCalibrationCapture(prompt.id.clone()));
                    }
                }
                CalibrationCaptureState::Processing => metadata(ui, "Finalizing in memory…"),
                CalibrationCaptureState::Ready => {
                    metadata(ui, "Ready · memory only");
                    ui.add_enabled_ui(performance.controls_enabled, |ui| {
                        if action(ui, "Re-record", ActionTone::Secondary).clicked() {
                            outbox.push(ShellEvent::StartCalibrationCapture(prompt.id.clone()));
                        }
                        if action(ui, "Discard", ActionTone::Quiet).clicked() {
                            outbox.push(ShellEvent::DiscardCalibrationCapture(prompt.id.clone()));
                        }
                    });
                }
                CalibrationCaptureState::Consumed => metadata(ui, "Consumed and destroyed"),
            });
            if let Some(detail) = &prompt.detail {
                ui.label(RichText::new(detail).size(12.0).color(tokens.destructive));
            }
            hairline(ui);
        }
    }

    ui.add_space(Space::MD);
    match performance.state {
        PerformanceRunState::Ready => {
            ui.add_enabled_ui(performance.can_start_benchmark, |ui| {
                if action(ui, "Run local benchmark", ActionTone::Primary).clicked() {
                    outbox.push(ShellEvent::StartPerformanceBenchmark);
                }
            });
        }
        PerformanceRunState::Running | PerformanceRunState::Cancelling => {
            if performance.progress_total != 0 {
                ui.add(
                    egui::ProgressBar::new(
                        performance.progress_completed as f32 / performance.progress_total as f32,
                    )
                    .text(format!(
                        "{} / {} samples",
                        performance.progress_completed, performance.progress_total
                    )),
                );
            }
            if performance.can_cancel_benchmark
                && action(ui, "Cancel benchmark", ActionTone::Secondary).clicked()
            {
                outbox.push(ShellEvent::CancelPerformanceBenchmark);
            }
        }
        _ => {}
    }
    if performance
        .prompts
        .iter()
        .any(|prompt| prompt.capture == CalibrationCaptureState::Consumed)
        && ui
            .add_enabled_ui(performance.can_reset_calibration, |ui| {
                action(ui, "Prepare another measurement", ActionTone::Secondary).clicked()
            })
            .inner
    {
        outbox.push(ShellEvent::ResetPerformanceCalibration);
    }

    if !performance.evidence.is_empty() {
        ui.add_space(Space::MD);
        metadata(ui, "Measured evidence");
        for evidence in &performance.evidence {
            ui.label(RichText::new(&evidence.title).strong().color(tokens.text));
            ui.label(
                RichText::new(format!(
                    "release p50 / p95 · {} / {} ms   RTF · {:.3}",
                    evidence.release_p50_ms,
                    evidence.release_p95_ms,
                    evidence.realtime_factor_milli as f32 / 1000.0
                ))
                .monospace()
                .color(tokens.secondary_text),
            );
            ui.label(
                RichText::new(format!(
                    "word error · {:.1}%   hallucination · {:.1}%   protected exact · {:.1}%",
                    evidence.word_error_per_mille as f32 / 10.0,
                    evidence.hallucination_per_mille as f32 / 10.0,
                    evidence.protected_token_exact_per_mille as f32 / 10.0
                ))
                .monospace()
                .color(tokens.secondary_text),
            );
            ui.label(
                RichText::new(format!(
                    "working-set peak · {} MiB   available memory · {} MiB",
                    evidence.peak_working_set_mib, evidence.available_memory_mib
                ))
                .monospace()
                .color(tokens.secondary_text),
            );
            hairline(ui);
        }
    }

    if let Some(recommendation) = &performance.recommendation {
        ui.add_space(Space::MD);
        ui.label(
            RichText::new(&recommendation.title)
                .size(20.0)
                .color(tokens.text),
        );
        ui.label(RichText::new(&recommendation.rationale).color(tokens.secondary_text));
        for excluded in &recommendation.excluded {
            ui.label(
                RichText::new(format!("Excluded · {excluded}"))
                    .size(12.0)
                    .color(tokens.secondary_text),
            );
        }
        if recommendation.can_apply {
            if state.confirm_performance_apply.as_deref() == Some(&recommendation.id) {
                ui.horizontal(|ui| {
                    if action(ui, "Apply exact recommendation", ActionTone::Primary).clicked() {
                        state.confirm_performance_apply = None;
                        outbox.push(ShellEvent::ApplyPerformanceRecommendation(
                            recommendation.id.clone(),
                        ));
                    }
                    if action(ui, "Not now", ActionTone::Quiet).clicked() {
                        state.confirm_performance_apply = None;
                    }
                });
                ui.label(RichText::new("Consent · Re-verify the exact measured assets, compare-and-save settings once, then restart recognition. The narrow rollback remains available after restart.").size(12.0).color(tokens.accent));
            } else if action(ui, "Review apply", ActionTone::Primary).clicked() {
                state.confirm_performance_apply = Some(recommendation.id.clone());
            }
        }
    }

    if performance.rollback_state != PerformanceRollbackState::None {
        ui.add_space(Space::MD);
        metadata(ui, "Recognition rollback");
        if let Some(detail) = &performance.rollback_detail {
            ui.label(RichText::new(detail).color(tokens.secondary_text));
        }
        match performance.rollback_state {
            PerformanceRollbackState::Ready => {
                if state.confirm_performance_revert {
                    ui.horizontal(|ui| {
                        if action(ui, "Restore and restart", ActionTone::Primary).clicked() {
                            state.confirm_performance_revert = false;
                            outbox.push(ShellEvent::RevertPerformanceRecommendation);
                        }
                        if action(ui, "Not now", ActionTone::Quiet).clicked() {
                            state.confirm_performance_revert = false;
                        }
                    });
                    ui.label(RichText::new("Consent · Restore only the recognition fields changed by the recommendation, but only if current settings still match exactly. Phorminx restarts recognition after a verified save.").size(12.0).color(tokens.accent));
                } else if performance.can_revert_after_restart
                    && action(ui, "Review revert", ActionTone::Secondary).clicked()
                {
                    state.confirm_performance_revert = true;
                }
            }
            PerformanceRollbackState::Inspecting => metadata(ui, "Inspecting…"),
            PerformanceRollbackState::Working => metadata(ui, "Working…"),
            PerformanceRollbackState::StaleOrCorrupt => {
                ui.label(RichText::new("The receipt cannot safely restore settings. Discarding it never changes current settings.").size(12.0).color(tokens.destructive));
            }
            PerformanceRollbackState::Unavailable => metadata(ui, "Inspection unavailable"),
            PerformanceRollbackState::None => {}
        }
        if performance.can_discard_rollback {
            if state.confirm_performance_discard {
                ui.horizontal(|ui| {
                    if action(ui, "Discard rollback only", ActionTone::Secondary).clicked() {
                        state.confirm_performance_discard = false;
                        outbox.push(ShellEvent::DiscardPerformanceRollback);
                    }
                    if action(ui, "Keep rollback", ActionTone::Quiet).clicked() {
                        state.confirm_performance_discard = false;
                    }
                });
                ui.label(RichText::new("Consent · Permanently remove only the content-free rollback receipt. Current settings remain unchanged.").size(12.0).color(tokens.accent));
            } else if action(ui, "Review discard rollback", ActionTone::Quiet).clicked() {
                state.confirm_performance_discard = true;
            }
        }
    }
}

const fn ollama_state_label(state: OllamaSetupState) -> &'static str {
    match state {
        OllamaSetupState::Inspecting => "Inspecting",
        OllamaSetupState::Missing => "Not installed",
        OllamaSetupState::Unsafe => "Unsafe installation",
        OllamaSetupState::Stopped => "Installed · stopped",
        OllamaSetupState::Incompatible => "Update required",
        OllamaSetupState::Unhealthy => "Unhealthy response",
        OllamaSetupState::Ready => "Ready · loopback verified",
    }
}

const fn preference_label(value: PerformancePreference) -> &'static str {
    match value {
        PerformancePreference::Fastest => "Fastest",
        PerformancePreference::Balanced => "Balanced",
        PerformancePreference::Quality => "Quality",
    }
}

fn format_bytes(bytes: u64) -> String {
    if bytes >= 1024 * 1024 * 1024 {
        format!("{:.2} GiB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
    } else {
        format!("{:.0} MiB", bytes as f64 / (1024.0 * 1024.0))
    }
}

fn home(ui: &mut Ui, snapshot: &ShellSnapshot, outbox: &mut Vec<ShellEvent>) {
    let tokens = ui.tokens();
    page_header(ui, Route::Home.title(), Route::Home.context(), None);
    ui.add_space(Space::LG);
    if ui.available_width() >= 640.0 {
        ui.columns(2, |columns| {
            home_instrument_state(&mut columns[0], snapshot, outbox);
            home_local_systems(&mut columns[1], snapshot);
        });
    } else {
        home_instrument_state(ui, snapshot, outbox);
        ui.add_space(Space::XL);
        home_local_systems(ui, snapshot);
    }
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
                        Button::new(RichText::new(item.preview()).color(tokens.text))
                            .wrap()
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

fn home_instrument_state(ui: &mut Ui, snapshot: &ShellSnapshot, outbox: &mut Vec<ShellEvent>) {
    let tokens = ui.tokens();
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
}

fn home_local_systems(ui: &mut Ui, snapshot: &ShellSnapshot) {
    metadata(ui, "Local systems");
    ui.add_space(Space::SM);
    for system in &snapshot.systems {
        readiness_row(ui, &system.name, &system.detail, system.state);
        hairline(ui);
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
        ui.allocate_ui_with_layout(
            Vec2::new(ui.available_width(), 44.0),
            Layout::right_to_left(Align::Center),
            |ui| {
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
            },
        );
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
                                item.time,
                                item.application,
                                item.preview()
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
                        state.history_variant = item
                            .first_available_variant()
                            .unwrap_or(HistoryVariant::Output);
                        state.history_page = 0;
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
                .filter(|variant| item.has_variant(*variant));
            if let Some(variant) = segmented(
                &mut columns[1],
                available,
                &mut state.history_variant,
                HistoryVariant::label,
            ) {
                state.history_page = 0;
                outbox.push(ShellEvent::SelectHistoryVariant {
                    id: item.id,
                    variant,
                });
            }
            columns[1].add_space(Space::LG);
            let loaded = item.loaded_for(state.history_variant);
            let page_count = loaded.map_or(1, super::model::HistoryLoadedText::page_count);
            state.history_page = state.history_page.min(page_count.saturating_sub(1));
            if page_count > 1 {
                columns[1].horizontal(|ui| {
                    if action(ui, "Previous page", ActionTone::Quiet).clicked() {
                        state.history_page = state.history_page.saturating_sub(1);
                    }
                    metadata(
                        ui,
                        &format!("Page {} of {page_count}", state.history_page + 1),
                    );
                    if action(ui, "Next page", ActionTone::Quiet).clicked() {
                        state.history_page =
                            state.history_page.saturating_add(1).min(page_count - 1);
                    }
                });
                columns[1].add_space(Space::SM);
            }
            let text = loaded
                .map(|loaded| loaded.page(state.history_page))
                .unwrap_or_else(|| {
                    if item.has_variant(state.history_variant) {
                        "Loading transcript…"
                    } else {
                        "This retained transcript is unavailable."
                    }
                });
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
                if item.has_variant(HistoryVariant::Output)
                    && action(ui, "Copy output", ActionTone::Primary).clicked()
                {
                    outbox.push(ShellEvent::CopyHistory {
                        id: item.id,
                        variant: HistoryVariant::Output,
                    });
                }
                if item.has_variant(HistoryVariant::Raw)
                    && action(ui, "Copy raw", ActionTone::Quiet).clicked()
                {
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
    ui.add_space(Space::LG);
    model_system(ui, "02", &snapshot.vosk, false, state, outbox);
    if let Some(variant) = state.confirm_model_download {
        ui.add_space(Space::MD);
        ui.label(
            RichText::new(
                format!("Download {}. The file stays local and replaces the active model only after SHA-256 verification.", variant.label()),
            )
            .color(tokens.secondary_text),
        );
        ui.add_space(Space::SM);
        ui.horizontal(|ui| {
            if action(ui, "Download and verify", ActionTone::Primary).clicked() {
                state.confirm_model_download = None;
                outbox.push(ShellEvent::ChangeWhisperModel(variant));
            }
            if action(ui, "Cancel", ActionTone::Quiet).clicked() {
                state.confirm_model_download = None;
            }
        });
    }
    ui.add_space(Space::XL);
    hairline(ui);
    ui.add_space(Space::XL);
    model_system(ui, "03", &snapshot.ollama, false, state, outbox);
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
                for variant in [
                    AccurateModel::TinyEnglish,
                    AccurateModel::BaseEnglish,
                    AccurateModel::TinyMultilingual,
                    AccurateModel::BaseMultilingual,
                ] {
                    if action(ui, variant.label(), ActionTone::Secondary).clicked() {
                        state.confirm_model_download = Some(variant);
                    }
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
        setting_section(ui, "01", "Appearance", |ui| {
            setting_row(
                ui,
                "Theme",
                "Follow Windows or hold a deliberate light or dark palette.",
                |ui| {
                    ComboBox::from_id_salt("appearance")
                        .selected_text(appearance_label(state.settings.appearance))
                        .show_ui(ui, |ui| {
                            for value in [
                                AppearancePreference::System,
                                AppearancePreference::Light,
                                AppearancePreference::Dark,
                            ] {
                                ui.selectable_value(
                                    &mut state.settings.appearance,
                                    value,
                                    appearance_label(value),
                                );
                            }
                        });
                },
            );
        });
        setting_section(ui, "02", "Input", |ui| {
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
        setting_section(ui, "03", "Recognition", |ui| {
            setting_row(
                ui,
                "Mode",
                "Instant streams locally; Accurate prioritizes fidelity.",
                |ui| {
                    ui.selectable_value(
                        &mut state.settings.recognition_mode,
                        RecognitionMode::Instant,
                        "Instant",
                    );
                    ui.selectable_value(
                        &mut state.settings.recognition_mode,
                        RecognitionMode::Accurate,
                        "Accurate",
                    );
                },
            );
            setting_row(
                ui,
                "Accurate model",
                "Pinned Whisper model size and language coverage.",
                |ui| {
                    ComboBox::from_id_salt("accurate-model")
                        .selected_text(state.settings.accurate_model.label())
                        .show_ui(ui, |ui| {
                            for value in AccurateModel::ALL {
                                ui.selectable_value(
                                    &mut state.settings.accurate_model,
                                    value,
                                    value.label(),
                                );
                            }
                        });
                },
            );
            setting_row(
                ui,
                "Compute backend",
                "Auto uses Vulkan when it loads successfully and otherwise recovers on CPU.",
                |ui| {
                    ComboBox::from_id_salt("accurate-backend")
                        .selected_text(state.settings.accurate_backend.label())
                        .show_ui(ui, |ui| {
                            for value in AccurateBackend::ALL {
                                ui.selectable_value(
                                    &mut state.settings.accurate_backend,
                                    value,
                                    value.label(),
                                );
                            }
                        });
                },
            );
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
            if state.settings.recognition_mode == RecognitionMode::Instant
                && state.settings.language == "Português (Brasil)"
            {
                ui.label(
                    RichText::new(
                        "Portuguese Instant quality depends on the selected Vosk model. Use Accurate when fidelity matters.",
                    )
                    .color(tokens.accent_focus),
                );
            }
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
        setting_section(ui, "04", "Formatting", |ui| {
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
            if state.settings.recognition_mode == RecognitionMode::Instant
                && matches!(
                    state.settings.formatting,
                    FormattingStrength::Balanced
                        | FormattingStrength::Strong
                        | FormattingStrength::Custom
                )
            {
                ui.label(
                    RichText::new(
                        "Recognition streams instantly; AI formatting remains a separate post-release latency stage.",
                    )
                    .color(tokens.secondary_text),
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
        setting_section(ui, "05", "Privacy", |ui| {
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
        setting_section(ui, "06", "Startup", |ui| {
            setting_row(
                ui,
                "Launch at login",
                "Ready before the first word.",
                |ui| {
                    ui.checkbox(&mut state.settings.launch_at_login, "Enabled");
                },
            );
        });
        setting_section(ui, "07", "Advanced", |ui| {
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
                "Vosk model directory",
                "Local unpacked model used by Instant mode.",
                |ui| {
                    ui.add(
                        TextEdit::singleline(&mut state.settings.instant_model_path)
                            .desired_width(360.0),
                    );
                },
            );
            setting_row(
                ui,
                "Vosk runtime bundle",
                "Local folder containing libvosk.dll and adjacent dependencies.",
                |ui| {
                    ui.add(
                        TextEdit::singleline(&mut state.settings.instant_runtime_path)
                            .desired_width(360.0),
                    );
                },
            );
            setting_row(
                ui,
                "Verified Vosk import",
                "Choose the official runtime and model ZIPs. Phorminx verifies exact SHA-256 and size before safe extraction; it never downloads or executes archive contents.",
                |ui| {
                    if action(ui, "Import verified archives", ActionTone::Secondary).clicked() {
                        outbox.push(ShellEvent::InstallVerifiedVoskAssets);
                    }
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

const fn appearance_label(value: AppearancePreference) -> &'static str {
    match value {
        AppearancePreference::System => "System",
        AppearancePreference::Light => "Light",
        AppearancePreference::Dark => "Dark",
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
    use crate::model::{
        BenchmarkCandidateView, BenchmarkEvidenceView, CalibrationPromptView, GalleryScenario,
        OllamaModelChoice, PerformanceRecommendationView,
    };
    use crate::theme::{self, ThemeMode};

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
    fn stale_setup_confirmation_is_cleared_by_host_snapshot() {
        let populated = ShellSnapshot::gallery(GalleryScenario::Error);
        let mut state = PageState::from_snapshot(&populated);
        state.confirm_setup_action = Some("stale-action".to_owned());
        state.reconcile(&populated);
        assert!(state.confirm_setup_action.is_none());
    }

    #[test]
    fn unavailable_output_falls_back_to_the_first_readable_variant() {
        let mut snapshot = ShellSnapshot::gallery(GalleryScenario::Populated);
        snapshot.history[0].variants.output = false;
        snapshot.history[0].variants.raw = true;
        let state = PageState::from_snapshot(&snapshot);
        assert_eq!(state.history_variant, HistoryVariant::Raw);
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

    #[test]
    fn setup_feature_states_paint_without_overflow_or_missing_controls() {
        let mut snapshot = ShellSnapshot::gallery(GalleryScenario::Empty);
        snapshot.route = Route::Setup;
        snapshot.setup.ollama.state = OllamaSetupState::Ready;
        snapshot.setup.ollama.detail = "Fixed loopback API verified.".into();
        snapshot.setup.ollama.can_inspect = true;
        snapshot.setup.ollama.controls_enabled = true;
        snapshot.setup.ollama.models = vec![OllamaModelChoice {
            id: "gemma3-1b".into(),
            display_name: "Gemma 3 1B".into(),
            exact_name: "gemma3:1b".into(),
            summary: "Reviewed multilingual cleanup model.".into(),
            languages: "EN · PT-BR".into(),
            model_bytes: 800_000_000,
            minimum_free_disk_bytes: 1_400_000_000,
            recommended_ram_bytes: 4_000_000_000,
            installed: false,
            identity_matches: true,
            selected: false,
        }];
        snapshot.setup.performance.state = PerformanceRunState::Complete;
        snapshot.setup.performance.controls_enabled = true;
        snapshot.setup.performance.can_reset_calibration = true;
        snapshot.setup.performance.rollback_state = PerformanceRollbackState::Ready;
        snapshot.setup.performance.rollback_detail =
            Some("Previous recognition settings can be restored after restart.".into());
        snapshot.setup.performance.can_revert_after_restart = true;
        snapshot.setup.performance.can_discard_rollback = true;
        snapshot.setup.performance.candidates = vec![BenchmarkCandidateView {
            id: "candidate-one".into(),
            title: "Base Accurate on Vulkan".into(),
            detail: "Accurate · Vulkan · exact pinned identity".into(),
            selected: true,
        }];
        snapshot.setup.performance.prompts = vec![CalibrationPromptView {
            id: "en-speech-01".into(),
            ordinal: 1,
            kind: "Speech".into(),
            text: "Pinned calibration prompt.".into(),
            capture: CalibrationCaptureState::Consumed,
            detail: None,
        }];
        snapshot.setup.performance.evidence = vec![BenchmarkEvidenceView {
            candidate_id: "candidate-one".into(),
            title: "Base Accurate on Vulkan".into(),
            release_p50_ms: 220,
            release_p95_ms: 390,
            realtime_factor_milli: 90,
            word_error_per_mille: 50,
            hallucination_per_mille: 0,
            protected_token_exact_per_mille: 1_000,
            peak_working_set_mib: 640,
            available_memory_mib: 8_192,
        }];
        snapshot.setup.performance.recommendation = Some(PerformanceRecommendationView {
            id: "candidate-one".into(),
            title: "Base Accurate on Vulkan".into(),
            rationale: "All pinned gates passed.".into(),
            excluded: vec!["candidate-two · ReleaseLatencyTooHigh".into()],
            can_apply: true,
            can_revert: false,
        });
        for mode in [
            ThemeMode::AuthoredDark,
            ThemeMode::AuthoredLight,
            ThemeMode::HighContrast,
            ThemeMode::HighContrastLight,
        ] {
            egui::__run_test_ui(|ui| {
                theme::apply(ui.ctx(), mode);
                let mut state = PageState::from_snapshot(&snapshot);
                let mut events = Vec::new();
                setup(ui, &snapshot, &mut state, &mut events);
            });
        }
    }

    #[test]
    fn busy_feature_snapshots_clear_stale_one_shot_confirmations() {
        let mut snapshot = ShellSnapshot::gallery(GalleryScenario::Empty);
        snapshot.route = Route::Setup;
        snapshot.setup.ollama.can_inspect = false;
        snapshot.setup.ollama.controls_enabled = false;
        snapshot.setup.performance.controls_enabled = false;
        egui::__run_test_ui(|ui| {
            let mut state = PageState::from_snapshot(&snapshot);
            state.confirm_ollama_page = true;
            state.confirm_ollama_pull = Some("gemma3-1b".into());
            state.confirm_ollama_activation = Some("gemma3-1b".into());
            state.confirm_performance_apply = Some("candidate".into());
            state.confirm_performance_revert = true;
            state.confirm_performance_discard = true;
            let mut events = Vec::new();
            setup(ui, &snapshot, &mut state, &mut events);
            assert!(!state.confirm_ollama_page);
            assert!(state.confirm_ollama_pull.is_none());
            assert!(state.confirm_ollama_activation.is_none());
            assert!(state.confirm_performance_apply.is_none());
            assert!(!state.confirm_performance_revert);
            assert!(!state.confirm_performance_discard);
            assert!(events.is_empty());
        });
    }
}
