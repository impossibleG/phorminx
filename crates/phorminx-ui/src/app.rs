use eframe::egui::{self, Align, Color32, Layout, Margin, RichText, ScrollArea, Stroke, Ui, Vec2};

use crate::components::{
    hairline, inline_notice, nav_id, nav_item, request_page_header_focus, shortcut_chord,
    status_seal, tensioned_p,
};
use crate::model::{HistoryLoadedText, HistoryVariant, Route, ShellEvent, ShellSnapshot};
use crate::pages::{self, PageState};
use crate::theme::{self, Space, ThemeMode, UiThemeExt};

/// Single-window Phorminx product shell.
///
/// The host supplies immutable snapshots and drains typed intent. Runtime resources,
/// persistence handles, audio buffers, window titles, and full executable paths never
/// cross this boundary.
pub struct PhorminxUi {
    snapshot: ShellSnapshot,
    route: Route,
    pages: PageState,
    outbox: Vec<ShellEvent>,
    theme: ThemeMode,
    theme_applied: bool,
    route_focus_requested: bool,
}

impl Default for PhorminxUi {
    fn default() -> Self {
        Self::new(ShellSnapshot::default())
    }
}

impl PhorminxUi {
    #[must_use]
    pub fn new(snapshot: ShellSnapshot) -> Self {
        let route = snapshot.route;
        let pages = PageState::from_snapshot(&snapshot);
        Self {
            snapshot,
            route,
            pages,
            outbox: Vec::new(),
            theme: ThemeMode::AuthoredDark,
            theme_applied: false,
            route_focus_requested: false,
        }
    }

    #[must_use]
    pub const fn route(&self) -> Route {
        self.route
    }

    #[must_use]
    pub const fn snapshot(&self) -> &ShellSnapshot {
        &self.snapshot
    }

    pub fn apply_snapshot(&mut self, snapshot: ShellSnapshot) {
        let retained = self
            .snapshot
            .history
            .iter_mut()
            .find(|item| self.pages.history_id == Some(item.id))
            .and_then(|item| item.loaded.take())
            .filter(|loaded| loaded.variant == self.pages.history_variant);
        let retained_id = self.pages.history_id;
        self.route = snapshot.route;
        self.pages.reconcile(&snapshot);
        self.snapshot = snapshot;
        if self.route == Route::History
            && retained_id == self.pages.history_id
            && let (Some(id), Some(loaded)) = (retained_id, retained)
            && let Some(item) = self.snapshot.history.iter_mut().find(|item| item.id == id)
            && item.has_variant(loaded.variant)
        {
            item.loaded = Some(loaded);
        }
    }

    pub fn navigate(&mut self, route: Route) {
        if self.route != route {
            self.route = route;
            if route != Route::History {
                self.clear_history_detail();
            }
            self.outbox.push(ShellEvent::Navigate(route));
        }
    }

    pub fn set_theme(&mut self, theme: ThemeMode) {
        if self.theme != theme {
            self.theme = theme;
            self.theme_applied = false;
        }
    }

    /// Updates the live runtime seal without replacing collection snapshots.
    ///
    /// Dictation state changes are frequent and must not force the host to
    /// re-read history, lexicon, or profile rows merely to repaint one label.
    pub fn set_runtime_status(&mut self, status: crate::model::RuntimeStatus) {
        self.snapshot.status = status;
    }

    /// Moves keyboard focus into the destination route on the next paint.
    /// Hosts use this after tray/deep-link navigation so focus does not remain
    /// on the window chrome or on a control from the previous route.
    pub fn request_route_focus(&mut self) {
        self.route_focus_requested = true;
    }

    #[must_use]
    pub const fn theme(&self) -> ThemeMode {
        self.theme
    }

    pub fn close_lexicon_editor(&mut self) {
        self.pages.lexicon_draft = None;
    }

    pub fn close_profile_editor(&mut self) {
        self.pages.profile_draft = None;
    }

    pub fn select_history(&mut self, id: i64) {
        if let Some(item) = self.snapshot.history.iter().find(|item| item.id == id) {
            self.pages.history_id = Some(id);
            self.pages.history_variant = item
                .first_available_variant()
                .unwrap_or(crate::model::HistoryVariant::Output);
            self.pages.history_page = 0;
        }
    }

