//! Focused workspace surfaces. All actions are typed intent, never side effects.
use eframe::egui::{self, RichText, TextEdit, Ui};

use crate::components::{ActionTone, action, hairline, metadata};
use crate::model::*;
use crate::pages::PageState;
use crate::theme::{Space, UiThemeExt};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ShortcutTarget {
    Launcher,
    Direct,
}

pub(crate) fn settings_navigation(ui: &mut Ui, route: Route, outbox: &mut Vec<ShellEvent>) {
    let current = if route == Route::Settings {
        Route::SettingsShortcuts
    } else {
        route
    };
    ui.horizontal_wrapped(|ui| {
        for destination in [
            Route::SettingsShortcuts,
            Route::SettingsDictation,
            Route::SettingsFormatting,
            Route::SettingsAppearance,
            Route::SettingsPrivacy,
        ] {
            if ui
                .selectable_label(current == destination, destination.label())
                .clicked()
            {
                outbox.push(ShellEvent::Navigate(destination));
            }
        }
    });
    ui.add_space(Space::LG);
}

pub(crate) fn models_navigation(ui: &mut Ui, route: Route, outbox: &mut Vec<ShellEvent>) {
    ui.horizontal_wrapped(|ui| {
        for (destination, label) in [
            (Route::Models, "Installed models"),
            (Route::Setup, "Setup & repair"),
        ] {
            if ui.selectable_label(route == destination, label).clicked() {
                outbox.push(ShellEvent::Navigate(destination));
            }
        }
    });
    ui.add_space(Space::LG);
}

pub(crate) fn shortcuts(ui: &mut Ui, state: &mut PageState, outbox: &mut Vec<ShellEvent>) {
    let original = state.settings.clone();
    // Never record the key which activated the capture button in this same frame.
    let capture_at_frame_start = state.shortcut_capture;
    let tokens = ui.tokens();
    metadata(ui, "EVERYWHERE YOU WORK");
    ui.add_space(Space::SM);
    ui.label(RichText::new("Open Phorminx").size(22.0));
    ui.label(
        RichText::new("Your main shortcut opens the launcher. Press once; choose what comes next.")
            .color(tokens.secondary_text),
    );
    shortcut_editor(ui, state, ShortcutTarget::Launcher, outbox);
    ui.add_space(Space::XL);
    hairline(ui);
    ui.add_space(Space::LG);
    ui.label(RichText::new("Go straight to dictation").size(22.0));
    ui.label(
        RichText::new("An optional second shortcut, without opening the launcher.")
            .color(tokens.secondary_text),
    );
    shortcut_editor(ui, state, ShortcutTarget::Direct, outbox);
    if !state.settings.direct_dictation_shortcut.trim().is_empty() {
        ui.horizontal_wrapped(|ui| {
            ui.label("Direct shortcut behavior");
            ui.selectable_value(
                &mut state.settings.recording_mode,
                RecordingMode::Toggle,
                "Press to start / stop",
            );
            ui.selectable_value(
                &mut state.settings.recording_mode,
                RecordingMode::Hold,
                "Hold to record",
            );
        });
    }
    ui.add_space(Space::LG);
    ui.label(RichText::new("Use Ctrl with a key, or a function key except F12. Alt and Shift may accompany Ctrl. Type or capture a combination, then Save changes; reserved combinations are checked before applying.").size(12.0).color(tokens.secondary_text));
    if let Some(target) = state
        .shortcut_capture
        .filter(|target| Some(*target) == capture_at_frame_start)
    {
        ui.add_space(Space::MD);
        ui.label(
            RichText::new("Press your combination. Escape cancels.").color(tokens.accent_focus),
        );
        let events = ui.input(|input| input.events.clone());
        for event in events {
            if let egui::Event::Key {
                key,
                pressed: true,
                repeat: false,
                modifiers,
                ..
            } = event
            {
                if key == egui::Key::Escape {
                    state.shortcut_capture = None;
                    state.shortcut_capture_error = None;
                    outbox.push(ShellEvent::ShortcutCapture(false));
                    break;
                }
                let chord = match captured_chord(key, modifiers) {
                    Ok(chord) => chord,
                    Err(message) => {
                        state.shortcut_capture_error = Some(message);
                        continue;
                    }
                };
                match target {
                    ShortcutTarget::Launcher => state.settings.launcher_shortcut = chord,
                    ShortcutTarget::Direct => state.settings.direct_dictation_shortcut = chord,
                }
                state.shortcut_capture = None;
                state.shortcut_capture_error = None;
                outbox.push(ShellEvent::ShortcutCapture(false));
                break;
            }
        }
        if let Some(message) = state.shortcut_capture_error {
            ui.label(RichText::new(message).color(tokens.accent_focus));
        }
    }
    state.settings_dirty |= original != state.settings;
}

