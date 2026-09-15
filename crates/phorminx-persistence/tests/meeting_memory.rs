use phorminx_persistence::{DeleteSelection, Persistence, RetentionPolicy};
fn fixture() -> (tempfile::TempDir, Persistence, i64) {
    let dir = tempfile::tempdir().unwrap();
    let db = Persistence::open(dir.path().join("memory.db")).unwrap();
    db.history()
        .set_retention(RetentionPolicy::Indefinite, 0)
        .unwrap();
    let id = db
        .meetings()
        .create("Meeting", "system", 0)
        .unwrap()
        .unwrap();
    (dir, db, id)
}
#[test]
fn chunks_cover_long_unicode_and_search_by_meaning() {
    let (_dir, db, id) = fixture();
    let text = "café 日本語🙂 budget ".repeat(300);
    db.meetings()
        .append_segment(id, 0, 0, 480000, &text, 1)
        .unwrap();
    db.meeting_memory().configure_model("local@digest").unwrap();
    let mut frontier = 0;
    while let Some(p) = db.meeting_memory().pending_passage().unwrap() {
        assert!(p.start_char <= frontier);
        assert!(p.end_char > frontier);
        frontier = p.end_char;
        assert!(
            db.meeting_memory()
                .index_passage("local@digest", &p, &[1., 0.])
                .unwrap()
        );
    }
    assert_eq!(frontier, text.chars().count());
    assert!(
        db.meeting_memory()
            .search("finance", None, 10, || false)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        db.meeting_memory()
            .search("finance", Some(("local@digest", &[1., 0.])), 10, || false)
            .unwrap()[0]
            .id,
        id
    );
    assert!(
        db.meeting_memory()
            .search("finance", Some(("other", &[1., 0.])), 10, || false)
            .unwrap()
            .is_empty()
    );
    assert!(
        db.meeting_memory()
            .search("budget", None, 10, || true)
            .unwrap()
            .is_empty()
    );
}
#[test]
fn deletion_rejects_inflight_vectors_and_titles_and_cascades_context() {
    let (_dir, db, id) = fixture();
    db.meetings()
        .append_segment(id, 0, 0, 480000, "Synthetic transcript", 1)
        .unwrap();
    db.meeting_memory().configure_model("local").unwrap();
    let p = db.meeting_memory().pending_passage().unwrap().unwrap();
    let title = db.meeting_memory().pending_title().unwrap().unwrap();
    let message = db
        .meetings()
        .append_user_question(
            id,
            Some(480000),
            "What next?",
            "Synthetic transcript",
            Some(0),
            2,
        )
        .unwrap()
        .unwrap();
    assert!(
        db.meeting_memory()
            .index_passage("local", &p, &[1., 0.])
            .unwrap()
    );
    assert!(
        db.meeting_memory()
            .save_title(&title, "Synthetic topic")
            .unwrap()
    );
    db.delete_selected(DeleteSelection {
        meeting_transcripts: true,
        ..Default::default()
    })
    .unwrap();
    assert!(
        !db.meeting_memory()
            .index_passage("local", &p, &[1., 0.])
            .unwrap()
    );
    assert!(
        !db.meeting_memory()
            .save_title(&title, "Stale title")
            .unwrap()
    );
    assert_eq!(db.meetings().get(id).unwrap().unwrap().title, "Meeting");
    assert!(
        db.meetings()
            .question_context(id, message)
            .unwrap()
            .is_some()
    ); // Chat attachment deliberately retained.
    assert!(
        db.meeting_memory()
            .search("unknown", Some(("local", &[1., 0.])), 10, || false)
            .unwrap()
            .is_empty()
    );
    db.delete_selected(DeleteSelection {
        chats: true,
        ..Default::default()
    })
    .unwrap();
    assert!(
        db.meetings()
            .question_context(id, message)
            .unwrap()
            .is_none()
    );
}
#[test]
fn disabled_history_never_reads_or_writes_memory() {
    let (_dir, db, id) = fixture();
    db.meetings()
        .append_segment(id, 0, 0, 480000, "Private synthetic", 1)
        .unwrap();
    db.meeting_memory().configure_model("local").unwrap();
    let p = db.meeting_memory().pending_passage().unwrap().unwrap();
    let title = db.meeting_memory().pending_title().unwrap().unwrap();
    db.history()
        .set_retention(RetentionPolicy::Disabled, 2)
        .unwrap();
    assert!(db.meeting_memory().pending_passage().unwrap().is_none());
    assert!(db.meeting_memory().pending_title().unwrap().is_none());
    assert!(
        !db.meeting_memory()
            .index_passage("local", &p, &[1., 0.])
            .unwrap()
    );
    assert!(!db.meeting_memory().save_title(&title, "Secret").unwrap());
    assert!(
        db.meeting_memory()
            .search("Private", None, 10, || false)
            .unwrap()
            .is_empty()
    );
}
#[test]
fn title_waits_for_enough_audio_or_short_session_end() {
    let (_dir, db, id) = fixture();
    db.meetings()
        .append_segment(id, 0, 0, 16000, "A short meeting", 1)
        .unwrap();
    assert!(db.meeting_memory().pending_title().unwrap().is_none());
    db.meetings().finalize(id, 2).unwrap();
    let source = db.meeting_memory().pending_title().unwrap().unwrap();
    assert!(
        db.meeting_memory()
            .save_title(&source, "Short meeting")
            .unwrap()
    );
    assert!(!db.meeting_memory().save_title(&source, "Other").unwrap());
    assert!(db.meeting_memory().pending_title().unwrap().is_none());
}
#[test]
fn attachment_retries_are_atomic_and_strict() {
    let (_dir, db, id) = fixture();
    db.meetings()
        .append_segment(id, 0, 0, 10, "First", 1)
        .unwrap();
    let message = db
        .meetings()
        .append_user_question(id, Some(10), "Question", "First", Some(0), 2)
        .unwrap()
        .unwrap();
    assert_eq!(
        db.meetings()
            .append_user_question(id, Some(10), "Question", "First", Some(0), 2)
            .unwrap(),
        Some(message)
    );
    assert!(
        db.meetings()
            .append_user_question(id, Some(10), "Changed", "First", Some(0), 2)
            .is_err()
    );
    assert!(
        db.meetings()
            .append_user_question(id, Some(10), "Question", "Changed", Some(0), 2)
            .is_err()
    );
    assert_eq!(db.meetings().messages(id, None, 10).unwrap().len(), 1);
    assert_eq!(
        db.meetings()
            .question_context(id, message)
            .unwrap()
            .unwrap()
            .text,
        "First"
    );
    assert!(
        db.meetings()
            .question_context(id + 1, message)
            .unwrap()
            .is_none()
    );
}
#[test]
fn invalid_vectors_and_stale_model_do_not_commit() {
    let (_dir, db, id) = fixture();
    db.meetings()
        .append_segment(id, 0, 0, 10, "Fixture", 1)
        .unwrap();
    db.meeting_memory().configure_model("a").unwrap();
    let p = db.meeting_memory().pending_passage().unwrap().unwrap();
    for v in [vec![], vec![0.], vec![f32::NAN], vec![f32::INFINITY]] {
        assert!(db.meeting_memory().index_passage("a", &p, &v).is_err());
    }
    db.meeting_memory().configure_model("b").unwrap();
    assert!(!db.meeting_memory().index_passage("a", &p, &[1.]).unwrap());
    assert!(db.meeting_memory().pending_passage().unwrap().is_some());
}