    #[must_use]
    pub fn history_selection(&self) -> Option<(i64, HistoryVariant)> {
        let id = self.pages.history_id?;
        self.snapshot
            .history
            .iter()
            .find(|item| item.id == id && item.has_variant(self.pages.history_variant))
            .map(|_| (id, self.pages.history_variant))
    }

    #[must_use]
    pub fn history_detail_loaded(&self, id: i64, variant: HistoryVariant) -> bool {
        self.snapshot
            .history
            .iter()
            .find(|item| item.id == id)
            .and_then(|item| item.loaded.as_ref())
            .is_some_and(|loaded| loaded.variant == variant)
    }

    /// Replaces any prior detail with one exact selected variant.
    pub fn set_history_detail(&mut self, id: i64, variant: HistoryVariant, text: String) -> bool {
        for item in &mut self.snapshot.history {
            item.loaded = None;
        }
        if self.pages.history_id != Some(id) || self.pages.history_variant != variant {
            return false;
        }
        let Some(item) = self.snapshot.history.iter_mut().find(|item| item.id == id) else {
            return false;
        };
        if !item.has_variant(variant) {
            return false;
        }
        item.loaded = Some(HistoryLoadedText { variant, text });
        self.pages.history_page = 0;
        true
    }

    pub fn clear_history_detail(&mut self) {
        for item in &mut self.snapshot.history {
            item.loaded = None;
        }
        self.pages.history_page = 0;
    }

    #[must_use]
    pub fn take_events(&mut self) -> Vec<ShellEvent> {
        std::mem::take(&mut self.outbox)
    }

    pub fn show(&mut self, ui: &mut Ui) {
        if !self.theme_applied {
            theme::apply(ui.ctx(), self.theme);
            self.theme_applied = true;
        }
        let tokens = ui.tokens();
        ui.set_min_size(Vec2::new(900.0, 620.0));
        egui::Frame::new()
            .fill(tokens.background)
            .inner_margin(Margin::ZERO)
            .show(ui, |ui| {
                self.brand_header(ui);
                hairline(ui);
                ui.horizontal_top(|ui| {
                    self.navigation(ui);
                    let (separator, _) = ui.allocate_exact_size(
                        Vec2::new(1.0, ui.available_height()),
                        egui::Sense::hover(),
                    );
                    ui.painter().line_segment(
                        [separator.left_top(), separator.left_bottom()],
                        Stroke::new(1.0, tokens.edge),
                    );
                    ScrollArea::vertical()
                        .id_salt("phorminx-route")
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            egui::Frame::new()
                                .inner_margin(Margin::same(32))
                                .show(ui, |ui| {
                                    // This frame is created inside the shell's horizontal
                                    // navigation row, so explicitly restore a vertical page
                                    // flow for route content.
                                    ui.vertical(|ui| {
                                        ui.set_width(ui.available_width());
                                        if let Some(notice) = &self.snapshot.notice {
                                            let (acted, dismissed) = inline_notice(ui, notice);
                                            if acted {
                                                self.outbox.push(ShellEvent::NoticeAction);
                                            }
                                            if dismissed {
                                                self.outbox.push(ShellEvent::DismissNotice);
                                            }
                                            ui.add_space(Space::LG);
                                        }
                                        if self.route_focus_requested {
                                            request_page_header_focus(ui.ctx());
                                            self.route_focus_requested = false;
                                        }
                                        pages::show(
                                            ui,
                                            self.route,
                                            &self.snapshot,
                                            &mut self.pages,
                                            &mut self.outbox,
                                        );
                                    });
                                });
                        });
                });
            });
    }

    // This is intentionally an in-content brand/status header. The native Windows
    // frame remains responsible for drag, resize, snap, system menu, and controls.
    fn brand_header(&self, ui: &mut Ui) {
        let tokens = ui.tokens();
        egui::Frame::new()
            .fill(tokens.surface)
            .inner_margin(Margin::symmetric(20, 10))
            .show(ui, |ui| {
                ui.set_height(48.0);
                ui.horizontal(|ui| {
                    tensioned_p(ui, 28.0);
                    ui.add_space(Space::SM);
                    ui.label(
                        RichText::new("PHORMINX")
                            .size(14.0)
                            .strong()
                            .color(tokens.text),
                    );
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        status_seal(ui, self.snapshot.status);
                    });
                });
            });
    }

    fn navigation(&mut self, ui: &mut Ui) {
        let tokens = ui.tokens();
        egui::Frame::new()
            .fill(tokens.surface)
            .inner_margin(Margin::symmetric(16, 20))
            .show(ui, |ui| {
                ui.set_min_width(180.0);
                ui.set_max_width(180.0);
                ui.set_min_height(ui.available_height());
                ui.vertical(|ui| {
                    if let Some(index) = Route::ALL.iter().position(|route| {
                        ui.memory(|memory| memory.has_focus(nav_id(ui, route.label())))
                    }) {
                        let delta = ui.input_mut(|input| {
                            if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown) {
                                1
                            } else if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp) {
                                -1
                            } else {
                                0
                            }
                        });
                        if delta != 0 {
                            let target = adjacent_route(index, delta);
                            ui.memory_mut(|memory| {
                                memory.request_focus(nav_id(ui, target.label()));
                            });
                            self.navigate(target);
                        }
                    }
                    for route in Route::ALL {
                        if nav_item(ui, route.label(), route == self.route).clicked() {
                            self.navigate(route);
                        }
                    }
                    ui.with_layout(Layout::bottom_up(Align::LEFT), |ui| {
                        shortcut_chord(ui, &self.snapshot.shortcut);
                        ui.add_space(Space::SM);
                        ui.label(
                            RichText::new("Local only")
                                .size(11.0)
                                .color(tokens.secondary_text),
                        );
                    });
                });
            });
    }
}

