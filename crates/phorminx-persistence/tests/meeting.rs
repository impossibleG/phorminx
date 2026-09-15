use phorminx_persistence::{MeetingMessageStatus as Status, Persistence, RetentionPolicy};

fn fixture() -> (tempfile::TempDir, Persistence, i64) {
    let dir = tempfile::tempdir().unwrap();
    let db = Persistence::open(dir.path().join("test.sqlite3")).unwrap();
    db.history()
        .set_retention(RetentionPolicy::Indefinite, 100)
        .unwrap();
    let id = db
        .meetings()
        .create("Client meeting", "system", 100)
        .unwrap()
        .unwrap();
    (dir, db, id)
}

fn populate_categories(db: &Persistence, id: i64) {
    db.meetings()
        .append_segment(id, 0, 0, 10, "Transcript fixture", 101)
        .unwrap();
    db.meetings()
        .append_user_message(id, 10, "Submitted excerpt", 102)
        .unwrap();
    db.meetings().start_assistant(id, 1, 103).unwrap();
    db.meetings()
        .update_assistant(id, 1, "Answer fixture", Status::Complete, 104)
        .unwrap();
    db.history()
        .insert(&phorminx_persistence::DictationDraft {
            created_at_ms: 100,
            raw_text: "Dictation fixture".into(),
            normalized_text: None,
            cleaned_text: None,
            selected_output: "Dictation fixture".into(),
            language: None,
            target_executable: None,
            timings: Default::default(),
            warnings: vec![],
        })
        .unwrap();
}

#[test]
fn all_eight_deletion_selections_preserve_every_unselected_category() {
    for bits in 0..8 {
        let (_dir, db, id) = fixture();
        populate_categories(&db, id);
        let selected = phorminx_persistence::DeleteSelection {
            dictations: bits & 1 != 0,
            meeting_transcripts: bits & 2 != 0,
            chats: bits & 4 != 0,
        };
        db.delete_selected(selected).unwrap();
        assert_eq!(
            db.history().count().unwrap(),
            u64::from(!selected.dictations)
        );
        assert_eq!(
            db.meetings().segments(id, None, 100).unwrap().len(),
            usize::from(!selected.meeting_transcripts)
        );
        assert_eq!(
            db.meetings().messages(id, None, 100).unwrap().len(),
            if selected.chats { 0 } else { 2 }
        );
        assert_eq!(
            db.history().retention().unwrap(),
            RetentionPolicy::Indefinite
        );
        db.delete_selected(selected).unwrap(); // Explicit retry is idempotent.
    }
}

#[test]
fn category_deletion_rolls_back_every_category_on_failure() {
    let (dir, db, id) = fixture();
    populate_categories(&db, id);
    let control = rusqlite::Connection::open(dir.path().join("test.sqlite3")).unwrap();
    control.execute_batch("CREATE TRIGGER block_chat_delete BEFORE DELETE ON meeting_messages BEGIN SELECT RAISE(ABORT,'synthetic storage failure'); END;").unwrap();
    assert!(
        db.delete_selected(phorminx_persistence::DeleteSelection {
            dictations: true,
            meeting_transcripts: true,
            chats: true
        })
        .is_err()
    );
    assert_eq!(db.history().count().unwrap(), 1);
    assert_eq!(db.meetings().segments(id, None, 100).unwrap().len(), 1);
    assert_eq!(db.meetings().messages(id, None, 100).unwrap().len(), 2);
}

#[test]
fn text_only_meeting_survives_reopen_and_paginates() {
    let (dir, db, id) = fixture();
    for n in 0..105 {
        assert!(
            db.meetings()
                .append_segment(id, n, n * 10, (n + 1) * 10, "Olá 世界", 100 + n as i64)
                .unwrap()
        );
    }
    let page = db.meetings().segments(id, None, usize::MAX).unwrap();
    assert_eq!(page.len(), 100);
    assert_eq!(page[99].sequence, 99);
    assert_eq!(db.meetings().segments(id, Some(99), 100).unwrap().len(), 5);
    assert!(db.meetings().segments(id, None, 0).unwrap().is_empty());
    assert!(db.meetings().finalize(id, 500).unwrap());
    assert!(!db.meetings().finalize(id, 600).unwrap());
    drop(db);
    let reopened = Persistence::open(dir.path().join("test.sqlite3")).unwrap();
    let saved = reopened.meetings().get(id).unwrap().unwrap();
    assert_eq!(saved.ended_at_ms, Some(500));
    assert_eq!(saved.committed_sample, 1050);
    assert_eq!(saved.next_sequence, 105);
    assert_eq!(reopened.schema_version().unwrap(), 1);
}

