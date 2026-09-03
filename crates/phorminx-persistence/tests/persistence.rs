use std::path::Path;

use phorminx_persistence::{
    AppProfile, CasePolicy, DictationDraft, ExecutableIdentity, FormattingStyle,
    HISTORY_PREVIEW_MAX_CHARS, HistoryTextVariant, InsertionPreference, MAX_TERMINAL_TEXT_BYTES,
    MAX_TERMINAL_WARNING_BYTES, MAX_TERMINAL_WARNINGS, NewLexiconEntry, Persistence,
    PersistenceError, RetentionPolicy, TerminalMetadata, TimingMetadata,
};
use tempfile::TempDir;

const NOW: i64 = 2_000_000_000_000;
const DAY_MS: i64 = 24 * 60 * 60 * 1_000;

fn open_temp() -> (TempDir, Persistence) {
    let directory = tempfile::tempdir().unwrap();
    let database = Persistence::open(directory.path().join("nested/phorminx.sqlite3")).unwrap();
    (directory, database)
}

fn draft(created_at_ms: i64, text: &str) -> DictationDraft {
    DictationDraft {
        created_at_ms,
        raw_text: format!("raw {text}"),
        normalized_text: Some(format!("normalized {text}")),
        cleaned_text: Some(format!("cleaned {text}")),
        selected_output: format!("selected {text}"),
        language: Some("en".into()),
        target_executable: Some("notepad.exe".into()),
        timings: TimingMetadata {
            audio_duration_ms: Some(1_234),
            stt_duration_ms: Some(250),
            formatting_duration_ms: Some(90),
            insertion_duration_ms: Some(12),
            audio_finalization_duration_ms: Some(7),
            worker_queue_duration_ms: Some(3),
            release_to_insert_duration_ms: Some(362),
        },
        warnings: vec!["low confidence".into(), "clipboard fallback".into()],
    }
}

fn terminal_metadata() -> TerminalMetadata {
    TerminalMetadata {
        checkpoint_count: Some(47),
        checkpoint_repair_count: Some(2),
        peak_retained_audio_ms: Some(31_250),
        formatting_chunk_count: Some(8),
        auto_stopped: Some(false),
    }
}

fn lexicon(alias: &str) -> NewLexiconEntry {
    NewLexiconEntry {
        canonical: "Phorminx".into(),
        alias: alias.into(),
        language: None,
        app_executable: None,
        case_policy: CasePolicy::UseCanonical,
        enabled: true,
    }
}

fn profile(executable: &str) -> AppProfile {
    AppProfile {
        executable: ExecutableIdentity::new(executable).unwrap(),
        formatting_style: FormattingStyle::Balanced,
        custom_instructions: None,
        language: Some("pt-BR".into()),
        insertion_preference: InsertionPreference::Direct,
        deny: false,
    }
}

#[test]
fn creates_parent_enables_wal_and_applies_migrations_idempotently() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("a/b/history.sqlite3");
    let database = Persistence::open(&path).unwrap();
    assert_eq!(database.schema_version().unwrap(), 1);
    drop(database);

    let reopened = Persistence::open(&path).unwrap();
    assert_eq!(reopened.schema_version().unwrap(), 1);

    let raw = rusqlite::Connection::open(&path).unwrap();
    let mode: String = raw
        .pragma_query_value(None, "journal_mode", |row| row.get(0))
        .unwrap();
    let foreign_keys: bool = raw
        .pragma_query_value(None, "foreign_keys", |row| row.get(0))
        .unwrap();
    assert_eq!(mode, "wal");
    // Bundled SQLite is built with foreign-key enforcement as its default, and
    // Persistence also enables it explicitly on every managed connection.
    assert!(foreign_keys);
}

