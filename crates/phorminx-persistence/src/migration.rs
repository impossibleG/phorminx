use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior};

use crate::Result;

// History metadata extensions are deliberately additive and keep schema version 1.
// Older Phorminx binaries use explicit column lists, so nullable extra columns
// are backward-compatible for both reads and writes.
const LATEST_VERSION: u32 = 1;
const TRANSITIONAL_ADDITIVE_VERSION: u32 = 2;

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

const ADDITIVE_HISTORY_COLUMNS: [(&str, &str); 8] = [
    (
        "audio_finalization_duration_ms",
        "ALTER TABLE dictation_history ADD COLUMN audio_finalization_duration_ms INTEGER",
    ),
    (
        "worker_queue_duration_ms",
        "ALTER TABLE dictation_history ADD COLUMN worker_queue_duration_ms INTEGER",
    ),
    (
        "release_to_insert_duration_ms",
        "ALTER TABLE dictation_history ADD COLUMN release_to_insert_duration_ms INTEGER",
    ),
    (
        "checkpoint_count",
        "ALTER TABLE dictation_history ADD COLUMN checkpoint_count INTEGER",
    ),
    (
        "checkpoint_repair_count",
        "ALTER TABLE dictation_history ADD COLUMN checkpoint_repair_count INTEGER",
    ),
    (
        "peak_retained_audio_ms",
        "ALTER TABLE dictation_history ADD COLUMN peak_retained_audio_ms INTEGER",
    ),
    (
        "formatting_chunk_count",
        "ALTER TABLE dictation_history ADD COLUMN formatting_chunk_count INTEGER",
    ),
    (
        "auto_stopped",
        "ALTER TABLE dictation_history ADD COLUMN auto_stopped INTEGER CHECK (auto_stopped IN (0, 1))",
    ),
];

const RECOVERY_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS interrupted_dictation (session TEXT PRIMARY KEY, updated_at_ms INTEGER NOT NULL, protected_text BLOB NOT NULL); INSERT OR IGNORE INTO persistence_settings(key,value) VALUES ('recovery_epoch','0');";
const RECOVERY_TOMBSTONES_SCHEMA: &str =
    "CREATE TABLE IF NOT EXISTS recovery_tombstones (session TEXT PRIMARY KEY);";

/// Applied, in order, on every open that finds the schema out of date.
const IDEMPOTENT_SCHEMAS: [&str; 11] = [
    RECOVERY_SCHEMA,
    RECOVERY_TOMBSTONES_SCHEMA,
    crate::library::SCHEMA,
    crate::meeting::SCHEMA,
    crate::meeting::TRANSLATION_SCHEMA,
    crate::meeting_memory::SCHEMA,
    crate::prompt::SCHEMA,
    crate::investigation::SCHEMA,
    crate::messaging::SCHEMA,
    crate::feedback::SCHEMA,
    crate::meeting_plan::SCHEMA,
];

/// Brings the database up to the current schema.
///
/// Every component opens its own connection at startup, several of them with
/// lock waits of 50 ms. Writing on every open made those opens race each other
/// and fail with SQLITE_BUSY, so an already current database is only read here,
/// and a database that needs changes takes the write lock up front, where the
/// busy timeout applies, instead of upgrading a read transaction mid-way.
pub(crate) fn apply(connection: &Connection) -> Result<()> {
    if schema_is_current(connection)? {
        return Ok(());
    }

    let transaction = Transaction::new_unchecked(connection, TransactionBehavior::Immediate)?;
    transaction.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations (             version INTEGER PRIMARY KEY NOT NULL,              applied_at_ms INTEGER NOT NULL         );",
    )?;

    let current = current_version(&transaction)?;
    if current < 1 {
        transaction.execute_batch(MIGRATION_1)?;
        transaction.execute(
            "INSERT INTO schema_migrations(version, applied_at_ms)              VALUES (1, unixepoch('subsec') * 1000)",
            [],
        )?;
    }

    for (name, statement) in ADDITIVE_HISTORY_COLUMNS {
        let exists: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_info('dictation_history') WHERE name = ?1)",
            [name],
            |row| row.get(0),
        )?;
        if !exists {
            transaction.execute_batch(statement)?;
        }
    }
    for schema in IDEMPOTENT_SCHEMAS {
        transaction.execute_batch(schema)?;
    }
    if current == TRANSITIONAL_ADDITIVE_VERSION {
        // Pre-release builds briefly labeled the same nullable extension as
        // schema 2. Normalize only that known shape so the previous binary can
        // open it, without copying or discarding any private user data.
        transaction.execute("DELETE FROM schema_migrations WHERE version = 2", [])?;
    }
    transaction.execute(
        "INSERT INTO persistence_settings(key, value) VALUES ('schema_fingerprint', ?1)          ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        [schema_fingerprint()],
    )?;
    transaction.pragma_update(None, "user_version", LATEST_VERSION)?;
    transaction.commit()?;

    Ok(())
}

/// True when this binary's whole schema was already applied, read without
/// taking a write lock. Any change to this file's schema statements, including
/// `IDEMPOTENT_SCHEMAS`, changes the fingerprint, so it is applied on next open.
fn schema_is_current(connection: &Connection) -> Result<bool> {
    let user_version: u32 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if user_version != LATEST_VERSION || current_version(connection)? != LATEST_VERSION {
        return Ok(false);
    }
    let stored: Option<String> = connection
        .query_row(
            "SELECT value FROM persistence_settings WHERE key = 'schema_fingerprint'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    Ok(stored.as_deref() == Some(schema_fingerprint().as_str()))
}

fn schema_fingerprint() -> String {
    // FNV-1a: stable across builds and platforms, unlike std's hasher.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let mut feed = |text: &str| {
        for byte in text.bytes().chain([0]) {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0100_0000_01b3);
        }
    };
    feed(&LATEST_VERSION.to_string());
    feed(MIGRATION_1);
    for (name, statement) in ADDITIVE_HISTORY_COLUMNS {
        feed(name);
        feed(statement);
    }
    for schema in IDEMPOTENT_SCHEMAS {
        feed(schema);
    }
    format!("{hash:016x}")
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
    if version > TRANSITIONAL_ADDITIVE_VERSION {
        return Err(crate::PersistenceError::Validation {
            field: "schema_version",
            reason: "database was created by a newer Phorminx version",
        });
    }
    Ok(version)
}
