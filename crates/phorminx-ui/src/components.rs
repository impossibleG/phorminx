//! Reusable, product-specific egui components.

use eframe::egui::{
    self, Align, Button, Color32, CornerRadius, FontId, Image, Layout, Margin, Rect, Response,
    RichText, Sense, Stroke, StrokeKind, TextureHandle, TextureOptions, Ui, Vec2,
};

use crate::model::{InlineNotice, NoticeKind, Readiness, RuntimeStatus};
use crate::theme::{Space, UiThemeExt};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ActionTone {
    Primary,
    Secondary,
    Quiet,
    Destructive,
}

#[must_use]
pub fn action(ui: &mut Ui, label: &str, tone: ActionTone) -> Response {
    let tokens = ui.tokens();
    let (fill, stroke, text) = match tone {
        ActionTone::Primary => (tokens.accent, Stroke::NONE, tokens.on_accent),
        ActionTone::Secondary => (tokens.surface, Stroke::new(1.0, tokens.edge), tokens.text),
        ActionTone::Quiet => (Color32::TRANSPARENT, Stroke::NONE, tokens.secondary_text),
        ActionTone::Destructive => (
            Color32::TRANSPARENT,
            Stroke::new(1.0, tokens.destructive),
            tokens.destructive_text,
        ),
    };
    ui.add(
        Button::new(RichText::new(label).color(text).strong())
            .fill(fill)
            .stroke(stroke)
            .corner_radius(CornerRadius::same(6))
            .min_size(Vec2::new(0.0, 40.0)),
    )
}

pub fn tensioned_p(ui: &mut Ui, size: f32) -> Response {
    let texture = mark_texture(ui);
    ui.add(
        Image::new(&texture)
            .fit_to_exact_size(Vec2::splat(size))
            .tint(ui.tokens().text)
            .sense(Sense::hover()),
    )
    .on_hover_text("Phorminx · Voice, disciplined.")
}

fn mark_texture(ui: &Ui) -> TextureHandle {
    let texture_id = egui::Id::new("phorminx-production-mark");
    if let Some(texture) = ui
        .ctx()
        .data_mut(|data| data.get_temp::<TextureHandle>(texture_id))
    {
        return texture;
    }

    let icon = eframe::icon_data::from_png_bytes(include_bytes!(
        "../../../design/brand/png/mark/phorminx-mark-white-48.png"
    ))
    .expect("the embedded Phorminx mark must be a valid PNG");
    let image = egui::ColorImage::from_rgba_unmultiplied(
        [icon.width as usize, icon.height as usize],
        &icon.rgba,
    );
    let texture = ui
        .ctx()
        .load_texture("phorminx-production-mark", image, TextureOptions::LINEAR);
    ui.ctx()
        .data_mut(|data| data.insert_temp(texture_id, texture.clone()));
    texture
}

#[must_use]
pub fn nav_item(ui: &mut Ui, label: &str, selected: bool) -> Response {
    let tokens = ui.tokens();
    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, 40.0), Sense::hover());
    let response = ui.interact(rect, nav_id(ui, label), Sense::click());
    response.widget_info(|| nav_widget_info(label, selected));
    if ui.is_rect_visible(rect) {
        let fill = if selected || response.hovered() {
            tokens.raised
        } else {
            Color32::TRANSPARENT
        };
        ui.painter().rect_filled(rect, CornerRadius::same(6), fill);
        if selected {
            let indicator = Rect::from_min_max(
                rect.left_top(),
                egui::pos2(rect.left() + 2.0, rect.bottom()),
            );
            ui.painter().rect_filled(indicator, 1, tokens.accent);
        }
        ui.painter().text(
            egui::pos2(rect.left() + Space::MD, rect.center().y),
            egui::Align2::LEFT_CENTER,
            label,
            FontId::proportional(14.0),
            if selected {
                tokens.text
            } else {
                tokens.secondary_text
            },
        );
        if response.has_focus() {
            ui.painter().rect_stroke(
                rect.shrink(1.0),
                CornerRadius::same(6),
                Stroke::new(2.0, tokens.accent_focus),
                StrokeKind::Inside,
            );
            let focus_notch = Rect::from_min_max(
                egui::pos2(rect.right() - 7.0, rect.top() + 5.0),
                egui::pos2(rect.right() - 3.0, rect.bottom() - 5.0),
            );
            ui.painter()
                .rect_filled(focus_notch, 0, tokens.accent_focus);
        }
    }
    response
}

#[must_use]
pub fn nav_id(ui: &Ui, label: &str) -> egui::Id {
    ui.make_persistent_id(("phorminx-nav", label))
}