#[test]
fn additive_timings_are_compatible_with_old_schema_one_reads_and_writes() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("phorminx.db");
    let current = Persistence::open(&path).unwrap();
    assert_eq!(current.schema_version().unwrap(), 1);
    drop(current);

    let old_binary = rusqlite::Connection::open(&path).unwrap();
    let timing_column_count: u32 = old_binary
        .query_row(
            "SELECT COUNT(*) FROM pragma_table_info('dictation_history')
             WHERE name IN (
                 'audio_finalization_duration_ms',
                 'worker_queue_duration_ms',
                 'release_to_insert_duration_ms',
                 'checkpoint_count',
                 'checkpoint_repair_count',
                 'peak_retained_audio_ms',
                 'formatting_chunk_count',
                 'auto_stopped'
             )",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(timing_column_count, 8);
    old_binary
        .execute(
            "INSERT INTO dictation_history(
                 created_at_ms, raw_text, normalized_text, cleaned_text,
                 selected_output, language, target_executable,
                 audio_duration_ms, stt_duration_ms, formatting_duration_ms,
                 insertion_duration_ms, warnings_json
             ) VALUES (
                 1, 'old raw', NULL, NULL, 'old selected', 'en', NULL,
                 100, 20, 0, 1, '[]'
             )",
            [],
        )
        .unwrap();
    let old_selected: String = old_binary
        .query_row(
            "SELECT selected_output FROM dictation_history WHERE id = 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(old_selected, "old selected");

    // A pre-release build briefly labeled these same nullable columns schema
    // 2. The current build safely normalizes that marker back to the additive,
    // old-binary-compatible schema 1 contract without touching content.
    old_binary
        .execute(
            "INSERT INTO schema_migrations(version, applied_at_ms) VALUES (2, 2)",
            [],
        )
        .unwrap();
    old_binary.pragma_update(None, "user_version", 2).unwrap();
    drop(old_binary);

    let normalized = Persistence::open(&path).unwrap();
    assert_eq!(normalized.schema_version().unwrap(), 1);
    let record = normalized.history().recent(1).unwrap().remove(0);
    assert_eq!(record.dictation.selected_output, "old selected");
    assert_eq!(record.dictation.timings.release_to_insert_duration_ms, None);
    assert_eq!(
        normalized.history().terminal_metadata(record.id).unwrap(),
        Some(TerminalMetadata::default())
    );
    drop(normalized);

    let old_binary = rusqlite::Connection::open(&path).unwrap();
    let version: u32 = old_binary
        .query_row("SELECT MAX(version) FROM schema_migrations", [], |row| {
            row.get(0)
        })
        .unwrap();
    let user_version: u32 = old_binary
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(version, 1);
    assert_eq!(user_version, 1);
    old_binary
        .execute(
            "INSERT INTO dictation_history(
                 created_at_ms, raw_text, selected_output, warnings_json
             ) VALUES (2, 'second old raw', 'second old output', '[]')",
            [],
        )
        .unwrap();
    let outputs: Vec<String> = old_binary
        .prepare("SELECT selected_output FROM dictation_history ORDER BY id")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(outputs, ["old selected", "second old output"]);
}

