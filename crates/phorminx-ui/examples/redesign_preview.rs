//! Native-renderer visual QA. Synthetic data only; no runtime, network or clipboard.
//! Usage: redesign_preview [--companion|--actions] [--dark] [--small] [--capture=PATH]
use eframe::egui;
use phorminx_ui::theme::ThemeMode;
use phorminx_ui::workspace::*;
use phorminx_ui::{GalleryScenario, PhorminxUi, Route, ShellSnapshot};

struct Preview {
    shell: PhorminxUi,
    companion: bool,
    capture: Option<String>,
    frames: u32,
}
impl Preview {
    fn new(args: &[String]) -> Self {
        let mut snapshot = ShellSnapshot::gallery(GalleryScenario::Populated);
        snapshot.route = if args.iter().any(|a| a == "--actions") {
            Route::Actions
        } else {
            Route::Meetings
        };
        let companion = args.iter().any(|a| a == "--companion");
        snapshot.workspace = WorkspaceSnapshot {
            history_enabled: true,
            capture: if companion { CaptureState::Listening } else { CaptureState::Idle },
            captured_samples: 11_648_000,
            committed_samples: 11_648_000,
            title: "Release scope & Friday delivery".into(),
            transcript: vec![MeetingLine { start_sample: 11_520_000, end_sample: 11_648_000, text: "If we narrow the first release to the core workflow, could we have it ready by Friday?".into() }],
            messages: vec![ChatLine { role: "You".into(), text: "Can we commit to Friday?".into(), status: "sent".into(), context_samples: Some((0,11_648_000)), ..Default::default() }, ChatLine { role: "Assistant".into(), text: "**Give a conditional commitment.**\n\n“Friday is realistic for the core workflow, provided we have final sign-off tomorrow.”\n\n- Confirm what stays in the first release.\n- Ask who owns the remaining sign-off.".into(), status: "complete".into(), ..Default::default() }],
            provider: ProviderDraft { model: "Qwen 3 · 8B".into(), ..Default::default() },
            input_devices: vec!["Example microphone".into()],
            output_devices: vec!["Headphones".into()],
            available_models: vec!["Qwen 3 · 8B".into()],
            sessions: vec![SavedMeeting { id: 1, title: "Release scope & Friday delivery".into(), source: "Computer audio".into() }],
            actions: vec![ActionDraft { id: "notes".into(), name: "Capture a note".into(), launcher_slot: Some(7), method: "POST".into(), endpoint: "https://notes.example.com/inbox".into(), ..Default::default() }, ActionDraft { id: "tasks".into(), name: "Create a task".into(), launcher_slot: Some(8), method: "POST".into(), endpoint: "https://tasks.example.com/items".into(), payload_mode: ActionPayloadMode::AiJson, ..Default::default() }],
            ..Default::default()
        };
        let mut shell = PhorminxUi::new(snapshot);
        shell.set_theme(if args.iter().any(|a| a == "--dark") {
            ThemeMode::AuthoredDark
        } else {
            ThemeMode::AuthoredLight
        });
        Self {
            shell,
            companion,
            capture: args
                .iter()
                .find_map(|a| a.strip_prefix("--capture=").map(str::to_owned)),
            frames: 0,
        }
    }
}
impl eframe::App for Preview {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        if self.companion {
            self.shell.show_meeting_companion(ui);
        } else {
            self.shell.show(ui);
        }
        // Intents are deliberately discarded in this harness.
        let _ = self.shell.take_events();
        self.frames += 1;
        if self.capture.is_some() && self.frames == 5 {
            ui.ctx()
                .send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
        }
        if let Some(path) = &self.capture {
            let image = ui.input(|i| {
                i.events.iter().find_map(|e| {
                    if let egui::Event::Screenshot { image, .. } = e {
                        Some(image.clone())
                    } else {
                        None
                    }
                })
            });
            if let Some(image) = image {
                let rgba: Vec<u8> = image.pixels.iter().flat_map(|p| p.to_array()).collect();
                image::save_buffer(
                    path,
                    &rgba,
                    image.width() as u32,
                    image.height() as u32,
                    image::ColorType::Rgba8,
                )
                .expect("save native preview screenshot");
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
            }
            if self.frames > 100 {
                panic!("Native screenshot did not arrive");
            }
        }
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(80));
    }
}
fn main() -> eframe::Result {
    let args: Vec<String> = std::env::args().collect();
    let companion = args.iter().any(|a| a == "--companion");
    let size = if companion {
        [430.0, 680.0]
    } else if args.iter().any(|a| a == "--small") {
        [900.0, 700.0]
    } else {
        [1120.0, 800.0]
    };
    eframe::run_native(
        "Phorminx · Synthetic redesign preview",
        eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_inner_size(size)
                .with_decorations(!companion)
                .with_active(false),
            ..Default::default()
        },
        Box::new(move |_| Ok(Box::new(Preview::new(&args)))),
    )
}