fn adjacent_route(index: usize, delta: isize) -> Route {
    let last = Route::ALL.len().saturating_sub(1);
    Route::ALL[(index as isize + delta).clamp(0, last as isize) as usize]
}

impl eframe::App for PhorminxUi {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        self.show(ui);
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        Color32::TRANSPARENT.to_normalized_gamma_f32()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{GalleryScenario, RuntimeStatus};

    #[test]
    fn navigation_emits_only_real_changes() {
        let mut app = PhorminxUi::default();
        app.navigate(Route::Home);
        assert!(app.take_events().is_empty());
        app.navigate(Route::Models);
        assert_eq!(app.take_events(), vec![ShellEvent::Navigate(Route::Models)]);
    }

    #[test]
    fn sidebar_arrow_navigation_clamps_at_both_ends() {
        assert_eq!(adjacent_route(0, -1), Route::Home);
        assert_eq!(adjacent_route(0, 1), Route::Setup);
        let last = Route::ALL.len() - 1;
        assert_eq!(adjacent_route(last, 1), Route::Settings);
        assert_eq!(adjacent_route(last, -1), Route::Models);
    }

    #[test]
    fn requested_route_focus_is_consumed_by_the_next_paint() {
        let mut app = PhorminxUi::default();
        app.request_route_focus();
        egui::__run_test_ui(|ui| app.show(ui));
        assert!(!app.route_focus_requested);
    }

    #[test]
    fn snapshot_is_host_authoritative() {
        let mut app = PhorminxUi::default();
        app.navigate(Route::History);
        let mut snapshot = ShellSnapshot::gallery(GalleryScenario::Error);
        snapshot.route = Route::Models;
        app.apply_snapshot(snapshot);
        assert_eq!(app.route(), Route::Models);
        assert_eq!(app.snapshot().status, RuntimeStatus::NeedsAttention);
    }

    #[test]
    fn home_history_selection_survives_route_navigation() {
        let snapshot = ShellSnapshot::gallery(GalleryScenario::Populated);
        let id = snapshot.history[1].id;
        let mut app = PhorminxUi::new(snapshot);
        app.select_history(id);
        app.navigate(Route::History);
        assert_eq!(app.pages.history_id, Some(id));
        assert_eq!(
            app.pages.history_variant,
            crate::model::HistoryVariant::Output
        );
    }