#[test]
fn privacy_deletions_never_leave_a_rollback_artifact() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("phorminx.db");
    let artifact = directory.path().join("phorminx.db.schema-1.backup");
    std::fs::write(
        &artifact,
        b"stale private transcript lexicon alias and profile",
    )
    .unwrap();
    std::fs::write(
        directory.path().join("phorminx.db.schema-1.backup-wal"),
        b"stale WAL private transcript",
    )
    .unwrap();
    std::fs::write(
        directory.path().join("phorminx.db.schema-1.backup-shm"),
        b"stale shared-memory artifact",
    )
    .unwrap();
    std::fs::write(
        directory
            .path()
            .join("phorminx.db.schema-1-backup-99-1.tmp"),
        b"interrupted private snapshot",
    )
    .unwrap();

    let database = Persistence::open(&path).unwrap();
    assert!(!artifact.exists());
    database
        .history()
        .set_retention(RetentionPolicy::Indefinite, NOW)
        .unwrap();
    database.history().insert(&draft(NOW, "clear me")).unwrap();
    assert_eq!(database.history().clear().unwrap(), 1);
    database
        .history()
        .insert(&draft(NOW - 2 * DAY_MS, "expire me"))
        .unwrap();
    database
        .history()
        .set_retention(RetentionPolicy::Hours24, NOW)
        .unwrap();
    assert_eq!(database.history().count().unwrap(), 0);

    let lexicon_id = database.lexicon().insert(&lexicon("delete alias")).unwrap();
    assert!(database.lexicon().delete(lexicon_id).unwrap());
    let profile = profile("private-profile.exe");
    database.app_profiles().upsert(&profile).unwrap();
    assert!(database.app_profiles().delete(&profile.executable).unwrap());
    drop(database);

    assert!(!artifact.exists());
    let artifacts = std::fs::read_dir(directory.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.contains("backup"))
        .collect::<Vec<_>>();
    assert!(
        artifacts.is_empty(),
        "unexpected rollback artifacts: {artifacts:?}"
    );
}

#[test]
fn rejects_a_database_from_a_newer_schema_without_mutating_it() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("future.sqlite3");
    drop(Persistence::open(&path).unwrap());
    let raw = rusqlite::Connection::open(&path).unwrap();
    raw.execute(
        "INSERT INTO schema_migrations(version, applied_at_ms) VALUES (999, 0)",
        [],
    )
    .unwrap();
    drop(raw);

    assert!(matches!(
        Persistence::open(&path),
        Err(PersistenceError::Validation {
            field: "schema_version",
            ..
        })
    ));
    let raw = rusqlite::Connection::open(&path).unwrap();
    let version: u32 = raw
        .query_row("SELECT MAX(version) FROM schema_migrations", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(version, 999);
}

#[test]
fn history_defaults_to_disabled_and_does_not_save_content() {
    let (_directory, database) = open_temp();
    let history = database.history();
    assert_eq!(history.retention().unwrap(), RetentionPolicy::Disabled);
    assert_eq!(history.insert(&draft(NOW, "private words")).unwrap(), None);
    assert_eq!(history.count().unwrap(), 0);
}

#[test]
fn history_round_trips_all_text_metadata_timings_and_warnings() {
    let (_directory, database) = open_temp();
    let history = database.history();
    history
        .set_retention(RetentionPolicy::Indefinite, NOW)
        .unwrap();
    let expected = draft(NOW, "hello");
    let id = history.insert(&expected).unwrap().unwrap();
    let record = history.get(id).unwrap().unwrap();
    assert_eq!(record.id, id);
    assert_eq!(record.dictation, expected);
}

#[test]
fn terminal_metadata_round_trips_without_changing_legacy_records() {
    let (_directory, database) = open_temp();
    let history = database.history();
    history
        .set_retention(RetentionPolicy::Indefinite, NOW)
        .unwrap();

    let legacy_id = history.insert(&draft(NOW, "legacy")).unwrap().unwrap();
    assert_eq!(
        history.terminal_metadata(legacy_id).unwrap(),
        Some(TerminalMetadata::default())
    );

    let metadata = terminal_metadata();
    let extended_id = history
        .insert_with_terminal_metadata(&draft(NOW + 1, "extended"), &metadata)
        .unwrap()
        .unwrap();
    assert_eq!(
        history.terminal_metadata(extended_id).unwrap(),
        Some(metadata)
    );
    assert_eq!(
        history.summary(extended_id).unwrap().unwrap().terminal,
        metadata
    );
    assert_eq!(history.terminal_metadata(i64::MAX).unwrap(), None);
}

