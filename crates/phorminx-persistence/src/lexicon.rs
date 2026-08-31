use rusqlite::{Connection, OptionalExtension, params};

use crate::{PersistenceError, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CasePolicy {
    PreserveInput,
    UseCanonical,
    Lowercase,
    Uppercase,
}

impl CasePolicy {
    fn as_db(self) -> &'static str {
        match self {
            Self::PreserveInput => "preserve_input",
            Self::UseCanonical => "canonical",
            Self::Lowercase => "lowercase",
            Self::Uppercase => "uppercase",
        }
    }

    fn from_db(value: &str) -> rusqlite::Result<Self> {
        match value {
            "preserve_input" => Ok(Self::PreserveInput),
            "canonical" => Ok(Self::UseCanonical),
            "lowercase" => Ok(Self::Lowercase),
            "uppercase" => Ok(Self::Uppercase),
            _ => Err(rusqlite::Error::InvalidColumnType(
                5,
                "case_policy".into(),
                rusqlite::types::Type::Text,
            )),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewLexiconEntry {
    pub canonical: String,
    /// Matched with SQLite `BINARY` equality; no fuzzy or case-folded matching.
    pub alias: String,
    pub language: Option<String>,
    pub app_executable: Option<String>,
    pub case_policy: CasePolicy,
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LexiconEntry {
    pub id: i64,
    pub entry: NewLexiconEntry,
}

pub struct LexiconRepository<'connection> {
    connection: &'connection Connection,
}

impl<'connection> LexiconRepository<'connection> {
    pub(crate) fn new(connection: &'connection Connection) -> Self {
        Self { connection }
    }

    pub fn insert(&self, entry: &NewLexiconEntry) -> Result<i64> {
        validate(entry)?;
        self.connection.execute(
            "INSERT INTO lexicon_entries(\
                 canonical, alias, language, app_executable, case_policy, enabled\
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                entry.canonical,
                entry.alias,
                entry.language,
                entry.app_executable,
                entry.case_policy.as_db(),
                entry.enabled,
            ],
        )?;
        Ok(self.connection.last_insert_rowid())
    }

    pub fn update(&self, id: i64, entry: &NewLexiconEntry) -> Result<bool> {
        validate(entry)?;
        Ok(self.connection.execute(
            "UPDATE lexicon_entries SET canonical = ?2, alias = ?3, language = ?4, \
                    app_executable = ?5, case_policy = ?6, enabled = ?7 WHERE id = ?1",
            params![
                id,
                entry.canonical,
                entry.alias,
                entry.language,
                entry.app_executable,
                entry.case_policy.as_db(),
                entry.enabled,
            ],
        )? > 0)
    }

    pub fn set_enabled(&self, id: i64, enabled: bool) -> Result<bool> {
        Ok(self.connection.execute(
            "UPDATE lexicon_entries SET enabled = ?2 WHERE id = ?1",
            params![id, enabled],
        )? > 0)
    }

    pub fn delete(&self, id: i64) -> Result<bool> {
        Ok(self
            .connection
            .execute("DELETE FROM lexicon_entries WHERE id = ?1", [id])?
            > 0)
    }

    pub fn get(&self, id: i64) -> Result<Option<LexiconEntry>> {
        Ok(self
            .connection
            .query_row(
                "SELECT id, canonical, alias, language, app_executable, case_policy, enabled \
                 FROM lexicon_entries WHERE id = ?1",
                [id],
                map_entry,
            )
            .optional()?)
    }

    pub fn list(&self) -> Result<Vec<LexiconEntry>> {
        let mut statement = self.connection.prepare(
            "SELECT id, canonical, alias, language, app_executable, case_policy, enabled \
             FROM lexicon_entries ORDER BY alias COLLATE BINARY, id",
        )?;
        Ok(statement
            .query_map([], map_entry)?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Returns enabled entries whose exact alias and optional scopes apply.
    /// More specific app/language entries are ordered first.
    pub fn exact_matches(
        &self,
        alias: &str,
        language: Option<&str>,
        app_executable: Option<&str>,
    ) -> Result<Vec<LexiconEntry>> {
        let mut statement = self.connection.prepare(
            "SELECT id, canonical, alias, language, app_executable, case_policy, enabled \
             FROM lexicon_entries \
             WHERE enabled = 1 AND alias = ?1 COLLATE BINARY \
               AND (language IS NULL OR language = ?2) \
               AND (app_executable IS NULL OR app_executable = ?3 COLLATE NOCASE) \
             ORDER BY (app_executable IS NOT NULL) DESC, (language IS NOT NULL) DESC, id",
        )?;
        Ok(statement
            .query_map(params![alias, language, app_executable], map_entry)?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }
}

fn validate(entry: &NewLexiconEntry) -> Result<()> {
    if entry.canonical.trim().is_empty() {
        return Err(PersistenceError::Validation {
            field: "canonical",
            reason: "must not be empty",
        });
    }
    if entry.alias.is_empty() {
        return Err(PersistenceError::Validation {
            field: "alias",
            reason: "must not be empty",
        });
    }
    if let Some(executable) = &entry.app_executable {
        super::profile::validate_executable(executable)?;
    }
    Ok(())
}

fn map_entry(row: &rusqlite::Row<'_>) -> rusqlite::Result<LexiconEntry> {
    Ok(LexiconEntry {
        id: row.get(0)?,
        entry: NewLexiconEntry {
            canonical: row.get(1)?,
            alias: row.get(2)?,
            language: row.get(3)?,
            app_executable: row.get(4)?,
            case_policy: CasePolicy::from_db(&row.get::<_, String>(5)?)?,
            enabled: row.get(6)?,
        },
    })
}