#[test]
fn segment_replay_is_idempotent_but_conflicting_overlaps_and_gaps_fail() {
    let (_dir, db, id) = fixture();
    let repo = db.meetings();
    assert!(repo.append_segment(id, 0, 0, 10, "first", 101).unwrap());
    assert!(repo.append_segment(id, 1, 10, 20, "second", 102).unwrap());
    assert!(repo.append_segment(id, 0, 0, 10, "first", 500).unwrap());
    for (sequence, start, end, text) in [
        (0, 0, 10, "changed"),
        (2, 19, 30, "overlap"),
        (3, 20, 30, "gap"),
        (2, 21, 30, "gap"),
        (2, 20, 20, "empty"),
    ] {
        assert!(
            repo.append_segment(id, sequence, start, end, text, 102)
                .is_err()
        );
    }
    assert_eq!(repo.segments(id, None, 100).unwrap().len(), 2);
    assert_eq!(repo.get(id).unwrap().unwrap().updated_at_ms, 102);
}

#[test]
fn user_messages_require_frozen_exact_boundaries_and_retry_safely() {
    let (_dir, db, id) = fixture();
    let repo = db.meetings();
    repo.append_segment(id, 0, 0, 10, "first", 101).unwrap();
    repo.append_segment(id, 1, 10, 20, "second", 102).unwrap();
    assert!(repo.append_user_message(id, 5, "half", 103).is_err());
    assert!(repo.append_user_message(id, 21, "future", 103).is_err());
    let first = repo
        .append_user_message(id, 10, "first", 103)
        .unwrap()
        .unwrap();
    assert_eq!(
        repo.append_user_message(id, 10, "first", 104).unwrap(),
        Some(first)
    );
    assert!(repo.append_user_message(id, 10, "different", 104).is_err());
    assert!(repo.append_user_message(id, 0, "backwards", 104).is_err());
    repo.append_user_message(id, 20, "second", 105).unwrap();
    assert_eq!(repo.messages(id, None, 100).unwrap().len(), 2);
}

#[test]
fn new_ai_generation_cancels_old_and_late_output_cannot_overwrite() {
    let (_dir, db, id) = fixture();
    let repo = db.meetings();
    repo.start_assistant(id, 1, 101).unwrap().unwrap();
    assert!(
        repo.update_assistant(id, 1, "partial", Status::Streaming, 102)
            .unwrap()
    );
    repo.start_assistant(id, 2, 103).unwrap().unwrap();
    assert!(
        !repo
            .update_assistant(id, 1, "stale completion", Status::Complete, 104)
            .unwrap()
    );
    assert_eq!(repo.start_assistant(id, 1, 104).unwrap(), None);
    assert_eq!(repo.start_assistant(id, 2, 104).unwrap(), None);
    assert!(
        repo.update_assistant(id, 2, "new response", Status::Complete, 105)
            .unwrap()
    );
    assert!(
        !repo
            .update_assistant(id, 2, "late", Status::Streaming, 106)
            .unwrap()
    );
    let messages = repo.messages(id, None, 100).unwrap();
    assert_eq!(messages[0].text, "partial");
    assert_eq!(messages[0].status, Status::Cancelled);
    assert_eq!(messages[1].text, "new response");
    assert_eq!(messages[1].status, Status::Complete);
}