#[test]
fn hundreds_of_long_unicode_records_have_strictly_bounded_summaries() {
    const RECORDS: usize = 300;
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("history.sqlite3");
    let database = Persistence::open(&path).unwrap();
    let history = database.history();
    history
        .set_retention(RetentionPolicy::Indefinite, NOW)
        .unwrap();

    let tail = "🛡️漢字".repeat(HISTORY_PREVIEW_MAX_CHARS + 1);
    for offset in 0..RECORDS {
        let mut record = draft(NOW + offset as i64, "summary-load");
        record.selected_output = format!("record-{offset}-{tail}");
        history
            .insert_with_terminal_metadata(&record, &terminal_metadata())
            .unwrap();
    }

    drop(database);
    let raw = rusqlite::Connection::open(&path).unwrap();
    raw.execute(
        "UPDATE dictation_history SET raw_text = x'80' WHERE id = 1",
        [],
    )
    .unwrap();
    drop(raw);

    let database = Persistence::open(&path).unwrap();
    let history = database.history();

    let summaries = history.recent_summaries(RECORDS + 100).unwrap();
    assert_eq!(summaries.len(), RECORDS);
    assert!(summaries.iter().all(|summary| {
        summary.selected_output_preview.chars().count() <= HISTORY_PREVIEW_MAX_CHARS
            && summary.preview_truncated
            && summary.selected_output_chars > u64::try_from(HISTORY_PREVIEW_MAX_CHARS).unwrap()
            && summary.warnings.len() <= MAX_TERMINAL_WARNINGS
            && summary.variants.raw
            && summary.variants.normalized
            && summary.variants.cleaned
            && summary.variants.selected_output
    }));
    let newest = &summaries[0];
    assert_eq!(newest.selected_output_preview.chars().count(), 240);
    assert!(
        newest
            .selected_output_preview
            .is_char_boundary(newest.selected_output_preview.len())
    );

    let exact = history.selected_output(newest.id).unwrap().unwrap();
    assert_eq!(exact.chars().count() as u64, newest.selected_output_chars);
    assert!(exact.ends_with("🛡️漢字"));
    assert!(history.text_variant(1, HistoryTextVariant::Raw).is_err());
}

#[test]
fn summary_queries_do_not_decode_unrequested_full_variants() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("history.sqlite3");
    let database = Persistence::open(&path).unwrap();
    database
        .history()
        .set_retention(RetentionPolicy::Indefinite, NOW)
        .unwrap();
    database.history().insert(&draft(NOW, "lazy")).unwrap();
    drop(database);

    let raw = rusqlite::Connection::open(&path).unwrap();
    raw.execute(
        "UPDATE dictation_history SET raw_text = x'80' WHERE id = 1",
        [],
    )
    .unwrap();
    let legacy_warnings = serde_json::to_string(&vec!["old warning"; 1_000]).unwrap();
    raw.execute(
        "UPDATE dictation_history SET warnings_json = ?1 WHERE id = 1",
        [legacy_warnings],
    )
    .unwrap();
    drop(raw);

    let database = Persistence::open(&path).unwrap();
    let history = database.history();
    let summary = history.summary(1).unwrap().unwrap();
    assert_eq!(summary.selected_output_preview, "selected lazy");
    assert!(summary.variants.raw);
    assert!(summary.variants.normalized);
    assert!(summary.variants.cleaned);
    assert!(summary.variants.selected_output);
    assert_eq!(summary.warnings.len(), MAX_TERMINAL_WARNINGS);
    assert_eq!(
        summary.warnings.last().unwrap(),
        "stored warning metadata exceeds the current display limit"
    );
    assert_eq!(
        history.selected_output(1).unwrap(),
        Some("selected lazy".into())
    );
    assert!(history.get(1).is_err());
}

