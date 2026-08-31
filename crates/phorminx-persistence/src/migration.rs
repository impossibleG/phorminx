use rusqlite::Connection;

use crate::Result;

const LATEST_VERSION: u32 = 1;

const MIGRATION_1: &str = r#"
CREATE TABLE dictation_history (
    id                    INTEGER PRIMARY KEY AUTOINCREMENT,
    created_at_ms         INTEGER NOT NULL,
    raw_text              TEXT NOT NULL,
    normalized_text       TEXT,
    cleaned_text          TEXT,
    selected_output       TEXT NOT NULL,
    language              TEXT,
    target_executable     TEXT,
    audio_duration_ms     INTEGER,
    stt_duration_ms       INTEGER,
    formatting_duration_ms INTEGER,
    insertion_duration_ms INTEGER,
    warnings_json         TEXT NOT NULL DEFAULT '[]'
);
CREATE INDEX dictation_history_created_at_idx
    ON dictation_history(created_at_ms DESC, id DESC);

CREATE TABLE persistence_settings (
    key   TEXT PRIMARY KEY NOT NULL,
    value TEXT NOT NULL
);
INSERT INTO persistence_settings(key, value) VALUES ('history_retention', 'disabled');

CREATE TABLE lexicon_entries (
    id                    INTEGER PRIMARY KEY AUTOINCREMENT,
    canonical             TEXT NOT NULL,
    alias                 TEXT NOT NULL COLLATE BINARY,
    language              TEXT,
    app_executable        TEXT,
    case_policy           TEXT NOT NULL,
    enabled               INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1))
);
CREATE UNIQUE INDEX lexicon_entry_scope_idx ON lexicon_entries(
    alias COLLATE BINARY,
    COALESCE(language, ''),
    COALESCE(app_executable, '') COLLATE NOCASE
);
CREATE INDEX lexicon_enabled_alias_idx
    ON lexicon_entries(enabled, alias COLLATE BINARY);

CREATE TABLE app_profiles (
    executable            TEXT PRIMARY KEY COLLATE NOCASE,
    formatting_style      TEXT NOT NULL,
    custom_instructions   TEXT,
    language              TEXT,
    insertion_preference  TEXT NOT NULL,
    deny                  INTEGER NOT NULL DEFAULT 0 CHECK (deny IN (0, 1))
);
"#;

pub(crate) fn apply(connection: &Connection) -> Result<()> {
    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations (\
             version INTEGER PRIMARY KEY NOT NULL, \
             applied_at_ms INTEGER NOT NULL\
         );",
    )?;

    let current = current_version(connection)?;
    if current < 1 {
        let transaction = connection.unchecked_transaction()?;
        transaction.execute_batch(MIGRATION_1)?;
        transaction.execute(
            "INSERT INTO schema_migrations(version, applied_at_ms) \
             VALUES (1, unixepoch('subsec') * 1000)",
            [],
        )?;
        transaction.pragma_update(None, "user_version", 1)?;
        transaction.commit()?;
    }

    Ok(())
}

pub(crate) fn current_version(connection: &Connection) -> Result<u32> {
    let exists: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master \
         WHERE type = 'table' AND name = 'schema_migrations')",
        [],
        |row| row.get(0),
    )?;
    if !exists {
        return Ok(0);
    }

    let version: u32 = connection.query_row(
        "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
        [],
        |row| row.get(0),
    )?;
    if version > LATEST_VERSION {
        return Err(crate::PersistenceError::Validation {
            field: "schema_version",
            reason: "database was created by a newer Phorminx version",
        });
    }
    Ok(version)
}
