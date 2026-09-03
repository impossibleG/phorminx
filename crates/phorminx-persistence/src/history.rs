use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::{PersistenceError, Result};

const HOUR_MS: i64 = 60 * 60 * 1_000;

/// Maximum UTF-8 payload accepted for each terminal transcript variant.
///
/// This matches the default transcript-ledger text budget. Enforcing it here
/// keeps history writes bounded even when a caller did not use that ledger.
pub const MAX_TERMINAL_TEXT_BYTES: usize = 16 * 1024 * 1024;
/// Maximum Unicode scalar values returned in a history-list preview.
pub const HISTORY_PREVIEW_MAX_CHARS: usize = 240;
/// Maximum number of content-free warnings retained for one terminal record.
pub const MAX_TERMINAL_WARNINGS: usize = 64;
/// Maximum UTF-8 payload accepted for one content-free warning.
pub const MAX_TERMINAL_WARNING_BYTES: usize = 1_024;
const MAX_WARNINGS_JSON_BYTES: usize = MAX_TERMINAL_WARNINGS * (MAX_TERMINAL_WARNING_BYTES + 8);
const OVERSIZED_WARNINGS_NOTICE: &str = "stored warning metadata exceeds the current display limit";

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
    pub audio_finalization_duration_ms: Option<u64>,
    pub worker_queue_duration_ms: Option<u64>,
    pub release_to_insert_duration_ms: Option<u64>,
}

/// Content-free terminal facts emitted by the extended-dictation pipeline.
///
/// Every field is optional so records written by older binaries remain
/// distinguishable from sessions that observed a value of zero or `false`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalMetadata {
    pub checkpoint_count: Option<u64>,
    pub checkpoint_repair_count: Option<u64>,
    pub peak_retained_audio_ms: Option<u64>,
    pub formatting_chunk_count: Option<u64>,
    pub auto_stopped: Option<bool>,
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

/// A bounded row for history-list rendering.
///
/// The query producing this type never selects a full transcript variant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistorySummary {
    pub id: i64,
    pub created_at_ms: i64,
    pub selected_output_preview: String,
    pub selected_output_chars: u64,
    pub preview_truncated: bool,
    pub language: Option<String>,
    pub target_executable: Option<String>,
    pub timings: TimingMetadata,
    pub warnings: Vec<String>,
    pub terminal: TerminalMetadata,
    pub variants: HistoryVariantAvailability,
}