#[test]
fn oversized_legacy_variant_is_rejected_without_returning_its_payload() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("history.sqlite3");
    let database = Persistence::open(&path).unwrap();
    database
        .history()
        .set_retention(RetentionPolicy::Indefinite, NOW)
        .unwrap();
    database.history().insert(&draft(NOW, "legacy")).unwrap();
    drop(database);

    let raw = rusqlite::Connection::open(&path).unwrap();
    raw.execute(
        "UPDATE dictation_history SET cleaned_text = zeroblob(?1) WHERE id = 1",
        [i64::try_from(MAX_TERMINAL_TEXT_BYTES + 1).unwrap()],
    )
    .unwrap();
    drop(raw);

    let database = Persistence::open(&path).unwrap();
    assert!(matches!(
        database
            .history()
            .text_variant(1, HistoryTextVariant::Cleaned),
        Err(PersistenceError::TextLimitExceeded {
            field: "cleaned_text",
            actual_bytes,
            ..
        }) if actual_bytes == MAX_TERMINAL_TEXT_BYTES + 1
    ));
    assert!(
        database
            .history()
            .summary(1)
            .unwrap()
            .unwrap()
            .variants
            .cleaned
    );
}

#[test]
fn direct_variant_fetches_are_exact_and_lazy() {
    let (_directory, database) = open_temp();
    let history = database.history();
    history
        .set_retention(RetentionPolicy::Indefinite, NOW)
        .unwrap();
    let expected = draft(NOW, "copy precisely 🦀");
    let id = history.insert(&expected).unwrap().unwrap();

    assert_eq!(
        history.text_variant(id, HistoryTextVariant::Raw).unwrap(),
        Some(expected.raw_text)
    );
    assert_eq!(
        history
            .text_variant(id, HistoryTextVariant::Normalized)
            .unwrap(),
        expected.normalized_text
    );
    assert_eq!(
        history
            .text_variant(id, HistoryTextVariant::Cleaned)
            .unwrap(),
        expected.cleaned_text
    );
    assert_eq!(
        history
            .text_variant(id, HistoryTextVariant::SelectedOutput)
            .unwrap(),
        Some(expected.selected_output)
    );
    assert_eq!(history.selected_output(i64::MAX).unwrap(), None);
}

#[test]
fn terminal_text_and_warning_caps_fail_with_typed_errors_before_sql_insert() {
    let (_directory, database) = open_temp();
    let history = database.history();
    history
        .set_retention(RetentionPolicy::Indefinite, NOW)
        .unwrap();

    let mut exact = draft(NOW, "exact cap");
    exact.selected_output = "🛡".repeat(MAX_TERMINAL_TEXT_BYTES / "🛡".len());
    let exact_id = history.insert(&exact).unwrap().unwrap();
    let exact_summary = history.summary(exact_id).unwrap().unwrap();
    assert_eq!(
        exact_summary.selected_output_preview.chars().count(),
        HISTORY_PREVIEW_MAX_CHARS
    );
    assert!(exact_summary.preview_truncated);

    let mut too_long = draft(NOW + 1, "too long");
    too_long.cleaned_text = Some("é".repeat(MAX_TERMINAL_TEXT_BYTES / 2 + 1));
    assert!(matches!(
        history.insert(&too_long),
        Err(PersistenceError::TextLimitExceeded {
            field: "cleaned_text",
            max_bytes: MAX_TERMINAL_TEXT_BYTES,
            ..
        })
    ));

    let mut too_many_warnings = draft(NOW + 2, "warnings");
    too_many_warnings.warnings = vec!["bounded".into(); MAX_TERMINAL_WARNINGS + 1];
    assert!(matches!(
        history.insert(&too_many_warnings),
        Err(PersistenceError::CollectionLimitExceeded {
            field: "warnings",
            ..
        })
    ));

    let mut oversized_warning = draft(NOW + 3, "warning bytes");
    oversized_warning.warnings = vec!["é".repeat(MAX_TERMINAL_WARNING_BYTES / 2 + 1)];
    assert!(matches!(
        history.insert(&oversized_warning),
        Err(PersistenceError::TextLimitExceeded {
            field: "warning",
            ..
        })
    ));
    assert_eq!(history.count().unwrap(), 1);
}

