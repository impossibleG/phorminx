//! Local, privacy-preserving persistence for Phorminx.
//!
//! The crate intentionally has no logging dependency. Database errors are returned
//! without augmenting them with database paths, dictated content, or window titles.

mod history;
mod lexicon;
mod library;
pub use library::{
    EmbeddedPassage, LibraryIndexStats, LibraryPassage, LibraryRepository, LibrarySearchHit,
};
mod meeting;
mod meeting_memory;
pub use meeting_memory::{MeetingMemoryPassage, MeetingMemoryRepository, MeetingTitleSource};
mod migration;
pub use meeting::{
    MeetingMessageRecord, MeetingMessageStatus, MeetingQuestionContext, MeetingRecord,
    MeetingRepository, MeetingSegmentRecord,
};
mod profile;
mod recovery;
pub use recovery::{RecoveryRecord, RecoveryRepository};

use std::ffi::OsString;
use std::{fs, path::Path, path::PathBuf, time::Duration};

pub use history::{
    DictationDraft, DictationRecord, HISTORY_PREVIEW_MAX_CHARS, HistoryRepository, HistorySummary,
    HistoryTextVariant, HistoryVariantAvailability, MAX_TERMINAL_TEXT_BYTES,
    MAX_TERMINAL_WARNING_BYTES, MAX_TERMINAL_WARNINGS, RetentionPolicy, TerminalMetadata,
    TimingMetadata,
};
pub use lexicon::{CasePolicy, LexiconEntry, LexiconRepository, NewLexiconEntry};
pub use profile::{
    AppProfile, AppProfileRepository, ExecutableIdentity, FormattingStyle, InsertionPreference,
};
use rusqlite::{Connection, functions::FunctionFlags, types::ValueRef};
use thiserror::Error;

pub type Result<T> = std::result::Result<T, PersistenceError>;

#[derive(Debug, Error)]
pub enum PersistenceError {
    #[error("interrupted text could not be protected or recovered for this Windows account")]
    Protection,
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

/// Explicit deletion intent. Every category defaults to preserved.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DeleteSelection {
    pub dictations: bool,
    pub meeting_transcripts: bool,
    pub chats: bool,
}

/// Cross-thread cancellation of an isolated background connection. Never share
/// this handle with the production recording/history-write connection.
pub struct PersistenceInterrupt(rusqlite::InterruptHandle);
impl PersistenceInterrupt {
    pub fn interrupt(&self) {
        self.0.interrupt();
    }
}

impl Persistence {
    /// Deletes only explicitly selected categories in one transaction. The host
    /// must first quiesce meeting/chat writers; dictation recovery uses its epoch.
    pub fn delete_selected(&self, selection: DeleteSelection) -> Result<()> {
        if selection == DeleteSelection::default() {
            return Ok(());
        }
        let tx = self.connection.unchecked_transaction()?;
        if selection.dictations {
            tx.execute("DELETE FROM dictation_history", [])?;
            tx.execute("DELETE FROM interrupted_dictation", [])?;
            tx.execute("DELETE FROM recovery_tombstones", [])?;
            tx.execute("UPDATE persistence_settings SET value=CAST(value AS INTEGER)+1 WHERE key='recovery_epoch'", [])?;
        }
        if selection.meeting_transcripts {
            tx.execute("DELETE FROM meeting_segments", [])?;
            tx.execute(
                "UPDATE meeting_sessions SET next_sequence=0,committed_sample=0",
                [],
            )?;
        }
        if selection.chats {
            tx.execute("DELETE FROM meeting_messages", [])?;
        }
        if selection.meeting_transcripts || selection.chats {
            tx.execute("DELETE FROM meeting_sessions WHERE NOT EXISTS(SELECT 1 FROM meeting_segments WHERE session_id=meeting_sessions.id) AND NOT EXISTS(SELECT 1 FROM meeting_messages WHERE session_id=meeting_sessions.id)", [])?;
        }
        tx.commit()?;
        Ok(())
    }
    pub fn interrupt_handle(&self) -> PersistenceInterrupt {
        PersistenceInterrupt(self.connection.get_interrupt_handle())
    }
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_with_busy_timeout(path, Duration::from_secs(5))
    }

    /// Opens a database with a caller-selected upper bound for lock waits.
    ///
    /// Interactive background readers use a shorter timeout so shutdown and
    /// navigation cancellation cannot be held hostage by another connection.
    pub fn open_with_busy_timeout(path: impl AsRef<Path>, busy_timeout: Duration) -> Result<Self> {
        let path = path.as_ref();
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent).map_err(PersistenceError::CreateDirectory)?;
        }
        remove_obsolete_rollback_artifact(path)?;

        let connection = Connection::open(path)?;
        register_read_guards(&connection)?;
        connection.busy_timeout(busy_timeout)?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.pragma_update(None, "synchronous", "NORMAL")?;
        migration::apply(&connection)?;

        Ok(Self { connection })
    }

    pub fn history(&self) -> HistoryRepository<'_> {
        HistoryRepository::new(&self.connection)
    }

    pub fn library(&self) -> LibraryRepository<'_> {
        LibraryRepository::new(&self.connection)
    }

    pub fn meetings(&self) -> MeetingRepository<'_> {
        MeetingRepository::new(&self.connection)
    }

    pub fn meeting_memory(&self) -> MeetingMemoryRepository<'_> {
        MeetingMemoryRepository::new(&self.connection)
    }

    pub fn recovery(&self) -> RecoveryRepository<'_> {
        RecoveryRepository::new(&self.connection)
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

fn register_read_guards(connection: &Connection) -> Result<()> {
    connection.create_scalar_function(
        "phorminx_find",
        2,
        FunctionFlags::SQLITE_DETERMINISTIC | FunctionFlags::SQLITE_INNOCUOUS,
        |context| {
            let text = context.get_raw(0).as_str()?;
            let query = context.get_raw(1).as_str()?;
            Ok(library::unicode_match_offset(text, query) as i64)
        },
    )?;
    connection.create_scalar_function(
        "phorminx_is_valid_text",
        1,
        FunctionFlags::SQLITE_DETERMINISTIC | FunctionFlags::SQLITE_INNOCUOUS,
        |context| {
            Ok(matches!(
                context.get_raw(0),
                ValueRef::Text(bytes) if std::str::from_utf8(bytes).is_ok()
            ))
        },
    )?;
    Ok(())
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