    #[test]
    fn exact_history_detail_is_single_record_bounded_and_survives_safe_refresh() {
        let mut initial = ShellSnapshot::gallery(GalleryScenario::Populated);
        initial.route = Route::History;
        let mut app = PhorminxUi::new(initial);
        let first = app.snapshot().history[0].id;
        let second = app.snapshot().history[1].id;

        app.select_history(first);
        app.pages.history_variant = HistoryVariant::Raw;
        assert!(app.set_history_detail(first, HistoryVariant::Raw, "exact raw 🦀".into()));
        assert!(app.history_detail_loaded(first, HistoryVariant::Raw));
        assert_eq!(
            app.snapshot()
                .history
                .iter()
                .filter(|item| item.loaded.is_some())
                .count(),
            1
        );

        let mut refreshed = ShellSnapshot::gallery(GalleryScenario::Populated);
        refreshed.route = Route::History;
        for item in &mut refreshed.history {
            item.loaded = None;
        }
        app.apply_snapshot(refreshed);
        assert_eq!(
            app.snapshot().history[0].text_for(HistoryVariant::Raw),
            Some("exact raw 🦀")
        );

        app.select_history(second);
        assert!(!app.set_history_detail(first, HistoryVariant::Raw, "late stale raw".into()));
        assert!(app.set_history_detail(second, HistoryVariant::Output, "second exact".into()));
        assert!(!app.history_detail_loaded(first, HistoryVariant::Raw));
        assert!(app.history_detail_loaded(second, HistoryVariant::Output));
        assert_eq!(
            app.snapshot()
                .history
                .iter()
                .filter(|item| item.loaded.is_some())
                .count(),
            1
        );
    }

    #[test]
    fn runtime_status_updates_preserve_history_and_loaded_exact_text() {
        let mut snapshot = ShellSnapshot::gallery(GalleryScenario::Populated);
        snapshot.route = Route::History;
        let mut app = PhorminxUi::new(snapshot);
        let selected = app.snapshot().history[0].id;
        app.select_history(selected);
        assert!(app.set_history_detail(
            selected,
            HistoryVariant::Output,
            "exact retained text".into()
        ));
        let before = app.snapshot().history.clone();

        app.set_runtime_status(crate::model::RuntimeStatus::Listening);

        assert_eq!(
            app.snapshot().status,
            crate::model::RuntimeStatus::Listening
        );
        assert_eq!(app.snapshot().history, before);
        assert_eq!(
            app.snapshot().history[0].text_for(HistoryVariant::Output),
            Some("exact retained text")
        );
    }

    #[test]
    fn refresh_drops_stale_detail_when_selected_record_disappears() {
        let mut app = PhorminxUi::new(ShellSnapshot::gallery(GalleryScenario::Populated));
        let selected = app.snapshot().history[1].id;
        app.select_history(selected);
        assert!(app.set_history_detail(selected, HistoryVariant::Output, "stale".into()));

        let mut refreshed = ShellSnapshot::gallery(GalleryScenario::Populated);
        refreshed.history.retain(|item| item.id != selected);
        for item in &mut refreshed.history {
            item.loaded = None;
        }
        app.apply_snapshot(refreshed);

        assert_ne!(app.history_selection().map(|(id, _)| id), Some(selected));
        assert!(
            app.snapshot()
                .history
                .iter()
                .all(|item| item.loaded.is_none())
        );
        assert!(!app.set_history_detail(selected, HistoryVariant::Output, "late".into()));
        assert!(
            app.snapshot()
                .history
                .iter()
                .all(|item| item.loaded.is_none())
        );
    }

    #[test]
    fn leaving_history_releases_exact_text_while_preserving_previews() {
        let mut snapshot = ShellSnapshot::gallery(GalleryScenario::Populated);
        snapshot.route = Route::History;
        for item in &mut snapshot.history {
            item.loaded = None;
        }
        let mut app = PhorminxUi::new(snapshot);
        let selected = app.snapshot().history[0].id;
        assert!(app.set_history_detail(
            selected,
            HistoryVariant::Output,
            "private exact text".into()
        ));

        app.navigate(Route::Home);

        assert!(
            app.snapshot()
                .history
                .iter()
                .all(|item| item.loaded.is_none())
        );
        assert!(!app.snapshot().history[0].output_preview.is_empty());
    }

    #[test]
    fn shell_paints_at_minimum_supported_size() {
        egui::__run_test_ui(|ui| {
            let mut app = PhorminxUi::new(ShellSnapshot::gallery(GalleryScenario::Populated));
            app.show(ui);
        });
    }

    #[test]
    fn every_theme_paints_the_complete_shell() {
        egui::__run_test_ui(|ui| {
            for mode in [
                ThemeMode::AuthoredDark,
                ThemeMode::AuthoredLight,
                ThemeMode::HighContrast,
                ThemeMode::HighContrastLight,
            ] {
                let mut app = PhorminxUi::new(ShellSnapshot::gallery(GalleryScenario::Populated));
                app.set_theme(mode);
                app.show(ui);
                assert_eq!(app.theme(), mode);
            }
        });
    }
}
