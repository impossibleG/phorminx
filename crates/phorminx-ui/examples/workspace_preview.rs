//! Synthetic, in-memory visual harness. No runtime, clipboard or global keys.
use eframe::egui;
use phorminx_ui::theme::ThemeMode;
use phorminx_ui::*;

struct Preview {
    shell: PhorminxUi,
    snapshot: ShellSnapshot,
    theme: ThemeMode,
}

impl Default for Preview {
    fn default() -> Self {
        let mut snapshot = ShellSnapshot::gallery(GalleryScenario::Populated);
        snapshot.route = Route::Meetings;
        snapshot.workspace = phorminx_ui::workspace::WorkspaceSnapshot {
            history_enabled: true,
            title: "Product review · synthetic preview".into(),
            transcript: vec![phorminx_ui::workspace::MeetingLine { start_sample:0, end_sample:480000, text:"We should confirm the delivery date before discussing the next milestone.".into() }],
            messages: vec![phorminx_ui::workspace::ChatLine { role:"You".into(),text:"Help me respond to the timing question.".into(),status:"sent".into(), context_text:"Synthetic meeting context".into(), ..Default::default() },phorminx_ui::workspace::ChatLine { role:"Assistant".into(),text:"**Clarify the milestone first.**\n\n- Ask which milestone is essential.\n- Confirm the dependencies before committing to a date.".into(),status:"complete".into(), ..Default::default() }],
            input_devices: vec!["Example microphone".into()],
            output_devices: vec!["Example speakers".into()],
            available_models: vec!["example-local-model".into()],
            ..Default::default()
        };
        snapshot.library.index = LibraryIndexSnapshot {
            state: LibraryIndexState::Ready,
            detail: "All saved passages are available on this machine.".into(),
            indexed_passages: 64,
            total_passages: Some(64),
            selected_model: Some("nomic-embed-text".into()),
            models: vec![LibraryEmbeddingModel {
                name: "nomic-embed-text".into(),
                detail: "Local embedding model · preview only".into(),
                available: true,
            }],
        };
        Self {
            shell: PhorminxUi::new(snapshot.clone()),
            snapshot,
            theme: ThemeMode::AuthoredLight,
        }
    }
}

impl eframe::App for Preview {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        ui.horizontal_wrapped(|ui| {
            ui.label("SYNTHETIC PREVIEW · no real data or actions");
            for (theme, label) in [
                (ThemeMode::AuthoredLight, "Light"),
                (ThemeMode::AuthoredDark, "Dark"),
                (ThemeMode::HighContrast, "Contrast"),
            ] {
                ui.selectable_value(&mut self.theme, theme, label);
            }
        });
        self.shell.set_theme(self.theme);
        self.shell.show(ui);
        for event in self.shell.take_events() {
            match event {
                ShellEvent::Navigate(route) => self.snapshot.route = route,
                ShellEvent::SaveSettings(settings) => {
                    self.snapshot.settings = settings;
                    self.snapshot
                        .shortcut
                        .clone_from(&self.snapshot.settings.launcher_shortcut);
                }
                ShellEvent::TestDictation => {
                    self.snapshot.status = if self.snapshot.status == RuntimeStatus::Listening {
                        RuntimeStatus::Ready
                    } else {
                        RuntimeStatus::Listening
                    };
                }
                ShellEvent::SearchLibrary { query, mode } => {
                    self.snapshot.library.query = query;
                    self.snapshot.library.mode = mode;
                    self.snapshot.library.status = LibrarySearchStatus::Ready;
                    self.snapshot.library.results = self
                        .snapshot
                        .history
                        .iter()
                        .enumerate()
                        .map(|(rank, item)| LibrarySearchHit {
                            history_id: item.id,
                            passage_id: format!("{}:0", item.id),
                            time: item.time.clone(),
                            application: item.application.clone(),
                            excerpt: item.output_preview.clone(),
                            rank: rank + 1,
                        })
                        .collect();
                    self.snapshot.library.detail =
                        Some("Preview results · synthetic saved passages".into());
                }
                ShellEvent::ClearLibrarySearch => {
                    self.snapshot.library.status = LibrarySearchStatus::Idle;
                    self.snapshot.library.query.clear();
                    self.snapshot.library.results.clear();
                }
                ShellEvent::SelectLibraryPassage { history_id, .. } => {
                    self.shell.select_history_passage(history_id, 0)
                }
                ShellEvent::SelectHistory(id) => self.shell.select_history(id),
                ShellEvent::SelectEmbeddingModel(model) => {
                    self.snapshot.library.index.selected_model =
                        (!model.is_empty()).then_some(model);
                    self.snapshot.library.index.state =
                        if self.snapshot.library.index.selected_model.is_some() {
                            LibraryIndexState::Ready
                        } else {
                            LibraryIndexState::Disabled
                        };
                }
                ShellEvent::SelectHistoryVariant { id, variant } => {
                    self.shell.set_history_detail(id, variant, "A synthetic passage for visual review. Nothing here comes from a real dictation.".into());
                }
                _ => {}
            }
            self.shell.apply_snapshot(self.snapshot.clone());
        }
    }
}

fn main() -> eframe::Result {
    eframe::run_native(
        "Phorminx · Workspace preview",
        eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_inner_size([1180.0, 820.0])
                .with_min_inner_size([480.0, 620.0]),
            ..Default::default()
        },
        Box::new(|_| Ok(Box::<Preview>::default())),
    )
}
