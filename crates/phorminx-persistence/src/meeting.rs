//! Additive, text-only meeting storage. Recording never depends on persistence.
use rusqlite::{Connection, OptionalExtension, params};

use crate::{
    HistoryRepository, MAX_TERMINAL_TEXT_BYTES, PersistenceError, Result, RetentionPolicy,
};

const MAX_SEGMENT_BYTES: usize = 256 * 1024;
const MESSAGE_PREVIEW_CHARS: usize = 16 * 1024;
const MAX_CONTEXT_BYTES: usize = 96 * 1024;

pub(crate) const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS meeting_sessions (
 id INTEGER PRIMARY KEY AUTOINCREMENT,
 title TEXT NOT NULL,
 source TEXT NOT NULL CHECK(source IN ('mic','system','chat')),
 created_at_ms INTEGER NOT NULL,
 updated_at_ms INTEGER NOT NULL,
 ended_at_ms INTEGER,
 next_sequence INTEGER NOT NULL DEFAULT 0,
 committed_sample INTEGER NOT NULL DEFAULT 0,
 assistant_generation INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS meeting_sessions_updated_idx ON meeting_sessions(updated_at_ms, id);
CREATE TABLE IF NOT EXISTS meeting_segments (
 session_id INTEGER NOT NULL REFERENCES meeting_sessions(id) ON DELETE CASCADE,
 sequence INTEGER NOT NULL,
 start_sample INTEGER NOT NULL,
 end_sample INTEGER NOT NULL,
 text TEXT NOT NULL,
 PRIMARY KEY(session_id, sequence),
 CHECK(start_sample >= 0 AND end_sample > start_sample)
);
CREATE TABLE IF NOT EXISTS meeting_messages (
 id INTEGER PRIMARY KEY AUTOINCREMENT,
 session_id INTEGER NOT NULL REFERENCES meeting_sessions(id) ON DELETE CASCADE,
 role TEXT NOT NULL CHECK(role IN ('user','assistant')),
 status TEXT NOT NULL CHECK(status IN ('streaming','complete','cancelled','failed')),
 cutoff_sample INTEGER,
 generation INTEGER,
 text TEXT NOT NULL,
 created_at_ms INTEGER NOT NULL,
 updated_at_ms INTEGER NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS meeting_user_cutoff_idx ON meeting_messages(session_id,cutoff_sample) WHERE role='user';
CREATE UNIQUE INDEX IF NOT EXISTS meeting_assistant_generation_idx ON meeting_messages(session_id,generation) WHERE role='assistant';
CREATE INDEX IF NOT EXISTS meeting_messages_session_idx ON meeting_messages(session_id,id);
CREATE TABLE IF NOT EXISTS meeting_question_context (
 message_id INTEGER PRIMARY KEY REFERENCES meeting_messages(id) ON DELETE CASCADE,
 text TEXT NOT NULL, start_sample INTEGER NOT NULL, end_sample INTEGER NOT NULL,
 CHECK(start_sample >= 0 AND end_sample > start_sample)
);
"#;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MeetingQuestionContext {
    pub text: String,
    pub start_sample: u64,
    pub end_sample: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MeetingRecord {
    pub id: i64,
    pub title: String,
    pub source: String,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub ended_at_ms: Option<i64>,
    pub next_sequence: u64,
    pub committed_sample: u64,
    pub assistant_generation: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MeetingSegmentRecord {
    pub sequence: u64,
    pub start_sample: u64,
    pub end_sample: u64,
    pub text: String,
    pub text_truncated: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MeetingMessageStatus {
    Streaming,
    Complete,
    Cancelled,
    Failed,
}

impl MeetingMessageStatus {
    fn as_db(self) -> &'static str {
        match self {
            Self::Streaming => "streaming",
            Self::Complete => "complete",
            Self::Cancelled => "cancelled",
            Self::Failed => "failed",
        }
    }
    fn from_db(s: &str) -> rusqlite::Result<Self> {
        match s {
            "streaming" => Ok(Self::Streaming),
            "complete" => Ok(Self::Complete),
            "cancelled" => Ok(Self::Cancelled),
            "failed" => Ok(Self::Failed),
            _ => Err(rusqlite::Error::InvalidQuery),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MeetingMessageRecord {
    pub id: i64,
    pub role: String,
    pub status: MeetingMessageStatus,
    pub cutoff_sample: Option<u64>,
    pub generation: Option<u64>,
    /// A bounded preview. Use `message_text` to read longer messages in chunks.
    pub text: String,
    pub text_truncated: bool,
    pub created_at_ms: i64,
}

pub struct MeetingRepository<'a> {
    connection: &'a Connection,
}

impl<'a> MeetingRepository<'a> {
    pub(crate) fn new(connection: &'a Connection) -> Self {
        Self { connection }
    }

    /// Saves the visible question separately from its private transcript attachment.
    /// Both rows commit together; exact cutoff replays are safe to retry.
    pub fn append_user_question(
        &self,
        session_id: i64,
        cutoff: Option<u64>,
        question: &str,
        context: &str,
        start: Option<u64>,
        now_ms: i64,
    ) -> Result<Option<i64>> {
        bounded("meeting_message", question, MAX_TERMINAL_TEXT_BYTES)?;
        bounded("meeting_context", context, MAX_CONTEXT_BYTES)?;
        match (start, cutoff) {
            (Some(start), Some(end)) if start < end => {}
            (None, None) if context.is_empty() => {
                return self.append_chat_user(session_id, question, now_ms);
            }
            _ => {
                return Err(invalid(
                    "meeting_context",
                    "context requires an ordered sample range",
                ));
            }
        }
        let start = number(start.unwrap())?;
        let cutoff = number(cutoff.unwrap())?;
        let tx = self.connection.unchecked_transaction()?;
        if HistoryRepository::new(&tx).retention()? == RetentionPolicy::Disabled {
            return Ok(None);
        }
        let Some(committed) = tx
            .query_row(
                "SELECT committed_sample FROM meeting_sessions WHERE id=?1",
                [session_id],
                |r| r.get::<_, i64>(0),
            )
            .optional()?
        else {
            return Ok(None);
        };
        if let Some((id, same)) = tx.query_row("SELECT m.id,m.text=?3 AND c.text=?4 AND c.start_sample=?5 AND c.end_sample=?2 FROM meeting_messages m JOIN meeting_question_context c ON c.message_id=m.id WHERE m.session_id=?1 AND m.role='user' AND m.cutoff_sample=?2", params![session_id,cutoff,question,context,start], |r| Ok((r.get::<_,i64>(0)?,r.get::<_,bool>(1)?))).optional()? {
            return if same { Ok(Some(id)) } else { Err(invalid("meeting_context", "conflicting cutoff replay")) };
        }
        let previous: i64 = tx.query_row("SELECT COALESCE(MAX(cutoff_sample),0) FROM meeting_messages WHERE session_id=?1 AND role='user'", [session_id], |r| r.get(0))?;
        let crossing: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM meeting_segments WHERE session_id=?1 AND ((start_sample<?2 AND end_sample>?2) OR (start_sample<?3 AND end_sample>?3)))", params![session_id,start,cutoff], |r| r.get(0))?;
        if cutoff <= previous || start < previous || cutoff > committed || crossing {
            return Err(invalid(
                "meeting_cutoff",
                "cutoff must advance to a committed segment boundary",
            ));
        }
        tx.execute("INSERT INTO meeting_messages(session_id,role,status,cutoff_sample,text,created_at_ms,updated_at_ms) VALUES(?1,'user','complete',?2,?3,?4,?4)", params![session_id,cutoff,question,now_ms])?;
        let id = tx.last_insert_rowid();
        tx.execute("INSERT INTO meeting_question_context(message_id,text,start_sample,end_sample) VALUES(?1,?2,?3,?4)", params![id,context,start,cutoff])?;
        tx.execute(
            "UPDATE meeting_sessions SET updated_at_ms=MAX(updated_at_ms,?2) WHERE id=?1",
            params![session_id, now_ms],
        )?;
        tx.commit()?;
        Ok(Some(id))
    }

    pub fn question_context(
        &self,
        session_id: i64,
        message_id: i64,
    ) -> Result<Option<MeetingQuestionContext>> {
        if HistoryRepository::new(self.connection).retention()? == RetentionPolicy::Disabled {
            return Ok(None);
        }
        let context=self.connection.query_row("SELECT substr(c.text,1,98305),c.start_sample,c.end_sample FROM meeting_question_context c JOIN meeting_messages m ON m.id=c.message_id WHERE m.session_id=?1 AND m.id=?2", params![session_id,message_id], |r| Ok(MeetingQuestionContext{text:r.get(0)?,start_sample:r.get(1)?,end_sample:r.get(2)?})).optional()?;
        if let Some(context) = &context {
            bounded("meeting_context", &context.text, MAX_CONTEXT_BYTES)?;
        }
        Ok(context)
    }

    /// `None` means text retention is disabled. The caller may continue ephemerally.
    pub fn create(&self, title: &str, source: &str, now_ms: i64) -> Result<Option<i64>> {
        bounded("meeting_title", title, 1024)?;
        if !matches!(source, "mic" | "system" | "chat") {
            return Err(invalid("meeting_source", "unsupported audio source"));
        }
        let tx = self.connection.unchecked_transaction()?;
        if HistoryRepository::new(&tx).retention()? == RetentionPolicy::Disabled {
            return Ok(None);
        }
        tx.execute("INSERT INTO meeting_sessions(title,source,created_at_ms,updated_at_ms) VALUES(?1,?2,?3,?3)", params![title,source,now_ms])?;
        let id = tx.last_insert_rowid();
        tx.commit()?;
        Ok(Some(id))
    }

    pub fn get(&self, session_id: i64) -> Result<Option<MeetingRecord>> {
        Ok(self.connection.query_row("SELECT id,substr(title,1,1024),source,created_at_ms,updated_at_ms,ended_at_ms,next_sequence,committed_sample,assistant_generation FROM meeting_sessions WHERE id=?1", [session_id], meeting_row).optional()?)
    }

    /// Stable descending-id pagination. Keyword search includes title, transcript,
    /// and chat. Bound parameters are literal substrings, never SQL wildcards.
    pub fn list(
        &self,
        query: &str,
        before_id: Option<i64>,
        limit: usize,
    ) -> Result<Vec<MeetingRecord>> {
        bounded("meeting_query", query, 1024)?;
        let mut statement = self.connection.prepare("SELECT id,substr(title,1,1024),source,created_at_ms,updated_at_ms,ended_at_ms,next_sequence,committed_sample,assistant_generation FROM meeting_sessions m WHERE (?1 IS NULL OR m.id<?1) AND (?2='' OR phorminx_find(m.title,?2)>0 OR EXISTS(SELECT 1 FROM meeting_segments s WHERE s.session_id=m.id AND phorminx_find(s.text,?2)>0) OR EXISTS(SELECT 1 FROM meeting_messages c WHERE c.session_id=m.id AND phorminx_find(c.text,?2)>0)) ORDER BY m.id DESC LIMIT ?3")?;
        Ok(statement
            .query_map(
                params![before_id, query, limit.min(100) as i64],
                meeting_row,
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Exact replays succeed without a second row. Missing/deleted/disabled sessions
    /// return false; stale workers can never recreate data after a privacy deletion.
    pub fn append_segment(
        &self,
        session_id: i64,
        sequence: u64,
        start_sample: u64,
        end_sample: u64,
        text: &str,
        now_ms: i64,
    ) -> Result<bool> {
        bounded("meeting_segment", text, MAX_SEGMENT_BYTES)?;
        let sequence = number(sequence)?;
        let start = number(start_sample)?;
        let end = number(end_sample)?;
        if end <= start {
            return Err(invalid("meeting_segment", "sample range must be nonempty"));
        }
        let tx = self.connection.unchecked_transaction()?;
        if HistoryRepository::new(&tx).retention()? == RetentionPolicy::Disabled {
            return Ok(false);
        }
        let Some((next, committed, ended)) = tx.query_row("SELECT next_sequence,committed_sample,ended_at_ms FROM meeting_sessions WHERE id=?1", [session_id], |r| Ok((r.get::<_,i64>(0)?,r.get::<_,i64>(1)?,r.get::<_,Option<i64>>(2)?))).optional()? else { return Ok(false); };
        if sequence < next {
            let same: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM meeting_segments WHERE session_id=?1 AND sequence=?2 AND start_sample=?3 AND end_sample=?4 AND text=?5)",params![session_id,sequence,start,end,text],|r|r.get(0))?;
            if same {
                return Ok(true);
            }
            return Err(invalid("meeting_segment", "conflicting replay"));
        }
        if ended.is_some() {
            return Ok(false);
        }
        if sequence != next || start != committed {
            return Err(invalid(
                "meeting_segment",
                "segments must be ordered and contiguous",
            ));
        }
        let next = next
            .checked_add(1)
            .ok_or_else(|| invalid("meeting_sequence", "out of range"))?;
        tx.execute("INSERT INTO meeting_segments(session_id,sequence,start_sample,end_sample,text) VALUES(?1,?2,?3,?4,?5)",params![session_id,sequence,start,end,text])?;
        tx.execute("UPDATE meeting_sessions SET next_sequence=?2,committed_sample=?3,updated_at_ms=MAX(updated_at_ms,?4) WHERE id=?1",params![session_id,next,end,now_ms])?;
        tx.commit()?;
        Ok(true)
    }

    pub fn segments(
        &self,
        session_id: i64,
        after_sequence: Option<u64>,
        limit: usize,
    ) -> Result<Vec<MeetingSegmentRecord>> {
        let after = after_sequence.map(number).transpose()?;
        let mut statement = self.connection.prepare("SELECT sequence,start_sample,end_sample,substr(text,1,262144),length(text)>262144 FROM meeting_segments WHERE session_id=?1 AND (?2 IS NULL OR sequence>?2) ORDER BY sequence LIMIT ?3")?;
        Ok(statement
            .query_map(params![session_id, after, limit.min(100) as i64], |r| {
                Ok(MeetingSegmentRecord {
                    sequence: r.get(0)?,
                    start_sample: r.get(1)?,
                    end_sample: r.get(2)?,
                    text: r.get(3)?,
                    text_truncated: r.get(4)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Latest segment previews in chronological order, without scanning earlier
    /// rows. Previews are explicit; `segments` supplies complete bounded segments.
    pub fn latest_segments(
        &self,
        session_id: i64,
        limit: usize,
    ) -> Result<Vec<MeetingSegmentRecord>> {
        let mut statement = self.connection.prepare("SELECT sequence,start_sample,end_sample,substr(text,1,16384),length(text)>16384 FROM meeting_segments WHERE session_id=?1 ORDER BY sequence DESC LIMIT ?2")?;
        let mut records = statement
            .query_map(params![session_id, limit.min(200) as i64], |r| {
                Ok(MeetingSegmentRecord {
                    sequence: r.get(0)?,
                    start_sample: r.get(1)?,
                    end_sample: r.get(2)?,
                    text: r.get(3)?,
                    text_truncated: r.get(4)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        records.reverse();
        Ok(records)
    }

    pub fn append_user_message(
        &self,
        session_id: i64,
        cutoff_sample: u64,
        text: &str,
        now_ms: i64,
    ) -> Result<Option<i64>> {
        bounded("meeting_message", text, MAX_TERMINAL_TEXT_BYTES)?;
        let cutoff = number(cutoff_sample)?;
        let tx = self.connection.unchecked_transaction()?;
        if HistoryRepository::new(&tx).retention()? == RetentionPolicy::Disabled {
            return Ok(None);
        }
        let Some(committed) = tx
            .query_row(
                "SELECT committed_sample FROM meeting_sessions WHERE id=?1",
                [session_id],
                |r| r.get::<_, i64>(0),
            )
            .optional()?
        else {
            return Ok(None);
        };
        if let Some((id, same)) = tx.query_row("SELECT id,text=?3 FROM meeting_messages WHERE session_id=?1 AND role='user' AND cutoff_sample=?2",params![session_id,cutoff,text],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,bool>(1)?))).optional()? {
            return if same { Ok(Some(id)) } else { Err(invalid("meeting_message", "conflicting cutoff replay")) };
        }
        let previous: i64 = tx.query_row("SELECT COALESCE(MAX(cutoff_sample),-1) FROM meeting_messages WHERE session_id=?1 AND role='user'",[session_id],|r|r.get(0))?;
        let crossing: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM meeting_segments WHERE session_id=?1 AND start_sample<?2 AND end_sample>?2)",params![session_id,cutoff],|r|r.get(0))?;
        if cutoff <= previous || cutoff > committed || crossing {
            return Err(invalid(
                "meeting_cutoff",
                "cutoff must advance to a committed segment boundary",
            ));
        }
        tx.execute("INSERT INTO meeting_messages(session_id,role,status,cutoff_sample,text,created_at_ms,updated_at_ms) VALUES(?1,'user','complete',?2,?3,?4,?4)",params![session_id,cutoff,text,now_ms])?;
        let id = tx.last_insert_rowid();
        tx.execute(
            "UPDATE meeting_sessions SET updated_at_ms=MAX(updated_at_ms,?2) WHERE id=?1",
            params![session_id, now_ms],
        )?;
        tx.commit()?;
        Ok(Some(id))
    }

    /// A typed chat message has no audio boundary. It can be sent after capture
    /// ends; a missing or privacy-disabled session is never recreated.
    pub fn append_chat_user(
        &self,
        session_id: i64,
        text: &str,
        now_ms: i64,
    ) -> Result<Option<i64>> {
        bounded("meeting_message", text, MAX_TERMINAL_TEXT_BYTES)?;
        let tx = self.connection.unchecked_transaction()?;
        if HistoryRepository::new(&tx).retention()? == RetentionPolicy::Disabled {
            return Ok(None);
        }
        let changed = tx.execute("INSERT INTO meeting_messages(session_id,role,status,text,created_at_ms,updated_at_ms) SELECT id,'user','complete',?2,?3,?3 FROM meeting_sessions WHERE id=?1",params![session_id,text,now_ms])?;
        if changed == 0 {
            return Ok(None);
        }
        let id = tx.last_insert_rowid();
        tx.execute(
            "UPDATE meeting_sessions SET updated_at_ms=MAX(updated_at_ms,?2) WHERE id=?1",
            params![session_id, now_ms],
        )?;
        tx.commit()?;
        Ok(Some(id))
    }

    /// A new generation atomically marks the previous unfinished response stopped.
    pub fn start_assistant(
        &self,
        session_id: i64,
        generation: u64,
        now_ms: i64,
    ) -> Result<Option<i64>> {
        let generation = number(generation)?;
        let tx = self.connection.unchecked_transaction()?;
        if HistoryRepository::new(&tx).retention()? == RetentionPolicy::Disabled {
            return Ok(None);
        }
        let Some(previous) = tx
            .query_row(
                "SELECT assistant_generation FROM meeting_sessions WHERE id=?1",
                [session_id],
                |r| r.get::<_, i64>(0),
            )
            .optional()?
        else {
            return Ok(None);
        };
        if generation <= previous {
            return Ok(None);
        }
        tx.execute("UPDATE meeting_messages SET status='cancelled',updated_at_ms=MAX(updated_at_ms,?2) WHERE session_id=?1 AND role='assistant' AND status='streaming'",params![session_id,now_ms])?;
        tx.execute("INSERT INTO meeting_messages(session_id,role,status,generation,text,created_at_ms,updated_at_ms) VALUES(?1,'assistant','streaming',?2,'',?3,?3)",params![session_id,generation,now_ms])?;
        let id = tx.last_insert_rowid();
        tx.execute("UPDATE meeting_sessions SET assistant_generation=?2,updated_at_ms=MAX(updated_at_ms,?3) WHERE id=?1",params![session_id,generation,now_ms])?;
        tx.commit()?;
        Ok(Some(id))
    }

    /// Complete replacement of the current response snapshot, not token append.
    /// Terminal and superseded generations reject late or duplicated output.
    pub fn update_assistant(
        &self,
        session_id: i64,
        generation: u64,
        text: &str,
        status: MeetingMessageStatus,
        now_ms: i64,
    ) -> Result<bool> {
        bounded("meeting_message", text, MAX_TERMINAL_TEXT_BYTES)?;
        let generation = number(generation)?;
        let tx = self.connection.unchecked_transaction()?;
        if HistoryRepository::new(&tx).retention()? == RetentionPolicy::Disabled {
            return Ok(false);
        }
        let changed = tx.execute("UPDATE meeting_messages SET text=?3,status=?4,updated_at_ms=MAX(updated_at_ms,?5) WHERE session_id=?1 AND generation=?2 AND role='assistant' AND status='streaming' AND EXISTS(SELECT 1 FROM meeting_sessions WHERE id=?1 AND assistant_generation=?2)",params![session_id,generation,text,status.as_db(),now_ms])?;
        if changed > 0 {
            tx.execute(
                "UPDATE meeting_sessions SET updated_at_ms=MAX(updated_at_ms,?2) WHERE id=?1",
                params![session_id, now_ms],
            )?;
        }
        tx.commit()?;
        Ok(changed > 0)
    }

    pub fn messages(
        &self,
        session_id: i64,
        after_id: Option<i64>,
        limit: usize,
    ) -> Result<Vec<MeetingMessageRecord>> {
        let mut statement = self.connection.prepare("SELECT id,role,status,cutoff_sample,generation,substr(text,1,?4),length(text)>?4,created_at_ms FROM meeting_messages WHERE session_id=?1 AND (?2 IS NULL OR id>?2) ORDER BY id LIMIT ?3")?;
        Ok(statement
            .query_map(
                params![
                    session_id,
                    after_id,
                    limit.min(100) as i64,
                    MESSAGE_PREVIEW_CHARS as i64
                ],
                |r| {
                    Ok(MeetingMessageRecord {
                        id: r.get(0)?,
                        role: r.get(1)?,
                        status: MeetingMessageStatus::from_db(&r.get::<_, String>(2)?)?,
                        cutoff_sample: r.get(3)?,
                        generation: r.get(4)?,
                        text: r.get(5)?,
                        text_truncated: r.get(6)?,
                        created_at_ms: r.get(7)?,
                    })
                },
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Unicode-character offsets, zero based, at most 16k characters per read.
    pub fn message_text(
        &self,
        session_id: i64,
        message_id: i64,
        offset_chars: usize,
        limit_chars: usize,
    ) -> Result<Option<String>> {
        let offset = i64::try_from(offset_chars)
            .ok()
            .and_then(|v| v.checked_add(1))
            .ok_or_else(|| invalid("meeting_offset", "out of range"))?;
        Ok(self
            .connection
            .query_row(
                "SELECT substr(text,?3,?4) FROM meeting_messages WHERE session_id=?1 AND id=?2",
                params![
                    session_id,
                    message_id,
                    offset,
                    limit_chars.min(MESSAGE_PREVIEW_CHARS) as i64
                ],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// Latest message previews, returned chronologically. Reads at most 80 rows;
    /// long messages retain `text_truncated` and can be read using `message_text`.
    pub fn latest_messages(
        &self,
        session_id: i64,
        limit: usize,
    ) -> Result<Vec<MeetingMessageRecord>> {
        let mut statement = self.connection.prepare("SELECT id,role,status,cutoff_sample,generation,substr(text,1,?3),length(text)>?3,created_at_ms FROM meeting_messages WHERE session_id=?1 ORDER BY id DESC LIMIT ?2")?;
        let mut records = statement
            .query_map(
                params![
                    session_id,
                    limit.min(80) as i64,
                    MESSAGE_PREVIEW_CHARS as i64
                ],
                |r| {
                    Ok(MeetingMessageRecord {
                        id: r.get(0)?,
                        role: r.get(1)?,
                        status: MeetingMessageStatus::from_db(&r.get::<_, String>(2)?)?,
                        cutoff_sample: r.get(3)?,
                        generation: r.get(4)?,
                        text: r.get(5)?,
                        text_truncated: r.get(6)?,
                        created_at_ms: r.get(7)?,
                    })
                },
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        records.reverse();
        Ok(records)
    }

    /// Ends capture only in saved metadata. It does not cancel an AI response:
    /// that response may finish after the microphone has been stopped explicitly.
    pub fn finalize(&self, session_id: i64, now_ms: i64) -> Result<bool> {
        Ok(self.connection.execute("UPDATE meeting_sessions SET ended_at_ms=MAX(created_at_ms,?2),updated_at_ms=MAX(updated_at_ms,?2) WHERE id=?1 AND ended_at_ms IS NULL",params![session_id,now_ms])? > 0)
    }

    pub fn delete(&self, session_id: i64) -> Result<bool> {
        Ok(self
            .connection
            .execute("DELETE FROM meeting_sessions WHERE id=?1", [session_id])?
            > 0)
    }

    pub fn clear(&self) -> Result<usize> {
        Ok(self
            .connection
            .execute("DELETE FROM meeting_sessions", [])?)
    }
}

fn meeting_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<MeetingRecord> {
    Ok(MeetingRecord {
        id: r.get(0)?,
        title: r.get(1)?,
        source: r.get(2)?,
        created_at_ms: r.get(3)?,
        updated_at_ms: r.get(4)?,
        ended_at_ms: r.get(5)?,
        next_sequence: r.get(6)?,
        committed_sample: r.get(7)?,
        assistant_generation: r.get(8)?,
    })
}
fn invalid(field: &'static str, reason: &'static str) -> PersistenceError {
    PersistenceError::Validation { field, reason }
}
fn number(n: u64) -> Result<i64> {
    i64::try_from(n).map_err(|_| invalid("meeting_sample", "out of range"))
}
fn bounded(field: &'static str, text: &str, max_bytes: usize) -> Result<()> {
    if text.len() > max_bytes {
        return Err(PersistenceError::TextLimitExceeded {
            field,
            max_bytes,
            actual_bytes: text.len(),
        });
    }
    Ok(())
}
