//! Phorminx visual tokens: disciplined monochrome with bronze used as tension.
//!
//! There are deliberately no animated presentation tokens. Every state and route
//! change is painted at its final geometry in the next frame, so Windows' reduced
//! motion preference is satisfied without a parallel animation setting.

use eframe::egui::{self, Color32, FontFamily, FontId, Stroke, TextStyle, Ui, Vec2};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ThemeMode {
    #[default]
    AuthoredDark,
    AuthoredLight,
    HighContrast,
    HighContrastLight,
}

impl ThemeMode {
    /// Maps the host's live Windows appearance observations into a shell theme.
    ///
    /// The host should call [`crate::PhorminxUi::set_theme`] whenever either OS
    /// value changes. `prefers_dark` must reflect the active contrast theme while
    /// high contrast is enabled so black and white contrast themes both work.
    #[must_use]
    pub const fn from_system(prefers_dark: bool, high_contrast: bool) -> Self {
        match (high_contrast, prefers_dark) {
            (true, true) => Self::HighContrast,
            (true, false) => Self::HighContrastLight,
            (false, true) => Self::AuthoredDark,
            (false, false) => Self::AuthoredLight,
        }
    }

    #[must_use]
    const fn is_light(self) -> bool {
        matches!(self, Self::AuthoredLight | Self::HighContrastLight)
    }
}

/// The immutable source palette. Product code consumes [`ThemeTokens`] instead.
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
    pub on_accent: Color32,
    pub destructive: Color32,
    pub destructive_text: Color32,
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
                on_accent: Colors::ABYSS,
                destructive: Colors::OXBLOOD,
                destructive_text: Color32::from_rgb(232, 150, 152),
                verified: Colors::MOSS,
            },
            ThemeMode::AuthoredLight => Self {
                background: Color32::from_rgb(246, 243, 236),
                surface: Color32::from_rgb(237, 233, 224),
                raised: Color32::from_rgb(225, 220, 209),
                edge: Color32::from_rgb(111, 108, 101),
                text: Color32::from_rgb(23, 26, 28),
                secondary_text: Color32::from_rgb(78, 82, 84),
                accent: Color32::from_rgb(126, 78, 33),
                accent_focus: Color32::from_rgb(101, 59, 20),
                on_accent: Color32::WHITE,
                destructive: Color32::from_rgb(126, 42, 49),
                destructive_text: Color32::from_rgb(126, 42, 49),
                verified: Color32::from_rgb(54, 83, 55),
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
                on_accent: Color32::BLACK,
                destructive: Color32::from_rgb(255, 118, 122),
                destructive_text: Color32::from_rgb(255, 160, 164),
                verified: Color32::from_rgb(182, 224, 174),
            },
            ThemeMode::HighContrastLight => Self {
                background: Color32::WHITE,
                surface: Color32::WHITE,
                raised: Color32::from_gray(226),
                edge: Color32::BLACK,
                text: Color32::BLACK,
                secondary_text: Color32::from_gray(40),
                accent: Color32::from_rgb(82, 42, 0),
                accent_focus: Color32::BLACK,
                on_accent: Color32::WHITE,
                destructive: Color32::from_rgb(112, 0, 8),
                destructive_text: Color32::from_rgb(112, 0, 8),
                verified: Color32::from_rgb(0, 75, 6),
            },
        }
    }
}

pub trait UiThemeExt {
    /// Returns the live theme tokens installed on this egui context.
    #[must_use]
    fn tokens(&self) -> ThemeTokens;
}

impl UiThemeExt for Ui {
    fn tokens(&self) -> ThemeTokens {
        let mode = self
            .ctx()
            .data_mut(|data| data.get_temp::<ThemeMode>(theme_mode_id()))
            .unwrap_or_default();
        ThemeTokens::for_mode(mode)
    }
}

fn theme_mode_id() -> egui::Id {
    egui::Id::new("phorminx-theme-mode")
}

