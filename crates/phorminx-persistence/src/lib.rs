//! Local, privacy-preserving persistence for Phorminx.
//!
//! The crate intentionally has no logging dependency. Database errors are returned
//! without augmenting them with database paths, dictated content, or window titles.

mod history;
mod lexicon;
mod migration;
mod profile;

use std::ffi::OsString;
use std::{fs, path::Path, path::PathBuf, time::Duration};

pub use history::{
    DictationDraft, DictationRecord, HISTORY_PREVIEW_MAX_CHARS, HistoryRepository, HistorySummary,
    HistoryTextVariant, MAX_TERMINAL_TEXT_BYTES, MAX_TERMINAL_WARNING_BYTES, MAX_TERMINAL_WARNINGS,
    RetentionPolicy, TerminalMetadata, TimingMetadata,
};
pub use lexicon::{CasePolicy, LexiconEntry, LexiconRepository, NewLexiconEntry};
pub use profile::{
    AppProfile, AppProfileRepository, ExecutableIdentity, FormattingStyle, InsertionPreference,
};
use rusqlite::Connection;
use thiserror::Error;

pub type Result<T> = std::result::Result<T, PersistenceError>;

#[derive(Debug, Error)]
pub enum PersistenceError {
    #[error("the database parent directory could not be created")]
    CreateDirectory(#[source] std::io::Error),
    #[error("the local database operation failed")]
    Database(#[from] rusqlite::Error),
    #[error("stored data could not be decoded")]
    Decode(#[from] serde_json::Error),
    #[error("invalid {field}: {reason}")]
    Validation {
        field: &'static str,
        reason: &'static str,
    },
    #[error("{field} exceeds its terminal text limit ({actual_bytes} bytes; maximum {max_bytes})")]
    TextLimitExceeded {
        field: &'static str,
        max_bytes: usize,
        actual_bytes: usize,
    },
    #[error("{field} contains too many items ({actual_items}; maximum {max_items})")]
    CollectionLimitExceeded {
        field: &'static str,
        max_items: usize,
        actual_items: usize,
    },
    #[error("an obsolete privacy-unsafe rollback artifact could not be removed")]
    PrivacyCleanup(#[source] std::io::Error),
}

/// A connection to the per-user Phorminx database.
///
/// `Persistence::open` creates the parent directory, enables WAL and foreign-key
/// enforcement, and applies every known schema migration transactionally.
pub struct Persistence {
    connection: Connection,
}

impl Persistence {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent).map_err(PersistenceError::CreateDirectory)?;
        }
        remove_obsolete_rollback_artifact(path)?;

        let connection = Connection::open(path)?;
        connection.busy_timeout(Duration::from_secs(5))?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.pragma_update(None, "synchronous", "NORMAL")?;
        migration::apply(&connection)?;

        Ok(Self { connection })
    }

    pub fn history(&self) -> HistoryRepository<'_> {
        HistoryRepository::new(&self.connection)
    }

    pub fn lexicon(&self) -> LexiconRepository<'_> {
        LexiconRepository::new(&self.connection)
    }

    pub fn app_profiles(&self) -> AppProfileRepository<'_> {
        AppProfileRepository::new(&self.connection)
    }

    pub fn schema_version(&self) -> Result<u32> {
        migration::current_version(&self.connection)
    }
}

fn obsolete_rollback_artifact_path(database_path: &Path) -> PathBuf {
    let mut name = database_path
        .file_name()
        .map_or_else(|| OsString::from("phorminx.db"), OsString::from);
    name.push(".schema-1.backup");
    database_path.with_file_name(name)
}

fn remove_obsolete_rollback_artifact(database_path: &Path) -> Result<()> {
    let artifact = obsolete_rollback_artifact_path(database_path);
    let mut artifacts = vec![artifact.clone()];
    for suffix in ["-wal", "-shm"] {
        let mut name = artifact.as_os_str().to_os_string();
        name.push(suffix);
        artifacts.push(PathBuf::from(name));
    }
    if let Some(directory) = database_path.parent() {
        let database_name = database_path
            .file_name()
            .map_or_else(|| "phorminx.db".into(), |name| name.to_string_lossy());
        let temporary_prefix = format!("{database_name}.schema-1-backup-");
        for entry in fs::read_dir(directory).map_err(PersistenceError::PrivacyCleanup)? {
            let entry = entry.map_err(PersistenceError::PrivacyCleanup)?;
            let name = entry.file_name();
            if name.to_string_lossy().starts_with(&temporary_prefix) {
                artifacts.push(entry.path());
            }
        }
    }
    for artifact in artifacts {
        if artifact.exists() {
            fs::remove_file(artifact).map_err(PersistenceError::PrivacyCleanup)?;
        }
    }
    Ok(())
}