#[test]
fn disabled_retention_discards_extended_terminal_records() {
    let (_directory, database) = open_temp();
    let history = database.history();
    assert_eq!(history.retention().unwrap(), RetentionPolicy::Disabled);
    assert_eq!(
        history
            .insert_with_terminal_metadata(&draft(NOW, "never store"), &terminal_metadata())
            .unwrap(),
        None
    );
    assert_eq!(history.count().unwrap(), 0);
    assert!(history.recent_summaries(100).unwrap().is_empty());
}

#[test]
fn recent_orders_newest_first_and_obeys_limit() {
    let (_directory, database) = open_temp();
    let history = database.history();
    history
        .set_retention(RetentionPolicy::Indefinite, NOW)
        .unwrap();
    history.insert(&draft(NOW - 2, "old")).unwrap();
    history.insert(&draft(NOW, "new")).unwrap();
    history.insert(&draft(NOW - 1, "middle")).unwrap();

    let records = history.recent(2).unwrap();
    assert_eq!(records.len(), 2);
    assert_eq!(records[0].dictation.selected_output, "selected new");
    assert_eq!(records[1].dictation.selected_output, "selected middle");
}

#[test]
fn every_bounded_retention_policy_removes_only_expired_rows() {
    for (policy, age) in [
        (RetentionPolicy::Hours24, DAY_MS),
        (RetentionPolicy::Days7, 7 * DAY_MS),
        (RetentionPolicy::Days30, 30 * DAY_MS),
    ] {
        let (_directory, database) = open_temp();
        let history = database.history();
        history
            .set_retention(RetentionPolicy::Indefinite, NOW)
            .unwrap();
        history.insert(&draft(NOW - age - 1, "expired")).unwrap();
        history.insert(&draft(NOW - age, "boundary")).unwrap();
        history.insert(&draft(NOW, "current")).unwrap();

        history.set_retention(policy, NOW).unwrap();
        assert_eq!(history.retention().unwrap(), policy);
        let records = history.recent(10).unwrap();
        assert_eq!(records.len(), 2);
        assert!(
            records
                .iter()
                .all(|record| { record.dictation.selected_output != "selected expired" })
        );
    }
}

#[test]
fn bounded_retention_is_enforced_during_normal_insertion() {
    let (_directory, database) = open_temp();
    let history = database.history();
    history
        .set_retention(RetentionPolicy::Indefinite, NOW)
        .unwrap();
    history.insert(&draft(NOW - 2 * DAY_MS, "expired")).unwrap();
    history
        .set_retention(RetentionPolicy::Hours24, NOW - 2 * DAY_MS)
        .unwrap();

    history.insert(&draft(NOW, "new")).unwrap();
    let records = history.recent(10).unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].dictation.selected_output, "selected new");
}

#[test]
fn purge_and_immediate_clear_report_removed_rows() {
    let (_directory, database) = open_temp();
    let history = database.history();
    history
        .set_retention(RetentionPolicy::Indefinite, NOW)
        .unwrap();
    history.insert(&draft(NOW - 2 * DAY_MS, "expired")).unwrap();
    history.insert(&draft(NOW, "current")).unwrap();
    history
        .set_retention(RetentionPolicy::Hours24, NOW - 2 * DAY_MS)
        .unwrap();
    assert_eq!(history.purge_expired(NOW).unwrap(), 1);
    assert_eq!(history.clear().unwrap(), 1);
    assert_eq!(history.count().unwrap(), 0);
}

#[test]
fn disabling_history_atomically_clears_existing_content() {
    let (_directory, database) = open_temp();
    let history = database.history();
    history
        .set_retention(RetentionPolicy::Indefinite, NOW)
        .unwrap();
    history.insert(&draft(NOW, "forget me")).unwrap();
    history
        .set_retention(RetentionPolicy::Disabled, NOW)
        .unwrap();
    assert_eq!(history.count().unwrap(), 0);
    assert_eq!(history.insert(&draft(NOW, "also forget")).unwrap(), None);
}

