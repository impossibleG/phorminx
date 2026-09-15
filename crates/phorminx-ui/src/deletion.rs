//! Explicit, category-scoped deletion. Opening this UI never selects anything.
use crate::{
    ShellEvent,
    components::{ActionTone, action},
    workspace::{WorkspaceEvent, WorkspaceSnapshot},
};
use eframe::egui::{self, RichText, Ui};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct Selection {
    dictations: bool,
    meeting_transcripts: bool,
    chats: bool,
}
impl Selection {
    fn labels(self) -> Vec<&'static str> {
        [
            (self.dictations, "Dictation history"),
            (self.meeting_transcripts, "Meeting transcripts"),
            (self.chats, "AI chats"),
        ]
        .into_iter()
        .filter_map(|(selected, label)| selected.then_some(label))
        .collect()
    }
}
#[derive(Clone, Debug, Default)]
pub(crate) struct DeletionState {
    selection: Selection,
    review: Option<Selection>,
}

pub(crate) fn show(
    ui: &mut Ui,
    snapshot: &WorkspaceSnapshot,
    state: &mut DeletionState,
    out: &mut Vec<ShellEvent>,
) {
    ui.heading("Delete saved data");
    ui.label("Choose exactly what to remove. Models, credentials and settings are never included.");
    let changed = ui
        .checkbox(&mut state.selection.dictations, "Dictation history")
        .changed()
        | ui.checkbox(
            &mut state.selection.meeting_transcripts,
            "Meeting transcripts",
        )
        .changed()
        | ui.checkbox(&mut state.selection.chats, "AI chats")
            .changed();
    if changed {
        state.review = None;
    }
    ui.label("AI chats include submitted transcript excerpts. Delete both categories if you want those copies removed too. Unsaved HTTP note drafts are not included.");
    ui.add_enabled_ui(state.selection != Selection::default(), |ui| {
        if action(ui, "Review deletion", ActionTone::Secondary).clicked() {
            state.review = Some(state.selection);
        }
    });
    if let Some(selection) = state.review {
        egui::Frame::group(ui.style()).show(ui,|ui| {
            ui.label(RichText::new(format!("Permanently delete all saved items in: {}?",selection.labels().join(", "))).strong());
            ui.label("This cannot be undone. Unselected categories will stay. Active meeting recording or AI responses must be stopped before deleting their data.");
            ui.horizontal_wrapped(|ui| {
                if action(ui,"Delete selected data",ActionTone::Destructive).clicked(){
                    out.push(ShellEvent::Workspace(WorkspaceEvent::DeleteSelected{dictations:selection.dictations,meeting_transcripts:selection.meeting_transcripts,chats:selection.chats}));
                    state.review=None;
                    state.selection=Selection::default();
                }
                if action(ui,"Cancel",ActionTone::Quiet).clicked(){state.review=None;}
            });
        });
    }
    if snapshot.deletion_revision > 0 {
        ui.label(&snapshot.deletion_notice);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn paint(
        ctx: &egui::Context,
        state: &mut DeletionState,
        out: &mut Vec<ShellEvent>,
        events: Vec<egui::Event>,
    ) -> egui::FullOutput {
        for theme in [egui::Theme::Light, egui::Theme::Dark] {
            ctx.style_mut_of(theme, |style| style.animation_time = 0.0);
        }
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1100.0, 1400.0),
                )),
                events,
                ..Default::default()
            },
            |ui| show(ui, &WorkspaceSnapshot::default(), state, out),
        );
        output.textures_delta.clear();
        output
    }
    fn click(
        ctx: &egui::Context,
        state: &mut DeletionState,
        out: &mut Vec<ShellEvent>,
        label: &str,
    ) {
        let frame = paint(ctx, state, out, vec![]);
        let pos = frame
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::epaint::Shape::Text(text) if text.galley.text() == label => {
                    Some(text.pos + text.galley.size() / 2.0)
                }
                _ => None,
            })
            .unwrap_or_else(|| panic!("Missing control: {label}"));
        for pressed in [true, false] {
            paint(
                ctx,
                state,
                out,
                vec![
                    egui::Event::PointerMoved(pos),
                    egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: Default::default(),
                    },
                ],
            );
        }
    }
    #[test]
    fn actual_controls_require_selection_review_and_confirmation() {
        let ctx = egui::Context::default();
        let mut state = DeletionState::default();
        let mut out = vec![];
        click(&ctx, &mut state, &mut out, "Review deletion");
        assert!(state.review.is_none());
        click(&ctx, &mut state, &mut out, "AI chats");
        assert!(out.is_empty());
        click(&ctx, &mut state, &mut out, "Review deletion");
        assert!(state.review.is_some());
        assert!(out.is_empty());
        click(&ctx, &mut state, &mut out, "Cancel");
        assert!(state.review.is_none());
        assert!(out.is_empty());
        click(&ctx, &mut state, &mut out, "Review deletion");
        click(&ctx, &mut state, &mut out, "Delete selected data");
        assert_eq!(
            out,
            vec![ShellEvent::Workspace(WorkspaceEvent::DeleteSelected {
                dictations: false,
                meeting_transcripts: false,
                chats: true
            })]
        );
        assert_eq!(state.selection, Selection::default());
        assert!(state.review.is_none());
    }
    #[test]
    fn changing_selection_invalidates_the_previous_confirmation() {
        let ctx = egui::Context::default();
        let mut state = DeletionState::default();
        let mut out = vec![];
        click(&ctx, &mut state, &mut out, "Meeting transcripts");
        click(&ctx, &mut state, &mut out, "Review deletion");
        assert!(state.review.is_some());
        click(&ctx, &mut state, &mut out, "AI chats");
        assert!(state.review.is_none());
        assert!(out.is_empty());
    }
    #[test]
    fn selection_is_empty_and_opening_does_not_emit_deletion() {
        let mut state = DeletionState::default();
        let mut out = vec![];
        egui::__run_test_ui(|ui| show(ui, &WorkspaceSnapshot::default(), &mut state, &mut out));
        assert_eq!(state.selection, Selection::default());
        assert!(state.review.is_none());
        assert!(out.is_empty());
    }
    #[test]
    fn selection_labels_never_include_unselected_categories() {
        for bits in 0..8 {
            let selection = Selection {
                dictations: bits & 1 != 0,
                meeting_transcripts: bits & 2 != 0,
                chats: bits & 4 != 0,
            };
            assert_eq!(selection.labels().len(), (bits as u8).count_ones() as usize);
            assert_eq!(selection.labels().contains(&"AI chats"), bits & 4 != 0);
        }
    }
}
