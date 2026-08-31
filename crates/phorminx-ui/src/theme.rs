//! Phorminx visual tokens: disciplined monochrome with bronze used as tension.

use eframe::egui::{self, Color32, FontFamily, FontId, Stroke, TextStyle, Vec2};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ThemeMode {
    #[default]
    AuthoredDark,
    HighContrast,
}

pub struct Colors;

impl Colors {
    pub const ABYSS: Color32 = Color32::from_rgb(10, 12, 13);
    pub const IRON: Color32 = Color32::from_rgb(18, 22, 25);
    pub const TEMPERED: Color32 = Color32::from_rgb(27, 32, 36);
    pub const EDGE: Color32 = Color32::from_rgb(48, 54, 59);
    pub const LIMESTONE: Color32 = Color32::from_rgb(232, 229, 221);
    pub const ASH: Color32 = Color32::from_rgb(168, 173, 176);
    pub const BRONZE: Color32 = Color32::from_rgb(168, 117, 66);
    pub const BRONZE_LIGHT: Color32 = Color32::from_rgb(208, 160, 106);
    pub const OXBLOOD: Color32 = Color32::from_rgb(139, 59, 62);
    pub const MOSS: Color32 = Color32::from_rgb(113, 129, 108);
}

pub struct Space;

impl Space {
    pub const XXS: f32 = 4.0;
    pub const XS: f32 = 8.0;
    pub const SM: f32 = 12.0;
    pub const MD: f32 = 16.0;
    pub const LG: f32 = 24.0;
    pub const XL: f32 = 32.0;
    pub const XXL: f32 = 48.0;
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ThemeTokens {
    pub background: Color32,
    pub surface: Color32,
    pub raised: Color32,
    pub edge: Color32,
    pub text: Color32,
    pub secondary_text: Color32,
    pub accent: Color32,
    pub accent_focus: Color32,
    pub destructive: Color32,
    pub verified: Color32,
}

impl ThemeTokens {
    #[must_use]
    pub const fn for_mode(mode: ThemeMode) -> Self {
        match mode {
            ThemeMode::AuthoredDark => Self {
                background: Colors::ABYSS,
                surface: Colors::IRON,
                raised: Colors::TEMPERED,
                edge: Colors::EDGE,
                text: Colors::LIMESTONE,
                secondary_text: Colors::ASH,
                accent: Colors::BRONZE,
                accent_focus: Colors::BRONZE_LIGHT,
                destructive: Colors::OXBLOOD,
                verified: Colors::MOSS,
            },
            ThemeMode::HighContrast => Self {
                background: Color32::BLACK,
                surface: Color32::BLACK,
                raised: Color32::from_gray(28),
                edge: Color32::WHITE,
                text: Color32::WHITE,
                secondary_text: Color32::from_gray(224),
                accent: Color32::from_rgb(255, 196, 124),
                accent_focus: Color32::WHITE,
                destructive: Color32::from_rgb(255, 118, 122),
                verified: Color32::from_rgb(182, 224, 174),
            },
        }
    }
}

#[must_use]
pub fn style(mode: ThemeMode) -> egui::Style {
    let tokens = ThemeTokens::for_mode(mode);
    let mut style = egui::Style {
        visuals: egui::Visuals::dark(),
        ..egui::Style::default()
    };
    style.visuals.panel_fill = tokens.background;
    style.visuals.window_fill = tokens.surface;
    style.visuals.faint_bg_color = tokens.surface;
    style.visuals.extreme_bg_color = tokens.background;
    style.visuals.override_text_color = Some(tokens.text);
    style.visuals.selection.bg_fill = tokens.accent;
    style.visuals.selection.stroke = Stroke::new(1.0, tokens.accent_focus);
    style.visuals.hyperlink_color = tokens.accent_focus;
    style.visuals.widgets.noninteractive.bg_fill = tokens.surface;
    style.visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, tokens.edge);
    style.visuals.widgets.inactive.weak_bg_fill = tokens.surface;
    style.visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, tokens.edge);
    style.visuals.widgets.hovered.weak_bg_fill = tokens.raised;
    style.visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, tokens.accent);
    style.visuals.widgets.active.weak_bg_fill = tokens.raised;
    style.visuals.widgets.active.bg_stroke = Stroke::new(1.0, tokens.accent_focus);
    style.visuals.widgets.open.weak_bg_fill = tokens.raised;
    style.visuals.widgets.open.bg_stroke = Stroke::new(1.0, tokens.accent);
    style.visuals.window_stroke = Stroke::new(1.0, tokens.edge);
    style.visuals.menu_corner_radius = 6.into();
    style.visuals.window_corner_radius = 10.into();
    style.spacing.item_spacing = Vec2::new(Space::SM, Space::SM);
    style.spacing.interact_size = Vec2::new(40.0, 36.0);
    style.spacing.button_padding = Vec2::new(Space::MD, Space::XS);
    style.text_styles.insert(
        TextStyle::Heading,
        FontId::new(28.0, FontFamily::Proportional),
    );
    style
        .text_styles
        .insert(TextStyle::Body, FontId::new(15.0, FontFamily::Proportional));
    style.text_styles.insert(
        TextStyle::Button,
        FontId::new(14.0, FontFamily::Proportional),
    );
    style.text_styles.insert(
        TextStyle::Monospace,
        FontId::new(13.0, FontFamily::Monospace),
    );
    style.text_styles.insert(
        TextStyle::Small,
        FontId::new(12.0, FontFamily::Proportional),
    );
    style
}

pub fn apply(ctx: &egui::Context, mode: ThemeMode) {
    ctx.set_theme(egui::Theme::Dark);
    ctx.set_style_of(egui::Theme::Dark, style(mode));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn channel(value: u8) -> f32 {
        let value = f32::from(value) / 255.0;
        if value <= 0.040_45 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    }

    fn luminance(color: Color32) -> f32 {
        0.2126 * channel(color.r()) + 0.7152 * channel(color.g()) + 0.0722 * channel(color.b())
    }

    fn contrast(first: Color32, second: Color32) -> f32 {
        let (bright, dark) = if luminance(first) > luminance(second) {
            (luminance(first), luminance(second))
        } else {
            (luminance(second), luminance(first))
        };
        (bright + 0.05) / (dark + 0.05)
    }

    #[test]
    fn authored_text_exceeds_wcag_normal_text_contrast() {
        let tokens = ThemeTokens::for_mode(ThemeMode::AuthoredDark);
        assert!(contrast(tokens.text, tokens.background) >= 7.0);
        assert!(contrast(tokens.secondary_text, tokens.background) >= 4.5);
    }

    #[test]
    fn high_contrast_keeps_structural_edges_visible() {
        let tokens = ThemeTokens::for_mode(ThemeMode::HighContrast);
        assert!(contrast(tokens.edge, tokens.background) >= 7.0);
    }
}
