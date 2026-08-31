//! Static proof surface for the design system.

use eframe::egui::{self, Color32, RichText, Stroke, Ui, Vec2};

use crate::components::{
    self, ActionTone, action, empty_state, hairline, metadata, readiness_row, status_seal,
};
use crate::model::{GalleryScenario, InlineNotice, NoticeKind, Readiness, RuntimeStatus};
use crate::theme::{Space, UiThemeExt};

#[derive(Clone, Debug, Default)]
pub struct ComponentGallery {
    scenario: GalleryScenario,
}

impl ComponentGallery {
    #[must_use]
    pub const fn scenario(&self) -> GalleryScenario {
        self.scenario
    }

    pub fn set_scenario(&mut self, scenario: GalleryScenario) {
        self.scenario = scenario;
    }

    pub fn show(&mut self, ui: &mut Ui) {
        let tokens = ui.tokens();
        ui.horizontal(|ui| {
            ui.label(
                RichText::new("COMPONENT PROOF")
                    .size(10.0)
                    .strong()
                    .color(tokens.secondary_text),
            );
            ui.add_space(Space::MD);
            for scenario in GalleryScenario::ALL {
                ui.selectable_value(&mut self.scenario, scenario, scenario.label());
            }
        });
        ui.add_space(Space::LG);
        token_strip(ui);
        ui.add_space(Space::XL);
        hairline(ui);
        ui.add_space(Space::XL);

        metadata(ui, "Identity");
        ui.add_space(Space::MD);
        ui.horizontal(|ui| {
            components::tensioned_p(ui, 48.0);
            ui.vertical(|ui| {
                ui.label(
                    RichText::new("PHORMINX")
                        .size(20.0)
                        .strong()
                        .color(tokens.text),
                );
                ui.label(RichText::new("Voice, disciplined.").color(tokens.secondary_text));
            });
            ui.add_space(Space::XL);
            status_seal(
                ui,
                if self.scenario == GalleryScenario::Error {
                    RuntimeStatus::NeedsAttention
                } else {
                    RuntimeStatus::Ready
                },
            );
        });

        ui.add_space(Space::XL);
        metadata(ui, "Actions");
        ui.add_space(Space::SM);
        ui.horizontal(|ui| {
            let _ = action(ui, "Test dictation", ActionTone::Primary);
            let _ = action(ui, "Change model", ActionTone::Secondary);
            let _ = action(ui, "Dismiss", ActionTone::Quiet);
            let _ = action(ui, "Delete", ActionTone::Destructive);
            ui.add_enabled(false, egui::Button::new("Unavailable"));
        });

        ui.add_space(Space::XL);
        metadata(ui, "Readiness");
        ui.add_space(Space::SM);
        readiness_row(ui, "Whisper", "base.en · verified", Readiness::Ready);
        readiness_row(ui, "Ollama", "Loading local model", Readiness::Working);
        readiness_row(ui, "Formatting", "Optional", Readiness::Optional);

        ui.add_space(Space::XL);
        match self.scenario {
            GalleryScenario::Empty => {
                empty_state(
                    ui,
                    "Nothing held",
                    "Completed dictations will gather here.",
                    None,
                );
            }
            GalleryScenario::Populated => {
                metadata(ui, "Selected transcript");
                ui.add_space(Space::SM);
                ui.label(
                    RichText::new(
                        "The quieter the interface, the more exact each decision must be.",
                    )
                    .size(21.0)
                    .color(tokens.text),
                );
            }
            GalleryScenario::Error => {
                let notice = InlineNotice {
                    kind: NoticeKind::Error,
                    title: "Local refinement unavailable".into(),
                    detail: "Start Ollama or continue with Light output.".into(),
                    action: Some("Check again".into()),
                };
                components::inline_notice(ui, &notice);
            }
        }
    }
}

fn token_strip(ui: &mut Ui) {
    let palette = ui.tokens();
    let tokens = [
        ("Background", palette.background),
        ("Surface", palette.surface),
        ("Raised", palette.raised),
        ("Edge", palette.edge),
        ("Text", palette.text),
        ("Secondary", palette.secondary_text),
        ("Accent", palette.accent),
        ("Destructive", palette.destructive),
        ("Verified", palette.verified),
    ];
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        for (name, color) in tokens {
            let (rect, response) =
                ui.allocate_exact_size(Vec2::new(58.0, 24.0), egui::Sense::hover());
            ui.painter().rect(
                rect,
                0,
                color,
                Stroke::new(1.0, Color32::from_white_alpha(30)),
                egui::StrokeKind::Inside,
            );
            response.on_hover_text(name);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gallery_paints_every_scenario() {
        egui::__run_test_ui(|ui| {
            let mut gallery = ComponentGallery::default();
            for scenario in GalleryScenario::ALL {
                gallery.set_scenario(scenario);
                gallery.show(ui);
            }
        });
    }
}