#[test]
fn finalizing_capture_does_not_interrupt_ai_but_blocks_new_audio_segments() {
    let (_dir, db, id) = fixture();
    let repo = db.meetings();
    repo.append_segment(id, 0, 0, 10, "first", 101).unwrap();
    repo.start_assistant(id, 1, 102).unwrap();
    repo.finalize(id, 103).unwrap();
    assert!(!repo.append_segment(id, 1, 10, 20, "late", 104).unwrap());
    assert!(
        repo.update_assistant(id, 1, "answer", Status::Complete, 105)
            .unwrap()
    );
    assert_eq!(repo.segments(id, None, 100).unwrap().len(), 1);
}

#[test]
fn explicit_session_delete_cascades_and_late_workers_cannot_recreate_it() {
    let (_dir, db, id) = fixture();
    let repo = db.meetings();
    repo.append_segment(id, 0, 0, 10, "private", 101).unwrap();
    repo.append_user_message(id, 10, "private", 101).unwrap();
    repo.start_assistant(id, 1, 102).unwrap();
    assert!(repo.delete(id).unwrap());
    assert!(!repo.delete(id).unwrap());
    assert!(repo.get(id).unwrap().is_none());
    assert!(repo.segments(id, None, 100).unwrap().is_empty());
    assert!(repo.messages(id, None, 100).unwrap().is_empty());
    assert!(!repo.append_segment(id, 1, 10, 20, "late", 103).unwrap());
    assert_eq!(repo.append_user_message(id, 10, "late", 103).unwrap(), None);
    assert_eq!(repo.start_assistant(id, 2, 103).unwrap(), None);
    assert!(
        !repo
            .update_assistant(id, 1, "late", Status::Complete, 103)
            .unwrap()
    );
    let next = repo.create("new", "mic", 200).unwrap().unwrap();
    assert!(next > id);
}

#[test]
fn disabled_retention_never_persists_future_meeting_content() {
    let (_dir, db, id) = fixture();
    db.history()
        .set_retention(RetentionPolicy::Disabled, 200)
        .unwrap();
    let repo = db.meetings();
    assert_eq!(repo.create("private", "mic", 201).unwrap(), None);
    assert!(!repo.append_segment(id, 0, 0, 10, "private", 201).unwrap());
    assert_eq!(
        repo.append_user_message(id, 0, "private", 201).unwrap(),
        None
    );
    assert_eq!(repo.start_assistant(id, 1, 201).unwrap(), None);
    assert!(
        !repo
            .update_assistant(id, 1, "private", Status::Complete, 201)
            .unwrap()
    );
    assert!(repo.segments(id, None, 100).unwrap().is_empty());
    assert!(repo.messages(id, None, 100).unwrap().is_empty());
}

#[test]
fn keyword_search_matches_transcript_and_chat_unicode_but_not_sql_wildcards() {
    let (_dir, db, id) = fixture();
    let repo = db.meetings();
    repo.append_segment(id, 0, 0, 10, "Olá orçamento", 101)
        .unwrap();
    repo.start_assistant(id, 1, 102).unwrap();
    repo.update_assistant(id, 1, "deployment proposal", Status::Complete, 103)
        .unwrap();
    for term in ["client", "ORÇAMENTO", "proposal"] {
        assert_eq!(repo.list(term, None, 20).unwrap()[0].id, id);
    }
    for term in ["missing", "%", "' OR 1=1 --"] {
        assert!(repo.list(term, None, 20).unwrap().is_empty());
    }
    assert!(repo.list("", Some(id), 20).unwrap().is_empty());
}

#[test]
fn message_preview_and_chunk_reads_are_bounded_on_unicode_boundaries() {
    let (_dir, db, id) = fixture();
    let repo = db.meetings();
    let text = "界".repeat(20_000);
    repo.start_assistant(id, 1, 101).unwrap();
    repo.update_assistant(id, 1, &text, Status::Complete, 102)
        .unwrap();
    let messages = repo.messages(id, None, usize::MAX).unwrap();
    assert!(messages[0].text_truncated);
    assert_eq!(messages[0].text.chars().count(), 16_384);
    let tail = repo
        .message_text(id, messages[0].id, 16_384, usize::MAX)
        .unwrap()
        .unwrap();
    assert_eq!(format!("{}{tail}", messages[0].text), text);
    assert_eq!(
        repo.message_text(id + 1, messages[0].id, 0, 1).unwrap(),
        None
    );
    assert!(
        repo.message_text(id, messages[0].id, usize::MAX, 1)
            .is_err()
    );
}