fn nav_widget_info(label: &str, selected: bool) -> egui::WidgetInfo {
    egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, selected, label)
}

pub fn page_header(ui: &mut Ui, title: &str, context: &str, action_label: Option<&str>) -> bool {
    let tokens = ui.tokens();
    let mut clicked = false;
    ui.horizontal(|ui| {
        ui.vertical(|ui| {
            let heading = ui.add(
                egui::Label::new(RichText::new(title).size(28.0).color(tokens.text))
                    .sense(Sense::focusable_noninteractive()),
            );
            heading.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, title));
            if ui
                .ctx()
                .data_mut(|data| data.remove_temp::<bool>(page_header_focus_id()))
                .unwrap_or(false)
            {
                heading.request_focus();
            }
            ui.add_space(Space::XXS);
            ui.label(
                RichText::new(context)
                    .size(14.0)
                    .color(tokens.secondary_text),
            );
        });
        if let Some(label) = action_label {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                clicked = action(ui, label, ActionTone::Primary).clicked();
            });
        }
    });
    ui.add_space(Space::LG);
    hairline(ui);
    ui.add_space(Space::LG);
    clicked
}

pub fn request_page_header_focus(ctx: &egui::Context) {
    ctx.data_mut(|data| data.insert_temp(page_header_focus_id(), true));
}

fn page_header_focus_id() -> egui::Id {
    egui::Id::new("phorminx-page-header-focus")
}

pub fn hairline(ui: &mut Ui) {
    let tokens = ui.tokens();
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 1.0), Sense::hover());
    ui.painter().line_segment(
        [rect.left_center(), rect.right_center()],
        Stroke::new(1.0, tokens.edge),
    );
}

pub fn metadata(ui: &mut Ui, text: &str) {
    let tokens = ui.tokens();
    ui.label(
        RichText::new(text.to_uppercase())
            .size(10.0)
            .color(tokens.secondary_text)
            .strong(),
    );
}

pub fn shortcut_chord(ui: &mut Ui, chord: &str) {
    let tokens = ui.tokens();
    let frame = egui::Frame::new()
        .fill(tokens.raised)
        .stroke(Stroke::new(1.0, tokens.edge))
        .inner_margin(Margin::symmetric(10, 5))
        .corner_radius(CornerRadius::same(6));
    frame.show(ui, |ui| {
        ui.label(
            RichText::new(chord)
                .monospace()
                .size(12.0)
                .color(tokens.text),
        );
    });
}

pub fn status_seal(ui: &mut Ui, status: RuntimeStatus) {
    let tokens = ui.tokens();
    let color = match status {
        RuntimeStatus::Ready | RuntimeStatus::Inserted | RuntimeStatus::Copied => tokens.verified,
        RuntimeStatus::Listening | RuntimeStatus::Transcribing | RuntimeStatus::Refining => {
            tokens.accent_focus
        }
        RuntimeStatus::NeedsAttention => tokens.destructive,
    };
    ui.horizontal(|ui| {
        let (dot, _) = ui.allocate_exact_size(Vec2::splat(8.0), Sense::hover());
        ui.painter().circle_filled(dot.center(), 3.0, color);
        ui.label(
            RichText::new("Local · ")
                .size(12.0)
                .color(tokens.secondary_text),
        );
        ui.label(RichText::new(status.label()).size(12.0).color(tokens.text));
    });
}

pub fn readiness_row(ui: &mut Ui, name: &str, detail: &str, state: Readiness) {
    let tokens = ui.tokens();
    let (symbol, color) = match state {
        Readiness::Ready => ("●", tokens.verified),
        Readiness::Working => ("◐", tokens.accent_focus),
        Readiness::Optional => ("○", tokens.secondary_text),
        Readiness::Unavailable => ("—", tokens.secondary_text),
        Readiness::Error => ("!", tokens.destructive),
    };
    ui.horizontal(|ui| {
        ui.set_height(36.0);
        ui.label(RichText::new(symbol).color(color).size(13.0));
        ui.label(RichText::new(name).color(tokens.text).strong());
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.label(
                RichText::new(detail)
                    .color(tokens.secondary_text)
                    .size(13.0),
            );
        });
    });
}