fn shortcut_editor(
    ui: &mut Ui,
    state: &mut PageState,
    target: ShortcutTarget,
    outbox: &mut Vec<ShellEvent>,
) {
    ui.add_space(Space::SM);
    ui.horizontal_wrapped(|ui| {
        let capturing = state.shortcut_capture == Some(target);
        let value = match target {
            ShortcutTarget::Launcher => &mut state.settings.launcher_shortcut,
            ShortcutTarget::Direct => &mut state.settings.direct_dictation_shortcut,
        };
        ui.add_enabled(
            !capturing,
            TextEdit::singleline(value)
                .id_salt(format!("shortcut-{target:?}"))
                .desired_width(240.0)
                .hint_text("Not assigned"),
        );
        if action(
            ui,
            if capturing {
                "Cancel capture"
            } else {
                "Capture shortcut"
            },
            ActionTone::Secondary,
        )
        .clicked()
        {
            state.shortcut_capture = if capturing { None } else { Some(target) };
            state.shortcut_capture_error = None;
            ui.memory_mut(|memory| {
                memory.surrender_focus(ui.id().with(format!("shortcut-{target:?}")))
            });
            outbox.push(ShellEvent::ShortcutCapture(!capturing));
        }
        if target == ShortcutTarget::Direct && action(ui, "Disable", ActionTone::Quiet).clicked() {
            state.settings.direct_dictation_shortcut.clear();
            state.shortcut_capture_error = None;
            if state.shortcut_capture.take().is_some() {
                outbox.push(ShellEvent::ShortcutCapture(false));
            }
        }
    });
}

fn captured_chord(key: egui::Key, modifiers: egui::Modifiers) -> Result<String, &'static str> {
    // Keep the capture policy aligned with the native Shortcut::parse policy.
    // `command` mirrors Ctrl on Windows, so it must not be rejected. `mac_cmd`
    // covers macOS Command input only; egui-winit does not expose Win here.
    if modifiers.mac_cmd || modifiers.alt && !modifiers.ctrl {
        return Err(
            "Windows-key and Alt-without-Ctrl shortcuts are not supported. Use Ctrl (optionally Alt/Shift), or a function key.",
        );
    }
    let name = format!("{key:?}");
    let key_name = if name.len() == 1 && name.as_bytes()[0].is_ascii_uppercase() {
        name
    } else if name.starts_with("Num") && name.len() == 4 {
        name[3..].to_owned()
    } else if matches!(key, egui::Key::Space | egui::Key::Enter)
        || name
            .strip_prefix('F')
            .and_then(|number| number.parse::<u8>().ok())
            .is_some_and(|number| (1..=24).contains(&number))
    {
        name
    } else {
        return Err("Choose a letter, number, Space, Enter, or a function key.");
    };
    let function_key = key_name.starts_with('F') && key_name.len() > 1;
    if !modifiers.ctrl && !function_key {
        return Err(
            "Letters, numbers, Space, and Enter need Ctrl so ordinary typing stays available.",
        );
    }
    if key == egui::Key::F12
        || key == egui::Key::F4 && modifiers.ctrl && !modifiers.alt && !modifiers.shift
    {
        return Err(
            "This shortcut is reserved by Windows or closes applications. Choose another combination.",
        );
    }
    let mut parts = Vec::new();
    if modifiers.ctrl {
        parts.push("Ctrl".to_owned());
    }
    if modifiers.alt {
        parts.push("Alt".to_owned());
    }
    if modifiers.shift {
        parts.push("Shift".to_owned());
    }
    parts.push(key_name);
    Ok(parts.join("+"))
}