#[test]
fn invalid_inputs_are_rejected_without_mutation() {
    let (_dir, db, id) = fixture();
    let repo = db.meetings();
    assert!(repo.create("bad source", "unknown", 100).is_err());
    assert!(repo.create(&"x".repeat(1025), "mic", 100).is_err());
    assert!(
        repo.append_segment(id, 0, 0, u64::MAX, "overflow", 101)
            .is_err()
    );
    assert!(
        repo.append_segment(id, 0, 0, 1, &"x".repeat(256 * 1024 + 1), 101)
            .is_err()
    );
    assert!(repo.start_assistant(id, u64::MAX, 101).is_err());
    assert_eq!(repo.start_assistant(id, 0, 101).unwrap(), None);
    assert!(repo.get(id).unwrap().unwrap().next_sequence == 0);
}

#[test]
fn additive_migration_preserves_legacy_history() {
    use phorminx_persistence::{DictationDraft, TimingMetadata};
    let (dir, db, id) = fixture();
    let history = db
        .history()
        .insert(&DictationDraft {
            created_at_ms: 100,
            raw_text: "existing".into(),
            normalized_text: None,
            cleaned_text: None,
            selected_output: "existing".into(),
            language: None,
            target_executable: None,
            timings: TimingMetadata::default(),
            warnings: vec![],
        })
        .unwrap()
        .unwrap();
    drop(db);
    let db = Persistence::open(dir.path().join("test.sqlite3")).unwrap();
    assert_eq!(
        db.history()
            .get(history)
            .unwrap()
            .unwrap()
            .dictation
            .selected_output,
        "existing"
    );
    assert!(db.meetings().get(id).unwrap().is_some());
    assert_eq!(db.schema_version().unwrap(), 1);
}

#[test]
fn typed_chat_is_independent_of_audio_and_can_continue_after_capture_ends() {
    let (_dir, db, id) = fixture();
    let repo = db.meetings();
    let conversation = repo
        .create("Plain conversation", "chat", 100)
        .unwrap()
        .unwrap();
    assert_eq!(repo.get(conversation).unwrap().unwrap().source, "chat");
    assert!(
        repo.append_chat_user(conversation, "No capture needed", 101)
            .unwrap()
            .is_some()
    );
    repo.append_chat_user(id, "Before capture", 101)
        .unwrap()
        .unwrap();
    repo.append_segment(id, 0, 0, 10, "spoken", 102).unwrap();
    repo.append_user_message(id, 10, "spoken", 102)
        .unwrap()
        .unwrap();
    repo.finalize(id, 103).unwrap();
    repo.append_chat_user(id, "After capture", 104)
        .unwrap()
        .unwrap();
    let messages = repo.messages(id, None, 100).unwrap();
    assert_eq!(messages.len(), 3);
    assert_eq!(messages[0].cutoff_sample, None);
    assert_eq!(messages[1].cutoff_sample, Some(10));
    assert_eq!(messages[2].cutoff_sample, None);
    assert_eq!(
        repo.append_chat_user(id + 999, "missing", 105).unwrap(),
        None
    );
    db.history()
        .set_retention(RetentionPolicy::Disabled, 106)
        .unwrap();
    assert_eq!(repo.append_chat_user(id, "private", 107).unwrap(), None);
}

#[test]
fn cancelled_and_failed_responses_cannot_be_resurrected() {
    let (_dir, db, id) = fixture();
    let repo = db.meetings();
    for (generation, status) in [(1, Status::Cancelled), (2, Status::Failed)] {
        repo.start_assistant(id, generation, 100).unwrap().unwrap();
        assert!(
            repo.update_assistant(id, generation, "partial", status, 101)
                .unwrap()
        );
        assert!(
            !repo
                .update_assistant(id, generation, "late", Status::Complete, 102)
                .unwrap()
        );
    }
    assert_eq!(
        repo.messages(id, None, 100).unwrap()[1].status,
        Status::Failed
    );
}