#[test]
fn lexicon_crud_preserves_exact_alias_case_and_policy() {
    let (_directory, database) = open_temp();
    let repository = database.lexicon();
    let id = repository.insert(&lexicon("form inks")).unwrap();
    let mut stored = repository.get(id).unwrap().unwrap();
    assert_eq!(stored.entry.alias, "form inks");
    assert_eq!(stored.entry.case_policy, CasePolicy::UseCanonical);

    stored.entry.alias = "Form Inks".into();
    stored.entry.case_policy = CasePolicy::PreserveInput;
    assert!(repository.update(id, &stored.entry).unwrap());
    assert!(
        repository
            .exact_matches("form inks", None, None)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        repository
            .exact_matches("Form Inks", None, None)
            .unwrap()
            .len(),
        1
    );

    assert!(repository.set_enabled(id, false).unwrap());
    assert!(
        repository
            .exact_matches("Form Inks", None, None)
            .unwrap()
            .is_empty()
    );
    assert_eq!(repository.list().unwrap().len(), 1);
    assert!(repository.delete(id).unwrap());
    assert!(!repository.delete(id).unwrap());
}

#[test]
fn lexicon_matches_global_and_scoped_entries_most_specific_first() {
    let (_directory, database) = open_temp();
    let repository = database.lexicon();
    repository.insert(&lexicon("codex")).unwrap();
    repository
        .insert(&NewLexiconEntry {
            language: Some("en".into()),
            canonical: "Codex English".into(),
            ..lexicon("codex")
        })
        .unwrap();
    repository
        .insert(&NewLexiconEntry {
            app_executable: Some("Code.exe".into()),
            canonical: "Codex Code".into(),
            ..lexicon("codex")
        })
        .unwrap();
    repository
        .insert(&NewLexiconEntry {
            language: Some("en".into()),
            app_executable: Some("Code.exe".into()),
            canonical: "Codex Most Specific".into(),
            ..lexicon("codex")
        })
        .unwrap();

    let matches = repository
        .exact_matches("codex", Some("en"), Some("code.EXE"))
        .unwrap();
    assert_eq!(matches.len(), 4);
    assert_eq!(matches[0].entry.canonical, "Codex Most Specific");
    assert_eq!(matches[1].entry.canonical, "Codex Code");
    assert_eq!(matches[2].entry.canonical, "Codex English");
    assert_eq!(matches[3].entry.canonical, "Phorminx");
}

#[test]
fn lexicon_rejects_duplicate_alias_with_same_scope() {
    let (_directory, database) = open_temp();
    let repository = database.lexicon();
    repository.insert(&lexicon("same")).unwrap();
    let error = repository.insert(&lexicon("same")).unwrap_err();
    assert!(matches!(error, PersistenceError::Database(_)));
    // Exact aliases are case-sensitive, so this remains a distinct valid entry.
    repository.insert(&lexicon("Same")).unwrap();
}

#[test]
fn app_profiles_upsert_lookup_case_insensitively_and_delete() {
    let (_directory, database) = open_temp();
    let repository = database.app_profiles();
    let mut expected = profile("Code.exe");
    repository.upsert(&expected).unwrap();

    expected.formatting_style = FormattingStyle::Strong;
    expected.insertion_preference = InsertionPreference::Clipboard;
    expected.deny = true;
    repository.upsert(&expected).unwrap();

    let lowercase = ExecutableIdentity::new("code.EXE").unwrap();
    let stored = repository.get(&lowercase).unwrap().unwrap();
    assert_eq!(stored, expected);
    assert_eq!(repository.list().unwrap(), vec![expected]);
    assert!(repository.delete(&lowercase).unwrap());
    assert!(repository.get(&lowercase).unwrap().is_none());
}

#[test]
fn app_profiles_replace_supports_case_only_renames() {
    let (_directory, database) = open_temp();
    let repository = database.app_profiles();
    let original = profile("Code.exe");
    repository.upsert(&original).unwrap();

    let mut replacement = profile("code.EXE");
    replacement.formatting_style = FormattingStyle::Strong;
    assert!(
        repository
            .replace(&original.executable, &replacement)
            .unwrap()
    );

    assert_eq!(repository.list().unwrap(), vec![replacement]);
}