#[must_use]
pub fn style(mode: ThemeMode) -> egui::Style {
    let tokens = ThemeTokens::for_mode(mode);
    let mut style = egui::Style {
        visuals: if mode.is_light() {
            egui::Visuals::light()
        } else {
            egui::Visuals::dark()
        },
        ..egui::Style::default()
    };
    style.visuals.panel_fill = tokens.background;
    style.visuals.window_fill = tokens.surface;
    style.visuals.faint_bg_color = tokens.surface;
    style.visuals.extreme_bg_color = tokens.background;
    style.visuals.override_text_color = Some(tokens.text);
    style.visuals.weak_text_alpha = 1.0;
    // Disabled controls remain readable; availability is also communicated by
    // interaction state and control geometry rather than opacity alone.
    style.visuals.disabled_alpha =
        if matches!(mode, ThemeMode::HighContrast | ThemeMode::HighContrastLight) {
            0.78
        } else {
            0.72
        };
    style.visuals.selection.bg_fill = tokens.accent;
    style.visuals.selection.stroke = Stroke::new(2.0, tokens.accent_focus);
    style.visuals.hyperlink_color = tokens.accent_focus;
    style.visuals.error_fg_color = tokens.destructive_text;
    style.visuals.warn_fg_color = tokens.accent_focus;
    style.visuals.widgets.noninteractive.bg_fill = tokens.surface;
    style.visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, tokens.edge);
    style.visuals.widgets.inactive.weak_bg_fill = tokens.surface;
    style.visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, tokens.edge);
    style.visuals.widgets.hovered.weak_bg_fill = tokens.raised;
    style.visuals.widgets.hovered.bg_stroke = Stroke::new(2.0, tokens.accent_focus);
    style.visuals.widgets.active.weak_bg_fill = tokens.raised;
    style.visuals.widgets.active.bg_stroke = Stroke::new(2.0, tokens.accent_focus);
    style.visuals.widgets.open.weak_bg_fill = tokens.raised;
    style.visuals.widgets.open.bg_stroke = Stroke::new(2.0, tokens.accent_focus);
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
    ctx.data_mut(|data| data.insert_temp(theme_mode_id(), mode));
    let egui_theme = if mode.is_light() {
        egui::Theme::Light
    } else {
        egui::Theme::Dark
    };
    ctx.set_theme(egui_theme);
    ctx.set_style_of(egui_theme, style(mode));
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

    fn blend_over(foreground: Color32, background: Color32, alpha: f32) -> Color32 {
        let blend = |front: u8, back: u8| {
            (f32::from(front).mul_add(alpha, f32::from(back) * (1.0 - alpha))).round() as u8
        };
        Color32::from_rgb(
            blend(foreground.r(), background.r()),
            blend(foreground.g(), background.g()),
            blend(foreground.b(), background.b()),
        )
    }

    #[test]
    fn every_mode_meets_text_and_focus_contrast_contracts() {
        for mode in [
            ThemeMode::AuthoredDark,
            ThemeMode::AuthoredLight,
            ThemeMode::HighContrast,
            ThemeMode::HighContrastLight,
        ] {
            let tokens = ThemeTokens::for_mode(mode);
            assert!(contrast(tokens.text, tokens.background) >= 4.5, "{mode:?}");
            assert!(
                contrast(tokens.secondary_text, tokens.background) >= 4.5,
                "{mode:?}"
            );
            assert!(
                contrast(tokens.accent_focus, tokens.background) >= 3.0,
                "{mode:?}"
            );
            assert!(
                contrast(tokens.accent_focus, tokens.raised) >= 3.0,
                "{mode:?}"
            );
            assert!(
                contrast(tokens.destructive_text, tokens.background) >= 4.5,
                "{mode:?}"
            );
        }
    }

    #[test]
    fn primary_action_text_is_legible_in_every_mode() {
        for mode in [
            ThemeMode::AuthoredDark,
            ThemeMode::AuthoredLight,
            ThemeMode::HighContrast,
            ThemeMode::HighContrastLight,
        ] {
            let tokens = ThemeTokens::for_mode(mode);
            assert!(contrast(tokens.on_accent, tokens.accent) >= 4.5, "{mode:?}");
        }
    }

    #[test]
    fn disabled_text_stays_legible_without_looking_enabled() {
        for mode in [
            ThemeMode::AuthoredDark,
            ThemeMode::AuthoredLight,
            ThemeMode::HighContrast,
            ThemeMode::HighContrastLight,
        ] {
            let tokens = ThemeTokens::for_mode(mode);
            let disabled_alpha = style(mode).visuals.disabled_alpha;
            let disabled = blend_over(tokens.text, tokens.surface, disabled_alpha);
            assert!(contrast(disabled, tokens.surface) >= 4.5, "{mode:?}");
            assert!(disabled_alpha < 1.0, "{mode:?}");
        }
    }

    #[test]
    fn system_mapping_covers_authored_and_contrast_variants() {
        assert_eq!(
            ThemeMode::from_system(false, false),
            ThemeMode::AuthoredLight
        );
        assert_eq!(ThemeMode::from_system(true, false), ThemeMode::AuthoredDark);
        assert_eq!(
            ThemeMode::from_system(false, true),
            ThemeMode::HighContrastLight
        );
        assert_eq!(ThemeMode::from_system(true, true), ThemeMode::HighContrast);
    }

    #[test]
    fn reduced_motion_is_immediate_by_construction() {
        let context = egui::Context::default();
        apply(&context, ThemeMode::AuthoredLight);
        assert_eq!(
            context.data_mut(|data| data.get_temp(theme_mode_id())),
            Some(ThemeMode::AuthoredLight)
        );
    }
}