#[test]
fn deletion_on_another_connection_rejects_inflight_text() {
    let (dir, db, id) = fixture();
    let writer = Persistence::open(dir.path().join("test.sqlite3")).unwrap();
    writer.meetings().start_assistant(id, 1, 101).unwrap();
    db.meetings().delete(id).unwrap();
    assert!(
        !writer
            .meetings()
            .append_segment(id, 0, 0, 10, "late", 102)
            .unwrap()
    );
    assert!(
        !writer
            .meetings()
            .update_assistant(id, 1, "late", Status::Complete, 102)
            .unwrap()
    );
    assert_eq!(
        writer.meetings().append_chat_user(id, "late", 102).unwrap(),
        None
    );
    assert!(db.meetings().list("late", None, 10).unwrap().is_empty());
}

#[test]
fn genuinely_old_database_gains_meetings_without_rewriting_history() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("legacy.sqlite3");
    let legacy = rusqlite::Connection::open(&path).unwrap();
    legacy.execute_batch("CREATE TABLE schema_migrations(version INTEGER PRIMARY KEY,applied_at_ms INTEGER NOT NULL); INSERT INTO schema_migrations VALUES(1,100); CREATE TABLE persistence_settings(key TEXT PRIMARY KEY,value TEXT NOT NULL); INSERT INTO persistence_settings VALUES('history_retention','indefinite'); CREATE TABLE dictation_history(id INTEGER PRIMARY KEY AUTOINCREMENT,created_at_ms INTEGER NOT NULL,raw_text TEXT NOT NULL,normalized_text TEXT,cleaned_text TEXT,selected_output TEXT NOT NULL,language TEXT,target_executable TEXT,audio_duration_ms INTEGER,stt_duration_ms INTEGER,formatting_duration_ms INTEGER,insertion_duration_ms INTEGER,warnings_json TEXT NOT NULL DEFAULT '[]'); INSERT INTO dictation_history(created_at_ms,raw_text,selected_output) VALUES(100,'legacy raw','legacy selected');").unwrap();
    drop(legacy);
    let migrated = Persistence::open(path).unwrap();
    assert_eq!(migrated.schema_version().unwrap(), 1);
    assert_eq!(
        migrated
            .history()
            .get(1)
            .unwrap()
            .unwrap()
            .dictation
            .selected_output,
        "legacy selected"
    );
    let session = migrated
        .meetings()
        .create("new", "system", 200)
        .unwrap()
        .unwrap();
    assert!(
        migrated
            .meetings()
            .append_segment(session, 0, 0, 10, "new", 201)
            .unwrap()
    );
}

#[test]
fn latest_pages_return_recent_rows_chronologically_with_explicit_previews() {
    let (_dir, db, id) = fixture();
    let repo = db.meetings();
    for n in 0..205 {
        let text = if n == 204 {
            "界".repeat(20_000)
        } else {
            format!("segment {n}")
        };
        repo.append_segment(id, n, n * 10, (n + 1) * 10, &text, 101)
            .unwrap();
        repo.append_chat_user(id, &text, 102).unwrap();
    }
    let segments = repo.latest_segments(id, usize::MAX).unwrap();
    assert_eq!(segments.len(), 200);
    assert_eq!(segments[0].sequence, 5);
    assert_eq!(segments[199].sequence, 204);
    assert!(segments[199].text_truncated);
    assert_eq!(segments[199].text.chars().count(), 16_384);
    let full = repo.segments(id, Some(203), 1).unwrap();
    assert!(!full[0].text_truncated);
    assert_eq!(full[0].text.chars().count(), 20_000);
    let messages = repo.latest_messages(id, usize::MAX).unwrap();
    assert_eq!(messages.len(), 80);
    assert_eq!(messages[0].text, "segment 125");
    assert!(messages[79].text_truncated);
    assert!(repo.latest_messages(id, 0).unwrap().is_empty());
    assert!(repo.latest_segments(id, 0).unwrap().is_empty());
}