pub fn empty_state(ui: &mut Ui, title: &str, detail: &str, action_label: Option<&str>) -> bool {
    let tokens = ui.tokens();
    let mut clicked = false;
    ui.add_space(Space::XXL);
    ui.vertical_centered(|ui| {
        let (line, _) = ui.allocate_exact_size(Vec2::new(48.0, 2.0), Sense::hover());
        ui.painter().rect_filled(line, 1, tokens.accent);
        ui.add_space(Space::MD);
        ui.label(RichText::new(title).size(20.0).color(tokens.text));
        ui.add_space(Space::XS);
        ui.label(
            RichText::new(detail)
                .size(14.0)
                .color(tokens.secondary_text),
        );
        if let Some(label) = action_label {
            ui.add_space(Space::LG);
            clicked = action(ui, label, ActionTone::Secondary).clicked();
        }
    });
    clicked
}

pub fn inline_notice(ui: &mut Ui, notice: &InlineNotice) -> (bool, bool) {
    let tokens = ui.tokens();
    let accent = match notice.kind {
        NoticeKind::Information => tokens.accent,
        NoticeKind::Warning => tokens.accent_focus,
        NoticeKind::Error => tokens.destructive,
    };
    let mut action_clicked = false;
    let mut dismissed = false;
    egui::Frame::new()
        .fill(tokens.surface)
        .stroke(Stroke::new(1.0, accent))
        .inner_margin(Margin::same(16))
        .corner_radius(CornerRadius::same(6))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.label(RichText::new(&notice.title).color(tokens.text).strong());
                    ui.label(RichText::new(&notice.detail).color(tokens.secondary_text));
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    dismissed = action(ui, "Dismiss", ActionTone::Quiet).clicked();
                    if let Some(label) = &notice.action {
                        action_clicked = action(ui, label, ActionTone::Secondary).clicked();
                    }
                });
            });
        });
    (action_clicked, dismissed)
}

pub fn section_title(ui: &mut Ui, index: &str, title: &str, detail: &str) {
    let tokens = ui.tokens();
    ui.horizontal(|ui| {
        ui.label(RichText::new(index).monospace().color(tokens.accent_focus));
        ui.vertical(|ui| {
            ui.label(RichText::new(title).size(17.0).color(tokens.text));
            ui.label(
                RichText::new(detail)
                    .size(13.0)
                    .color(tokens.secondary_text),
            );
        });
    });
}

pub fn segmented<T: Copy + Eq>(
    ui: &mut Ui,
    values: impl IntoIterator<Item = T>,
    selected: &mut T,
    label: impl Fn(T) -> &'static str,
) -> Option<T> {
    let tokens = ui.tokens();
    let mut changed = None;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        for value in values {
            let active = *selected == value;
            let response = ui.add(
                Button::new(RichText::new(label(value)).size(12.0).color(if active {
                    tokens.text
                } else {
                    tokens.secondary_text
                }))
                .selected(active)
                .fill(if active {
                    tokens.raised
                } else {
                    tokens.surface
                })
                .stroke(Stroke::new(1.0, tokens.edge))
                .corner_radius(CornerRadius::ZERO)
                .min_size(Vec2::new(86.0, 32.0)),
            );
            if response.clicked() {
                *selected = value;
                changed = Some(value);
            }
        }
    });
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gallery_components_paint_without_panicking() {
        egui::__run_test_ui(|ui| {
            tensioned_p(ui, 40.0);
            let _ = nav_item(ui, "History", true);
            page_header(
                ui,
                "Recovered thought",
                "Compare what was spoken.",
                Some("Copy"),
            );
            status_seal(ui, RuntimeStatus::Ready);
            readiness_row(ui, "Whisper", "base.en · verified", Readiness::Ready);
            empty_state(ui, "Nothing held", "History is local.", None);
        });
    }

    #[test]
    fn navigation_exposes_name_role_and_selected_state() {
        let selected = nav_widget_info("History", true);
        assert_eq!(selected.typ, egui::WidgetType::SelectableLabel);
        assert_eq!(selected.label.as_deref(), Some("History"));
        assert_eq!(selected.selected, Some(true));

        let idle = nav_widget_info("Models", false);
        assert_eq!(idle.label.as_deref(), Some("Models"));
        assert_eq!(idle.selected, Some(false));
    }

    #[test]
    fn focused_navigation_activates_with_keyboard() {
        let context = egui::Context::default();
        let mut clicked = false;
        let first = context.run_ui(egui::RawInput::default(), |ui| {
            nav_item(ui, "History", false).request_focus();
        });
        first.drop_without_applying_deltas();

        let mut input = egui::RawInput::default();
        input.events.push(egui::Event::Key {
            key: egui::Key::Enter,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        });
        let second = context.run_ui(input, |ui| {
            clicked = nav_item(ui, "History", false).clicked();
        });
        second.drop_without_applying_deltas();
        assert!(clicked);
    }
}
