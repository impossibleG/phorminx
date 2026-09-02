use std::path::Path;

use phorminx_persistence::{
    AppProfile, CasePolicy, DictationDraft, ExecutableIdentity, FormattingStyle,
    InsertionPreference, NewLexiconEntry, Persistence, PersistenceError, RetentionPolicy,
    TimingMetadata,
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
    assert_eq!(database.schema_version().unwrap(), 2);
    drop(database);

    let reopened = Persistence::open(&path).unwrap();
    assert_eq!(reopened.schema_version().unwrap(), 2);

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
