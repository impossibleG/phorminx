use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::{PersistenceError, Result};

const HOUR_MS: i64 = 60 * 60 * 1_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetentionPolicy {
    /// Do not save future dictations. Existing records are removed immediately.
    Disabled,
    Hours24,
    Days7,
    Days30,
    Indefinite,
}

impl RetentionPolicy {
    fn as_db(self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::Hours24 => "24h",
            Self::Days7 => "7d",
            Self::Days30 => "30d",
            Self::Indefinite => "indefinite",
        }
    }

    fn from_db(value: &str) -> Result<Self> {
        match value {
            "disabled" => Ok(Self::Disabled),
            "24h" => Ok(Self::Hours24),
            "7d" => Ok(Self::Days7),
            "30d" => Ok(Self::Days30),
            "indefinite" => Ok(Self::Indefinite),
            _ => Err(PersistenceError::Validation {
                field: "history_retention",
                reason: "database contains an unsupported policy",
            }),
        }
    }

    fn max_age_ms(self) -> Option<i64> {
        match self {
            Self::Disabled | Self::Indefinite => None,
            Self::Hours24 => Some(24 * HOUR_MS),
            Self::Days7 => Some(7 * 24 * HOUR_MS),
            Self::Days30 => Some(30 * 24 * HOUR_MS),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimingMetadata {
    pub audio_duration_ms: Option<u64>,
    pub stt_duration_ms: Option<u64>,
    pub formatting_duration_ms: Option<u64>,
    pub insertion_duration_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DictationDraft {
    pub created_at_ms: i64,
    pub raw_text: String,
    pub normalized_text: Option<String>,
    pub cleaned_text: Option<String>,
    pub selected_output: String,
    pub language: Option<String>,
    /// Executable basename only. Window titles and filesystem paths are not stored.
    pub target_executable: Option<String>,
    pub timings: TimingMetadata,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DictationRecord {
    pub id: i64,
    pub dictation: DictationDraft,
}

pub struct HistoryRepository<'connection> {
    connection: &'connection Connection,
}

impl<'connection> HistoryRepository<'connection> {
    pub(crate) fn new(connection: &'connection Connection) -> Self {
        Self { connection }
    }

    pub fn retention(&self) -> Result<RetentionPolicy> {
        let value: String = self.connection.query_row(
            "SELECT value FROM persistence_settings WHERE key = 'history_retention'",
            [],
            |row| row.get(0),
        )?;
        RetentionPolicy::from_db(&value)
    }

    /// Changes retention. Disabling history clears it in the same transaction.
    pub fn set_retention(&self, policy: RetentionPolicy, now_ms: i64) -> Result<()> {
        let transaction = self.connection.unchecked_transaction()?;
        transaction.execute(
            "INSERT INTO persistence_settings(key, value) VALUES ('history_retention', ?1) \
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [policy.as_db()],
        )?;

        match policy {
            RetentionPolicy::Disabled => {
                transaction.execute("DELETE FROM dictation_history", [])?;
            }
            RetentionPolicy::Indefinite => {}
            bounded => {
                let cutoff = now_ms.saturating_sub(bounded.max_age_ms().unwrap_or_default());
                transaction.execute(
                    "DELETE FROM dictation_history WHERE created_at_ms < ?1",
                    [cutoff],
                )?;
            }
        }
        transaction.commit()?;
        Ok(())
    }

    /// Saves a dictation if history is enabled, returning `None` when disabled.
    pub fn insert(&self, draft: &DictationDraft) -> Result<Option<i64>> {
        validate_draft(draft)?;
        let retention = self.retention()?;
        if retention == RetentionPolicy::Disabled {
            return Ok(None);
        }

        let warnings = serde_json::to_string(&draft.warnings)?;
        let transaction = self.connection.unchecked_transaction()?;
        if let Some(max_age_ms) = retention.max_age_ms() {
            let cutoff = draft.created_at_ms.saturating_sub(max_age_ms);
            transaction.execute(
                "DELETE FROM dictation_history WHERE created_at_ms < ?1",
                [cutoff],
            )?;
        }
        transaction.execute(
            "INSERT INTO dictation_history(\
                 created_at_ms, raw_text, normalized_text, cleaned_text, selected_output, \
                 language, target_executable, audio_duration_ms, stt_duration_ms, \
                 formatting_duration_ms, insertion_duration_ms, warnings_json\
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                draft.created_at_ms,
                draft.raw_text,
                draft.normalized_text,
                draft.cleaned_text,
                draft.selected_output,
                draft.language,
                draft.target_executable,
                u64_to_i64(draft.timings.audio_duration_ms)?,
                u64_to_i64(draft.timings.stt_duration_ms)?,
                u64_to_i64(draft.timings.formatting_duration_ms)?,
                u64_to_i64(draft.timings.insertion_duration_ms)?,
                warnings,
            ],
        )?;
        let id = transaction.last_insert_rowid();
        transaction.commit()?;
        Ok(Some(id))
    }

    pub fn get(&self, id: i64) -> Result<Option<DictationRecord>> {
        let record = self
            .connection
            .query_row(
                "SELECT id, created_at_ms, raw_text, normalized_text, cleaned_text, \
                        selected_output, language, target_executable, audio_duration_ms, \
                        stt_duration_ms, formatting_duration_ms, insertion_duration_ms, warnings_json \
                 FROM dictation_history WHERE id = ?1",
                [id],
                map_record,
            )
            .optional()?;
        decode_record(record)
    }

    pub fn recent(&self, limit: usize) -> Result<Vec<DictationRecord>> {
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let mut statement = self.connection.prepare(
            "SELECT id, created_at_ms, raw_text, normalized_text, cleaned_text, \
                    selected_output, language, target_executable, audio_duration_ms, \
                    stt_duration_ms, formatting_duration_ms, insertion_duration_ms, warnings_json \
             FROM dictation_history ORDER BY created_at_ms DESC, id DESC LIMIT ?1",
        )?;
        let rows = statement.query_map([limit], map_record)?;
        rows.map(|row| decode_record(Some(row?)).map(Option::unwrap))
            .collect()
    }

    /// Applies the active bounded retention policy and returns removed rows.
    pub fn purge_expired(&self, now_ms: i64) -> Result<usize> {
        match self.retention()? {
            RetentionPolicy::Disabled => self.clear(),
            RetentionPolicy::Indefinite => Ok(0),
            policy => {
                let cutoff = now_ms.saturating_sub(policy.max_age_ms().unwrap_or_default());
                Ok(self.connection.execute(
                    "DELETE FROM dictation_history WHERE created_at_ms < ?1",
                    [cutoff],
                )?)
            }
        }
    }

    /// Immediately deletes all persisted dictations.
    pub fn clear(&self) -> Result<usize> {
        Ok(self
            .connection
            .execute("DELETE FROM dictation_history", [])?)
    }

    pub fn count(&self) -> Result<u64> {
        Ok(self
            .connection
            .query_row("SELECT COUNT(*) FROM dictation_history", [], |row| {
                row.get(0)
            })?)
    }
}

type EncodedRecord = (
    i64,
    i64,
    String,
    Option<String>,
    Option<String>,
    String,
    Option<String>,
    Option<String>,
    Option<i64>,
    Option<i64>,
    Option<i64>,
    Option<i64>,
    String,
);

fn map_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<EncodedRecord> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
        row.get(6)?,
        row.get(7)?,
        row.get(8)?,
        row.get(9)?,
        row.get(10)?,
        row.get(11)?,
        row.get(12)?,
    ))
}

