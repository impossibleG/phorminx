use std::time::Instant;

use phorminx_ui::{
    GalleryScenario, HistoryLoadedText, HistoryVariant, PhorminxUi, Route, ShellSnapshot,
};

#[test]
fn measure_full_size_history_detail_paint() {
    let mut snapshot = ShellSnapshot::gallery(GalleryScenario::Populated);
    snapshot.route = Route::History;
    let mut app = PhorminxUi::new(snapshot);
    let id = app.snapshot().history[0].id;
    app.select_history(id);
    assert!(app.set_history_detail(id, HistoryVariant::Output, "x ".repeat(8 * 1024 * 1024)));

    eframe::egui::__run_test_ui(|ui| {
        let started = Instant::now();
        app.show(ui);
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
    });
}

#[test]
fn unicode_pages_are_bounded_contiguous_and_exact_when_reassembled() {
    let exact = "prefix 🛡️漢字 é🦀 ".repeat(4_000);
    let loaded = HistoryLoadedText {
        variant: HistoryVariant::Output,
        text: exact.clone(),
    };
    let mut assembled = String::new();
    for page in 0..loaded.page_count() {
        let slice = loaded.page(page);
        assert!(slice.len() <= 16 * 1024 + 3);
        assembled.push_str(slice);
    }
    assert_eq!(assembled, exact);
}
