//! Named, reusable instruction prompts with a version for every save.
//!
//! Instruction fields elsewhere refer to a prompt by id. They always use the
//! latest version, and a deleted prompt leaves its id unresolved so callers
//! can show it as missing instead of silently dropping text.

use rusqlite::{Connection, OptionalExtension, params};

use crate::{PersistenceError, Result};

pub(crate) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS prompts (
    id             INTEGER PRIMARY KEY,
    name           TEXT NOT NULL COLLATE NOCASE UNIQUE,
    tags_json      TEXT NOT NULL DEFAULT '[]',
    created_at_ms  INTEGER NOT NULL,
    updated_at_ms  INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS prompt_versions (
    prompt_id      INTEGER NOT NULL REFERENCES prompts(id) ON DELETE CASCADE,
    version        INTEGER NOT NULL,
    name           TEXT NOT NULL,
    body           TEXT NOT NULL,
    created_at_ms  INTEGER NOT NULL,
    PRIMARY KEY (prompt_id, version)
);
";

pub const MAX_PROMPT_NAME_CHARS: usize = 80;
pub const MAX_PROMPT_BODY_CHARS: usize = 4_096;
pub const MAX_PROMPT_TAGS: usize = 12;
pub const MAX_PROMPT_TAG_CHARS: usize = 32;

/// What a caller saves: a new prompt, or the next version of an existing one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptDraft {
    pub name: String,
    pub tags: Vec<String>,
    pub body: String,
}

/// A prompt at its latest version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prompt {
    pub id: i64,
    pub name: String,
    pub tags: Vec<String>,
    pub body: String,
    pub version: i64,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptVersion {
    pub version: i64,
    pub name: String,
    pub body: String,
    pub created_at_ms: i64,
}

pub struct PromptRepository<'connection> {
    connection: &'connection Connection,
}

impl<'connection> PromptRepository<'connection> {
    pub(crate) fn new(connection: &'connection Connection) -> Self {
        Self { connection }
    }

    /// Creates a prompt with its first version.
    pub fn create(&self, draft: &PromptDraft, now_ms: i64) -> Result<i64> {
        let draft = normalized(draft)?;
        let transaction = self.connection.unchecked_transaction()?;
        transaction
            .execute(
                "INSERT INTO prompts(name, tags_json, created_at_ms, updated_at_ms) \
                 VALUES (?1, ?2, ?3, ?3)",
                params![draft.name, serde_json::to_string(&draft.tags)?, now_ms],
            )
            .map_err(duplicate_name)?;
        let id = transaction.last_insert_rowid();
        insert_version(&transaction, id, 1, &draft, now_ms)?;
        transaction.commit()?;
        Ok(id)
    }

    /// Saves a new version. Returns false when the prompt no longer exists.
    pub fn save(&self, id: i64, draft: &PromptDraft, now_ms: i64) -> Result<bool> {
        let draft = normalized(draft)?;
        let transaction = self.connection.unchecked_transaction()?;
        let updated = transaction
            .execute(
                "UPDATE prompts SET name = ?2, tags_json = ?3, updated_at_ms = ?4 WHERE id = ?1",
                params![id, draft.name, serde_json::to_string(&draft.tags)?, now_ms],
            )
            .map_err(duplicate_name)?;
        if updated == 0 {
            return Ok(false);
        }
        let next: i64 = transaction.query_row(
            "SELECT COALESCE(MAX(version), 0) + 1 FROM prompt_versions WHERE prompt_id = ?1",
            [id],
            |row| row.get(0),
        )?;
        insert_version(&transaction, id, next, &draft, now_ms)?;
        transaction.commit()?;
        Ok(true)
    }

    /// Makes an older version current again by saving it as the newest one,
    /// so the history itself is never rewritten.
    pub fn restore(&self, id: i64, version: i64, now_ms: i64) -> Result<bool> {
        let Some(old) = self
            .versions(id)?
            .into_iter()
            .find(|candidate| candidate.version == version)
        else {
            return Ok(false);
        };
        let Some(current) = self.get(id)? else {
            return Ok(false);
        };
        self.save(
            id,
            &PromptDraft {
                name: old.name,
                tags: current.tags,
                body: old.body,
            },
            now_ms,
        )
    }

    pub fn delete(&self, id: i64) -> Result<bool> {
        Ok(self
            .connection
            .execute("DELETE FROM prompts WHERE id = ?1", [id])?
            > 0)
    }