/// Returns true when the ranked results replace the ordinary chronology.
pub(crate) fn library_search(
    ui: &mut Ui,
    snapshot: &ShellSnapshot,
    state: &mut PageState,
    outbox: &mut Vec<ShellEvent>,
) -> bool {
    let tokens = ui.tokens();
    if !snapshot.history_enabled {
        // Defensive privacy boundary even if a host refresh still carries old search hits.
        return false;
    }
    ui.add_space(Space::MD);
    let mut submit = false;
    ui.add_enabled_ui(snapshot.history_enabled, |ui| {
        ui.horizontal_wrapped(|ui| {
            let response = ui.add(
                TextEdit::singleline(&mut state.library_query)
                    .id_salt("library-search")
                    .hint_text("A phrase, a project, an idea…")
                    .desired_width((ui.available_width() - 110.0).max(180.0)),
            );
            submit |=
                response.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter));
            submit |= action(ui, "Search", ActionTone::Primary).clicked();
        });
        ui.horizontal_wrapped(|ui| {
            ui.selectable_value(
                &mut state.library_mode,
                LibrarySearchMode::Keyword,
                "Exact words",
            );
            ui.selectable_value(
                &mut state.library_mode,
                LibrarySearchMode::Semantic,
                "Meaning",
            );
            if (!state.library_query.is_empty()
                || snapshot.library.status != LibrarySearchStatus::Idle)
                && action(ui, "Show recent", ActionTone::Quiet).clicked()
            {
                state.library_query.clear();
                state.selected_passage = None;
                state.library_detail = false;
                outbox.push(ShellEvent::ClearLibrarySearch);
            }
        });
    });
    if submit && !state.library_query.trim().is_empty() {
        state.library_detail = false;
        state.selected_passage = None;
        outbox.push(ShellEvent::SearchLibrary {
            query: state.library_query.trim().to_owned(),
            mode: state.library_mode,
        });
    }
    if state.library_mode == LibrarySearchMode::Semantic
        && snapshot.library.index.selected_model.is_none()
    {
        ui.label(RichText::new("Meaning search uses a local embedding model. Choose one in Models; exact-word search remains available.").size(12.0).color(tokens.secondary_text));
        if action(ui, "Choose a search model", ActionTone::Quiet).clicked() {
            outbox.push(ShellEvent::Navigate(Route::Models));
        }
    }
    ui.add_space(Space::LG);
    hairline(ui);
    ui.add_space(Space::LG);
    if snapshot.library.status == LibrarySearchStatus::Idle {
        return false;
    }
    if state.library_detail {
        if action(ui, "Back to search results", ActionTone::Quiet).clicked() {
            state.library_detail = false;
        }
        return false;
    }
    if let Some(detail) = &snapshot.library.detail {
        ui.label(RichText::new(detail).color(tokens.secondary_text));
    }
    match snapshot.library.status {
        LibrarySearchStatus::Searching => {
            ui.spinner();
            ui.label("Finding your words…");
        }
        LibrarySearchStatus::Error | LibrarySearchStatus::Unavailable => {
            ui.label("Search needs attention. Your saved dictations are unchanged.");
        }
        LibrarySearchStatus::Ready if snapshot.library.results.is_empty() => {
            ui.label(RichText::new("No matching passages.").size(22.0));
            ui.label("Try a shorter phrase or search by meaning.");
        }
        _ => {}
    }
    if !snapshot.library.results.is_empty() {
        metadata(
            ui,
            &format!("{} MATCHING PASSAGES", snapshot.library.results.len()),
        );
        ui.add_space(Space::SM);
        for hit in &snapshot.library.results {
            ui.push_id((&hit.passage_id, hit.history_id), |ui| {
                let selected = state.history_id == Some(hit.history_id)
                    && state.selected_passage.as_deref() == Some(&hit.passage_id);
                egui::Frame::new()
                    .fill(if selected {
                        tokens.raised
                    } else {
                        tokens.surface
                    })
                    .inner_margin(egui::Margin::same(18))
                    .show(ui, |ui| {
                        ui.horizontal_wrapped(|ui| {
                            metadata(ui, &format!("{:02}  ·  {}", hit.rank, hit.time));
                            ui.label(
                                RichText::new(if hit.application.is_empty() {
                                    "Saved dictation"
                                } else {
                                    &hit.application
                                })
                                .size(12.0)
                                .color(tokens.secondary_text),
                            );
                        });
                        let response = ui.add(
                            egui::Button::new(
                                RichText::new(&hit.excerpt).size(18.0).color(tokens.text),
                            )
                            .wrap()
                            .frame(false)
                            .min_size(egui::vec2(ui.available_width(), 48.0)),
                        );
                        if response.clicked() {
                            state.selected_passage = Some(hit.passage_id.clone());
                            state.history_id = Some(hit.history_id);
                            state.history_variant = HistoryVariant::Output;
                            outbox.push(ShellEvent::SelectLibraryPassage {
                                history_id: hit.history_id,
                                passage_id: hit.passage_id.clone(),
                            });
                        }
                        if selected
                            && action(ui, "Open full dictation", ActionTone::Secondary).clicked()
                        {
                            state.library_query.clear();
                            outbox.push(ShellEvent::ClearLibrarySearch);
                            outbox.push(ShellEvent::SelectHistory(hit.history_id));
                        }
                    });
                ui.add_space(Space::SM);
            });
        }
    }
    true
}

