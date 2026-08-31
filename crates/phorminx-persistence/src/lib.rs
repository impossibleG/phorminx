//! Local, privacy-preserving persistence for Phorminx.
//!
//! The crate intentionally has no logging dependency. Database errors are returned
//! without augmenting them with database paths, dictated content, or window titles.

mod history;
mod lexicon;
mod migration;
mod profile;

use std::{fs, path::Path, time::Duration};

pub use history::{
    DictationDraft, DictationRecord, HistoryRepository, RetentionPolicy, TimingMetadata,
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