    pub fn get(&self, id: i64) -> Result<Option<Prompt>> {
        Ok(self
            .connection
            .query_row(
                &format!("{SELECT_LATEST} WHERE p.id = ?1"),
                [id],
                prompt_row,
            )
            .optional()?)
    }

    /// Every prompt at its latest version, by name.
    pub fn list(&self) -> Result<Vec<Prompt>> {
        let mut statement = self
            .connection
            .prepare(&format!("{SELECT_LATEST} ORDER BY p.name COLLATE NOCASE"))?;
        let rows = statement.query_map([], prompt_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Newest first.
    pub fn versions(&self, id: i64) -> Result<Vec<PromptVersion>> {
        let mut statement = self.connection.prepare(
            "SELECT version, name, body, created_at_ms FROM prompt_versions \
             WHERE prompt_id = ?1 ORDER BY version DESC",
        )?;
        let rows = statement.query_map([id], |row| {
            Ok(PromptVersion {
                version: row.get(0)?,
                name: row.get(1)?,
                body: row.get(2)?,
                created_at_ms: row.get(3)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }
}

const SELECT_LATEST: &str = "SELECT p.id, p.name, p.tags_json, v.body, v.version, \
     p.created_at_ms, p.updated_at_ms FROM prompts p \
     JOIN prompt_versions v ON v.prompt_id = p.id \
     AND v.version = (SELECT MAX(version) FROM prompt_versions WHERE prompt_id = p.id)";

fn prompt_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Prompt> {
    let tags_json: String = row.get(2)?;
    Ok(Prompt {
        id: row.get(0)?,
        name: row.get(1)?,
        tags: serde_json::from_str(&tags_json).unwrap_or_default(),
        body: row.get(3)?,
        version: row.get(4)?,
        created_at_ms: row.get(5)?,
        updated_at_ms: row.get(6)?,
    })
}

fn insert_version(
    connection: &Connection,
    id: i64,
    version: i64,
    draft: &PromptDraft,
    now_ms: i64,
) -> Result<()> {
    connection.execute(
        "INSERT INTO prompt_versions(prompt_id, version, name, body, created_at_ms) \
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![id, version, draft.name, draft.body, now_ms],
    )?;
    Ok(())
}

fn duplicate_name(error: rusqlite::Error) -> PersistenceError {
    match &error {
        rusqlite::Error::SqliteFailure(failure, _)
            if failure.code == rusqlite::ErrorCode::ConstraintViolation =>
        {
            PersistenceError::Validation {
                field: "prompt_name",
                reason: "another prompt already has this name",
            }
        }
        _ => PersistenceError::Database(error),
    }
}

fn normalized(draft: &PromptDraft) -> Result<PromptDraft> {
    let name = draft.name.split_whitespace().collect::<Vec<_>>().join(" ");
    if name.is_empty() {
        return Err(PersistenceError::Validation {
            field: "prompt_name",
            reason: "a prompt needs a name",
        });
    }
    if name.chars().count() > MAX_PROMPT_NAME_CHARS {
        return Err(PersistenceError::Validation {
            field: "prompt_name",
            reason: "the name is too long",
        });
    }
    // Instruction fields refer to prompts as `@[Name]`.
    if name.contains(['[', ']']) {
        return Err(PersistenceError::Validation {
            field: "prompt_name",
            reason: "a prompt name cannot contain [ or ]",
        });
    }
    if draft.body.trim().is_empty() {
        return Err(PersistenceError::Validation {
            field: "prompt_body",
            reason: "a prompt needs instructions",
        });
    }
    if draft.body.chars().count() > MAX_PROMPT_BODY_CHARS {
        return Err(PersistenceError::Validation {
            field: "prompt_body",
            reason: "the instructions are too long",
        });
    }
    let mut tags = Vec::new();
    for tag in &draft.tags {
        let tag = tag.trim().trim_start_matches('#').to_lowercase();
        if tag.is_empty() || tags.contains(&tag) {
            continue;
        }
        if tag.chars().count() > MAX_PROMPT_TAG_CHARS {
            return Err(PersistenceError::Validation {
                field: "prompt_tags",
                reason: "a tag is too long",
            });
        }
        tags.push(tag);
    }
    if tags.len() > MAX_PROMPT_TAGS {
        return Err(PersistenceError::Validation {
            field: "prompt_tags",
            reason: "too many tags",
        });
    }
    Ok(PromptDraft {
        name,
        tags,
        body: draft.body.trim().to_owned(),
    })
}