pub(crate) fn library_index(ui: &mut Ui, snapshot: &ShellSnapshot, outbox: &mut Vec<ShellEvent>) {
    let index = &snapshot.library.index;
    let tokens = ui.tokens();
    ui.add_space(Space::LG);
    metadata(ui, "LIBRARY SEARCH");
    ui.add_space(Space::SM);
    ui.label(RichText::new("Find the idea, not just the phrase.").size(22.0));
    ui.label(RichText::new("Embeddings are created locally from saved text. They follow the library's retention and deletion choices.").color(tokens.secondary_text));
    ui.add_space(Space::MD);
    let mut selected = index.selected_model.clone().unwrap_or_default();
    let before = selected.clone();
    egui::ComboBox::from_id_salt("embedding-model")
        .selected_text(if selected.is_empty() {
            "Exact-word search only"
        } else {
            &selected
        })
        .show_ui(ui, |ui| {
            ui.selectable_value(&mut selected, String::new(), "Exact-word search only");
            for model in &index.models {
                ui.add_enabled_ui(model.available, |ui| {
                    ui.selectable_value(&mut selected, model.name.clone(), &model.name)
                        .on_hover_text(&model.detail);
                });
            }
        });
    if before != selected {
        outbox.push(ShellEvent::SelectEmbeddingModel(selected));
    }
    ui.add_space(Space::SM);
    let label = match index.state {
        LibraryIndexState::Disabled => "Meaning search is off",
        LibraryIndexState::MissingModel => "Search model needed",
        LibraryIndexState::Ready => "Search index ready",
        LibraryIndexState::Building => "Indexing saved passages",
        LibraryIndexState::Stale => "Index update needed",
        LibraryIndexState::Error => "Index needs attention",
    };
    ui.label(RichText::new(label).strong());
    if !index.detail.is_empty() {
        ui.label(
            RichText::new(&index.detail)
                .size(12.0)
                .color(tokens.secondary_text),
        );
    }
    if index.indexed_passages > 0 {
        metadata(ui, &format!("{} passages indexed", index.indexed_passages));
    }
    if let Some(total) = index.total_passages.filter(|total| *total > 0)
        && index.state == LibraryIndexState::Building
    {
        ui.add(egui::ProgressBar::new(
            (index.indexed_passages as f64 / total as f64).min(1.0) as f32,
        ));
    }
    ui.add_enabled_ui(
        snapshot.history_enabled
            && index.selected_model.is_some()
            && index.state != LibraryIndexState::Building,
        |ui| {
            if action(ui, "Rebuild search index", ActionTone::Secondary).clicked() {
                outbox.push(ShellEvent::RebuildLibraryIndex);
            }
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_requires_explicit_chord_or_function_key() {
        assert!(captured_chord(egui::Key::A, egui::Modifiers::NONE).is_err());
        assert_eq!(
            captured_chord(
                egui::Key::Space,
                egui::Modifiers {
                    ctrl: true,
                    alt: true,
                    ..Default::default()
                }
            ),
            Ok("Ctrl+Alt+Space".into())
        );
        assert_eq!(
            captured_chord(egui::Key::F8, egui::Modifiers::NONE),
            Ok("F8".into())
        );
        assert!(captured_chord(egui::Key::Escape, egui::Modifiers::CTRL).is_err());
    }

    #[test]
    fn capture_rejects_combinations_the_native_parser_cannot_save() {
        for (key, modifiers) in [
            (egui::Key::Space, egui::Modifiers::ALT),
            (egui::Key::F8, egui::Modifiers::ALT),
            (egui::Key::A, egui::Modifiers::SHIFT),
            (egui::Key::F12, egui::Modifiers::NONE),
            (egui::Key::F12, egui::Modifiers::CTRL),
            (egui::Key::F4, egui::Modifiers::CTRL),
            (
                egui::Key::F8,
                egui::Modifiers {
                    mac_cmd: true,
                    ..Default::default()
                },
            ),
            (
                egui::Key::A,
                egui::Modifiers {
                    mac_cmd: true,
                    ctrl: true,
                    ..Default::default()
                },
            ),
        ] {
            assert!(
                captured_chord(key, modifiers).is_err(),
                "{key:?} {modifiers:?}"
            );
        }
        assert_eq!(
            captured_chord(egui::Key::F8, egui::Modifiers::SHIFT),
            Ok("Shift+F8".into())
        );
        assert_eq!(
            captured_chord(
                egui::Key::A,
                egui::Modifiers {
                    ctrl: true,
                    command: true,
                    ..Default::default()
                }
            ),
            Ok("Ctrl+A".into())
        );
    }

    #[test]
    fn rejected_capture_preserves_draft_and_allows_retry_or_cancel() {
        for next_key in [egui::Key::F8, egui::Key::Escape] {
            let mut state = PageState::from_snapshot(&ShellSnapshot::default());
            let original = state.settings.clone();
            state.shortcut_capture = Some(ShortcutTarget::Launcher);
            let mut events = Vec::new();
            let context = egui::Context::default();
            for key in [egui::Key::F12, next_key] {
                let input = egui::RawInput {
                    events: vec![egui::Event::Key {
                        key,
                        physical_key: None,
                        pressed: true,
                        repeat: false,
                        modifiers: egui::Modifiers::NONE,
                    }],
                    ..Default::default()
                };
                let mut output = context.run_ui(input, |ui| shortcuts(ui, &mut state, &mut events));
                output.textures_delta.clear();
                if key == egui::Key::F12 {
                    assert_eq!(state.settings, original);
                    assert!(!state.settings_dirty);
                    assert_eq!(state.shortcut_capture, Some(ShortcutTarget::Launcher));
                    assert!(state.shortcut_capture_error.unwrap().contains("reserved"));
                    assert!(events.is_empty());
                }
            }
            assert!(state.shortcut_capture.is_none());
            assert!(state.shortcut_capture_error.is_none());
            assert_eq!(events, vec![ShellEvent::ShortcutCapture(false)]);
            if next_key == egui::Key::F8 {
                assert_eq!(state.settings.launcher_shortcut, "F8");
                assert!(state.settings_dirty);
            } else {
                assert_eq!(state.settings, original);
                assert!(!state.settings_dirty);
            }
        }
    }

    #[test]
    fn ranked_result_outside_recent_history_is_renderable() {
        let mut snapshot = ShellSnapshot::default();
        snapshot.library.status = LibrarySearchStatus::Ready;
        snapshot.library.results.push(LibrarySearchHit {
            history_id: 9999,
            passage_id: "9999:40".into(),
            time: "Yesterday".into(),
            application: String::new(),
            excerpt: "A thought from an older dictation.".into(),
            rank: 1,
        });
        let mut state = PageState::from_snapshot(&snapshot);
        let mut events = Vec::new();
        egui::__run_test_ui(|ui| assert!(library_search(ui, &snapshot, &mut state, &mut events)));
        assert!(events.is_empty());
    }

    #[test]
    fn disabled_history_never_renders_stale_search_results() {
        let mut snapshot = ShellSnapshot {
            history_enabled: false,
            ..Default::default()
        };
        snapshot.library.status = LibrarySearchStatus::Ready;
        snapshot.library.results.push(LibrarySearchHit {
            history_id: 1,
            passage_id: "0:20".into(),
            time: "Yesterday".into(),
            application: String::new(),
            excerpt: "stale private content".into(),
            rank: 1,
        });
        let mut state = PageState::from_snapshot(&snapshot);
        let mut events = Vec::new();
        egui::__run_test_ui(|ui| assert!(!library_search(ui, &snapshot, &mut state, &mut events)));
        assert!(events.is_empty());
    }

    #[test]
    fn active_capture_applies_one_key_then_resumes_shortcuts() {
        let mut state = PageState::from_snapshot(&ShellSnapshot::default());
        state.shortcut_capture = Some(ShortcutTarget::Direct);
        let mut events = Vec::new();
        let context = egui::Context::default();
        let input = egui::RawInput {
            events: vec![egui::Event::Key {
                key: egui::Key::F8,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
            ..Default::default()
        };
        let mut output = context.run_ui(input, |ui| shortcuts(ui, &mut state, &mut events));
        output.textures_delta.clear();
        assert_eq!(state.settings.direct_dictation_shortcut, "F8");
        assert!(state.shortcut_capture.is_none());
        assert!(state.settings_dirty);
        assert_eq!(events, vec![ShellEvent::ShortcutCapture(false)]);
    }
}