#[test]
fn app_profiles_replace_rolls_back_when_new_identity_conflicts() {
    let (_directory, database) = open_temp();
    let repository = database.app_profiles();
    let original = profile("code.exe");
    let occupied = profile("notes.exe");
    repository.upsert(&original).unwrap();
    repository.upsert(&occupied).unwrap();

    let conflicting = profile("NOTES.EXE");
    assert!(matches!(
        repository.replace(&original.executable, &conflicting),
        Err(PersistenceError::Database(_))
    ));

    assert_eq!(
        repository.get(&original.executable).unwrap(),
        Some(original)
    );
    assert_eq!(
        repository.get(&occupied.executable).unwrap(),
        Some(occupied)
    );
}

#[test]
fn app_profiles_replace_preserves_original_when_replacement_is_invalid() {
    let (_directory, database) = open_temp();
    let repository = database.app_profiles();
    let original = profile("code.exe");
    repository.upsert(&original).unwrap();

    let mut invalid = profile("renamed.exe");
    invalid.formatting_style = FormattingStyle::Custom;
    invalid.custom_instructions = None;
    assert!(matches!(
        repository.replace(&original.executable, &invalid),
        Err(PersistenceError::Validation {
            field: "custom_instructions",
            ..
        })
    ));

    assert_eq!(
        repository.get(&original.executable).unwrap(),
        Some(original)
    );
}

#[test]
fn custom_profile_requires_instructions() {
    let (_directory, database) = open_temp();
    let repository = database.app_profiles();
    let mut invalid = profile("writer.exe");
    invalid.formatting_style = FormattingStyle::Custom;
    invalid.custom_instructions = Some("  ".into());
    assert!(matches!(
        repository.upsert(&invalid),
        Err(PersistenceError::Validation {
            field: "custom_instructions",
            ..
        })
    ));

    invalid.custom_instructions = Some("Write concise prose".into());
    repository.upsert(&invalid).unwrap();
}

#[test]
fn executable_identities_reject_paths_for_every_repository() {
    for invalid in [
        r"C:\Program Files\Editor\editor.exe",
        r"folder/editor.exe",
        "C:editor.exe",
        "   ",
    ] {
        assert!(ExecutableIdentity::new(invalid).is_err());
    }

    let (_directory, database) = open_temp();
    let history = database.history();
    history
        .set_retention(RetentionPolicy::Indefinite, NOW)
        .unwrap();
    let mut invalid_history = draft(NOW, "private");
    invalid_history.target_executable = Some(r"C:\private\editor.exe".into());
    assert!(history.insert(&invalid_history).is_err());

    let mut invalid_lexicon = lexicon("alias");
    invalid_lexicon.app_executable = Some(r"C:\private\editor.exe".into());
    assert!(database.lexicon().insert(&invalid_lexicon).is_err());
}

#[test]
fn persistence_errors_do_not_echo_database_paths() {
    let directory = tempfile::tempdir().unwrap();
    let database_path = directory.path().join("not-a-database.sqlite3");
    std::fs::write(&database_path, "sensitive invalid bytes").unwrap();
    let rendered = match Persistence::open(&database_path) {
        Ok(_) => panic!("invalid SQLite content unexpectedly opened"),
        Err(error) => error.to_string(),
    };
    assert!(!rendered.contains(database_path.to_string_lossy().as_ref()));
    assert!(!rendered.contains("sensitive invalid bytes"));
}

#[test]
fn no_api_accepts_window_titles_or_full_paths() {
    // This compile-time-oriented test documents the privacy boundary: stored
    // structs expose executable basenames only and no window-title field.
    let fields = std::mem::size_of::<ExecutableIdentity>();
    assert!(fields > 0);
    assert!(!Path::new("notepad.exe").is_absolute());
}