#[test]
fn empty_first_segment_does_not_prevent_a_title_and_changed_source_is_rejected() {
    let (_dir, db, id) = fixture();
    db.meetings().append_segment(id, 0, 0, 100, "", 1).unwrap();
    db.meetings()
        .append_segment(id, 1, 100, 480000, "Actual meeting topic", 2)
        .unwrap();
    let source = db.meeting_memory().pending_title().unwrap().unwrap();
    assert!(source.text.contains("Actual meeting topic"));
    db.meetings()
        .append_segment(id, 2, 480000, 490000, "More context", 3)
        .unwrap();
    assert!(
        !db.meeting_memory()
            .save_title(&source, "Stale title")
            .unwrap()
    );
    let current = db.meeting_memory().pending_title().unwrap().unwrap();
    assert!(
        db.meeting_memory()
            .save_title(&current, "Current topic")
            .unwrap()
    );
}

#[test]
fn out_of_range_passages_and_oversized_attachment_fail_without_partial_rows() {
    let (_dir, db, id) = fixture();
    db.meetings().append_segment(id, 0, 0, 10, "x", 1).unwrap();
    db.meeting_memory().configure_model("local").unwrap();
    let mut p = db.meeting_memory().pending_passage().unwrap().unwrap();
    p.start_char = usize::MAX - 1;
    p.end_char = usize::MAX;
    assert!(
        db.meeting_memory()
            .index_passage("local", &p, &[1.])
            .is_err()
    );
    assert!(
        db.meetings()
            .append_user_question(
                id,
                Some(10),
                "Question",
                &"x".repeat(96 * 1024 + 1),
                Some(0),
                2
            )
            .is_err()
    );
    assert!(db.meetings().messages(id, None, 10).unwrap().is_empty());
}