fn decode_record(record: Option<EncodedRecord>) -> Result<Option<DictationRecord>> {
    let Some(record) = record else {
        return Ok(None);
    };
    Ok(Some(DictationRecord {
        id: record.0,
        dictation: DictationDraft {
            created_at_ms: record.1,
            raw_text: record.2,
            normalized_text: record.3,
            cleaned_text: record.4,
            selected_output: record.5,
            language: record.6,
            target_executable: record.7,
            timings: TimingMetadata {
                audio_duration_ms: i64_to_u64(record.8)?,
                stt_duration_ms: i64_to_u64(record.9)?,
                formatting_duration_ms: i64_to_u64(record.10)?,
                insertion_duration_ms: i64_to_u64(record.11)?,
            },
            warnings: serde_json::from_str(&record.12)?,
        },
    }))
}

fn validate_draft(draft: &DictationDraft) -> Result<()> {
    if draft.created_at_ms < 0 {
        return Err(PersistenceError::Validation {
            field: "created_at_ms",
            reason: "must be non-negative",
        });
    }
    if let Some(executable) = &draft.target_executable {
        super::profile::validate_executable(executable)?;
    }
    Ok(())
}

fn u64_to_i64(value: Option<u64>) -> Result<Option<i64>> {
    value
        .map(|value| {
            i64::try_from(value).map_err(|_| PersistenceError::Validation {
                field: "timing",
                reason: "exceeds the supported range",
            })
        })
        .transpose()
}

fn i64_to_u64(value: Option<i64>) -> Result<Option<u64>> {
    value
        .map(|value| {
            u64::try_from(value).map_err(|_| PersistenceError::Validation {
                field: "timing",
                reason: "database contains a negative value",
            })
        })
        .transpose()
}
