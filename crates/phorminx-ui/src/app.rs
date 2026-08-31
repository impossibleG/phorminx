use eframe::egui::{self, Align, Color32, Layout, Margin, RichText, ScrollArea, Stroke, Ui, Vec2};

use crate::components::{
    hairline, inline_notice, nav_id, nav_item, request_page_header_focus, shortcut_chord,
    status_seal, tensioned_p,
};
use crate::model::{Route, ShellEvent, ShellSnapshot};
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
        self.route = snapshot.route;
        self.pages.reconcile(&snapshot);
        self.snapshot = snapshot;
    }

    pub fn navigate(&mut self, route: Route) {
        if self.route != route {
            self.route = route;
            self.outbox.push(ShellEvent::Navigate(route));
        }
    }

    pub fn set_theme(&mut self, theme: ThemeMode) {
        if self.theme != theme {
            self.theme = theme;
            self.theme_applied = false;
        }
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
        if self.snapshot.history.iter().any(|item| item.id == id) {
            self.pages.history_id = Some(id);
            self.pages.history_variant = crate::model::HistoryVariant::Output;
        }
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
        assert_eq!(adjacent_route(0, 1), Route::History);
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