/// Content-free presence metadata for transcript variants.
///
/// SQLite computes these flags without returning or decoding any variant text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HistoryVariantAvailability {
    pub raw: bool,
    pub normalized: bool,
    pub cleaned: bool,
    pub selected_output: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryTextVariant {
    Raw,
    Normalized,
    Cleaned,
    SelectedOutput,
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
        self.insert_with_terminal_metadata(draft, &TerminalMetadata::default())
    }

    /// Saves a terminal dictation and its content-free extended-session facts.
    pub fn insert_with_terminal_metadata(
        &self,
        draft: &DictationDraft,
        terminal: &TerminalMetadata,
    ) -> Result<Option<i64>> {
        validate_draft(draft)?;
        validate_terminal_metadata(terminal)?;
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
                 formatting_duration_ms, insertion_duration_ms, \
                 audio_finalization_duration_ms, worker_queue_duration_ms, \
                 release_to_insert_duration_ms, warnings_json, checkpoint_count, \
                 checkpoint_repair_count, peak_retained_audio_ms, formatting_chunk_count, \
                 auto_stopped\
             ) VALUES (\
                 ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, \
                 ?16, ?17, ?18, ?19, ?20\
             )",
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
                u64_to_i64(draft.timings.audio_finalization_duration_ms)?,
                u64_to_i64(draft.timings.worker_queue_duration_ms)?,
                u64_to_i64(draft.timings.release_to_insert_duration_ms)?,
                warnings,
                u64_to_i64(terminal.checkpoint_count)?,
                u64_to_i64(terminal.checkpoint_repair_count)?,
                u64_to_i64(terminal.peak_retained_audio_ms)?,
                u64_to_i64(terminal.formatting_chunk_count)?,
                terminal.auto_stopped.map(i64::from),
            ],
        )?;
        let id = transaction.last_insert_rowid();
        transaction.commit()?;
        Ok(Some(id))
    }

    /// Returns one bounded history-list row without selecting any full text variant.
    pub fn summary(&self, id: i64) -> Result<Option<HistorySummary>> {
        self.connection
            .query_row(
                &(SUMMARY_SELECT.to_owned() + " WHERE id = ?3"),
                params![summary_preview_chars()?, warnings_json_bytes()?, id],
                map_summary,
            )
            .optional()?
            .map(decode_summary)
            .transpose()
    }

    /// Returns bounded history-list rows without loading full transcripts.
    pub fn recent_summaries(&self, limit: usize) -> Result<Vec<HistorySummary>> {
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let mut statement = self.connection.prepare(
            &(SUMMARY_SELECT.to_owned() + " ORDER BY created_at_ms DESC, id DESC LIMIT ?3"),
        )?;
        let rows = statement.query_map(
            params![summary_preview_chars()?, warnings_json_bytes()?, limit],
            map_summary,
        )?;
        rows.map(|row| decode_summary(row?)).collect()
    }

    /// Fetches exactly one transcript variant for an explicit detail/copy action.
    pub fn text_variant(&self, id: i64, variant: HistoryTextVariant) -> Result<Option<String>> {
        let (statement, field) = match variant {
            HistoryTextVariant::Raw => (
                "SELECT length(CAST(raw_text AS BLOB)), \
                        CASE WHEN length(CAST(raw_text AS BLOB)) <= ?2 THEN raw_text END \
                 FROM dictation_history WHERE id = ?1",
                "raw_text",
            ),
            HistoryTextVariant::Normalized => (
                "SELECT length(CAST(normalized_text AS BLOB)), \
                            CASE WHEN length(CAST(normalized_text AS BLOB)) <= ?2 \
                                 THEN normalized_text END \
                     FROM dictation_history WHERE id = ?1",
                "normalized_text",
            ),
            HistoryTextVariant::Cleaned => (
                "SELECT length(CAST(cleaned_text AS BLOB)), \
                            CASE WHEN length(CAST(cleaned_text AS BLOB)) <= ?2 \
                                 THEN cleaned_text END \
                     FROM dictation_history WHERE id = ?1",
                "cleaned_text",
            ),
            HistoryTextVariant::SelectedOutput => (
                "SELECT length(CAST(selected_output AS BLOB)), \
                            CASE WHEN length(CAST(selected_output AS BLOB)) <= ?2 \
                                 THEN selected_output END \
                     FROM dictation_history WHERE id = ?1",
                "selected_output",
            ),
        };
        let encoded = self
            .connection
            .query_row(statement, params![id, terminal_text_bytes()?], |row| {
                Ok((
                    row.get::<_, Option<i64>>(0)?,
                    row.get::<_, Option<String>>(1)?,
                ))
            })
            .optional()?;
        let Some((byte_count, text)) = encoded else {
            return Ok(None);
        };
        let Some(byte_count) = byte_count else {
            return Ok(None);
        };
        let actual_bytes =
            usize::try_from(byte_count).map_err(|_| PersistenceError::Validation {
                field,
                reason: "database contains an invalid byte count",
            })?;
        if actual_bytes > MAX_TERMINAL_TEXT_BYTES {
            return Err(PersistenceError::TextLimitExceeded {
                field,
                max_bytes: MAX_TERMINAL_TEXT_BYTES,
                actual_bytes,
            });
        }
        text.map_or_else(
            || {
                Err(PersistenceError::Validation {
                    field,
                    reason: "database text could not be read",
                })
            },
            |text| Ok(Some(text)),
        )
    }

    /// Convenience accessor for the usual history copy action.
    pub fn selected_output(&self, id: i64) -> Result<Option<String>> {
        self.text_variant(id, HistoryTextVariant::SelectedOutput)
    }

    /// Loads extended-session facts independently of transcript content.
    pub fn terminal_metadata(&self, id: i64) -> Result<Option<TerminalMetadata>> {
        self.connection
            .query_row(
                "SELECT checkpoint_count, checkpoint_repair_count, peak_retained_audio_ms, \
                        formatting_chunk_count, auto_stopped \
                 FROM dictation_history WHERE id = ?1",
                [id],
                map_terminal_metadata,
            )
            .optional()?
            .map(decode_terminal_metadata)
            .transpose()
    }

    pub fn get(&self, id: i64) -> Result<Option<DictationRecord>> {
        let record = self
            .connection
            .query_row(
                "SELECT id, created_at_ms, raw_text, normalized_text, cleaned_text, \
                        selected_output, language, target_executable, audio_duration_ms, \
                        stt_duration_ms, formatting_duration_ms, insertion_duration_ms, \
                        audio_finalization_duration_ms, worker_queue_duration_ms, \
                        release_to_insert_duration_ms, warnings_json \
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
                    stt_duration_ms, formatting_duration_ms, insertion_duration_ms, \
                    audio_finalization_duration_ms, worker_queue_duration_ms, \
                    release_to_insert_duration_ms, warnings_json \
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

const SUMMARY_SELECT: &str = "SELECT id, created_at_ms, substr(selected_output, 1, ?1), length(selected_output), \
            language, target_executable, audio_duration_ms, stt_duration_ms, \
            formatting_duration_ms, insertion_duration_ms, audio_finalization_duration_ms, \
            worker_queue_duration_ms, release_to_insert_duration_ms, \
            CASE WHEN length(CAST(warnings_json AS BLOB)) <= ?2 THEN warnings_json ELSE NULL END, \
            checkpoint_count, checkpoint_repair_count, peak_retained_audio_ms, \
            formatting_chunk_count, auto_stopped, raw_text IS NOT NULL, \
            normalized_text IS NOT NULL, cleaned_text IS NOT NULL, selected_output IS NOT NULL \
     FROM dictation_history";

#[derive(Debug)]
struct EncodedSummary {
    id: i64,
    created_at_ms: i64,
    preview: String,
    character_count: i64,
    language: Option<String>,
    target_executable: Option<String>,
    timings: [Option<i64>; 7],
    warnings_json: Option<String>,
    terminal: [Option<i64>; 5],
    variants: [bool; 4],
}

fn map_summary(row: &rusqlite::Row<'_>) -> rusqlite::Result<EncodedSummary> {
    Ok(EncodedSummary {
        id: row.get(0)?,
        created_at_ms: row.get(1)?,
        preview: row.get(2)?,
        character_count: row.get(3)?,
        language: row.get(4)?,
        target_executable: row.get(5)?,
        timings: [
            row.get(6)?,
            row.get(7)?,
            row.get(8)?,
            row.get(9)?,
            row.get(10)?,
            row.get(11)?,
            row.get(12)?,
        ],
        warnings_json: row.get(13)?,
        terminal: [
            row.get(14)?,
            row.get(15)?,
            row.get(16)?,
            row.get(17)?,
            row.get(18)?,
        ],
        variants: [row.get(19)?, row.get(20)?, row.get(21)?, row.get(22)?],
    })
}

fn decode_summary(encoded: EncodedSummary) -> Result<HistorySummary> {
    let selected_output_chars =
        u64::try_from(encoded.character_count).map_err(|_| PersistenceError::Validation {
            field: "selected_output",
            reason: "database contains an invalid character count",
        })?;
    let warnings = decode_summary_warnings(encoded.warnings_json)?;
    Ok(HistorySummary {
        id: encoded.id,
        created_at_ms: encoded.created_at_ms,
        selected_output_preview: encoded.preview,
        selected_output_chars,
        preview_truncated: selected_output_chars > HISTORY_PREVIEW_MAX_CHARS as u64,
        language: encoded.language,
        target_executable: encoded.target_executable,
        timings: TimingMetadata {
            audio_duration_ms: i64_to_u64(encoded.timings[0])?,
            stt_duration_ms: i64_to_u64(encoded.timings[1])?,
            formatting_duration_ms: i64_to_u64(encoded.timings[2])?,
            insertion_duration_ms: i64_to_u64(encoded.timings[3])?,
            audio_finalization_duration_ms: i64_to_u64(encoded.timings[4])?,
            worker_queue_duration_ms: i64_to_u64(encoded.timings[5])?,
            release_to_insert_duration_ms: i64_to_u64(encoded.timings[6])?,
        },
        warnings,
        terminal: decode_terminal_values(encoded.terminal)?,
        variants: HistoryVariantAvailability {
            raw: encoded.variants[0],
            normalized: encoded.variants[1],
            cleaned: encoded.variants[2],
            selected_output: encoded.variants[3],
        },
    })
}

fn decode_summary_warnings(json: Option<String>) -> Result<Vec<String>> {
    let Some(json) = json else {
        return Ok(vec![OVERSIZED_WARNINGS_NOTICE.to_owned()]);
    };
    let mut warnings: Vec<String> = serde_json::from_str(&json)?;
    let omitted = warnings.len() > MAX_TERMINAL_WARNINGS;
    warnings.truncate(MAX_TERMINAL_WARNINGS);
    for warning in &mut warnings {
        if warning.len() > MAX_TERMINAL_WARNING_BYTES {
            let mut boundary = MAX_TERMINAL_WARNING_BYTES;
            while !warning.is_char_boundary(boundary) {
                boundary -= 1;
            }
            warning.truncate(boundary);
        }
    }
    if omitted {
        if let Some(last) = warnings.last_mut() {
            *last = OVERSIZED_WARNINGS_NOTICE.to_owned();
        } else {
            warnings.push(OVERSIZED_WARNINGS_NOTICE.to_owned());
        }
    }
    Ok(warnings)
}

fn map_terminal_metadata(row: &rusqlite::Row<'_>) -> rusqlite::Result<[Option<i64>; 5]> {
    Ok([
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
    ])
}

fn decode_terminal_metadata(encoded: [Option<i64>; 5]) -> Result<TerminalMetadata> {
    decode_terminal_values(encoded)
}

fn decode_terminal_values(encoded: [Option<i64>; 5]) -> Result<TerminalMetadata> {
    Ok(TerminalMetadata {
        checkpoint_count: i64_to_u64(encoded[0])?,
        checkpoint_repair_count: i64_to_u64(encoded[1])?,
        peak_retained_audio_ms: i64_to_u64(encoded[2])?,
        formatting_chunk_count: i64_to_u64(encoded[3])?,
        auto_stopped: encoded[4].map(bool_from_i64).transpose()?,
    })
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
        row.get(13)?,
        row.get(14)?,
        row.get(15)?,
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
                audio_finalization_duration_ms: i64_to_u64(record.12)?,
                worker_queue_duration_ms: i64_to_u64(record.13)?,
                release_to_insert_duration_ms: i64_to_u64(record.14)?,
            },
            warnings: serde_json::from_str(&record.15)?,
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
    validate_text("raw_text", &draft.raw_text)?;
    if let Some(text) = &draft.normalized_text {
        validate_text("normalized_text", text)?;
    }
    if let Some(text) = &draft.cleaned_text {
        validate_text("cleaned_text", text)?;
    }
    validate_text("selected_output", &draft.selected_output)?;
    if draft.warnings.len() > MAX_TERMINAL_WARNINGS {
        return Err(PersistenceError::CollectionLimitExceeded {
            field: "warnings",
            max_items: MAX_TERMINAL_WARNINGS,
            actual_items: draft.warnings.len(),
        });
    }
    for warning in &draft.warnings {
        if warning.len() > MAX_TERMINAL_WARNING_BYTES {
            return Err(PersistenceError::TextLimitExceeded {
                field: "warning",
                max_bytes: MAX_TERMINAL_WARNING_BYTES,
                actual_bytes: warning.len(),
            });
        }
    }
    Ok(())
}

fn validate_text(field: &'static str, value: &str) -> Result<()> {
    if value.len() > MAX_TERMINAL_TEXT_BYTES {
        return Err(PersistenceError::TextLimitExceeded {
            field,
            max_bytes: MAX_TERMINAL_TEXT_BYTES,
            actual_bytes: value.len(),
        });
    }
    Ok(())
}

fn validate_terminal_metadata(metadata: &TerminalMetadata) -> Result<()> {
    for value in [
        metadata.checkpoint_count,
        metadata.checkpoint_repair_count,
        metadata.peak_retained_audio_ms,
        metadata.formatting_chunk_count,
    ] {
        let _ = u64_to_i64(value)?;
    }
    Ok(())
}

fn summary_preview_chars() -> Result<i64> {
    i64::try_from(HISTORY_PREVIEW_MAX_CHARS).map_err(|_| PersistenceError::Validation {
        field: "history_preview",
        reason: "exceeds the supported range",
    })
}

fn warnings_json_bytes() -> Result<i64> {
    i64::try_from(MAX_WARNINGS_JSON_BYTES).map_err(|_| PersistenceError::Validation {
        field: "warnings",
        reason: "display limit exceeds the supported range",
    })
}

fn terminal_text_bytes() -> Result<i64> {
    i64::try_from(MAX_TERMINAL_TEXT_BYTES).map_err(|_| PersistenceError::Validation {
        field: "history_text",
        reason: "display limit exceeds the supported range",
    })
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

fn bool_from_i64(value: i64) -> Result<bool> {
    match value {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(PersistenceError::Validation {
            field: "auto_stopped",
            reason: "database contains a non-boolean value",
        }),
    }
}
