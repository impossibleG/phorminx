//! Disposable, local meeting memory. Source rows and privacy policy remain authoritative.
use crate::{
    HistoryRepository, MeetingRecord, MeetingRepository, PersistenceError, Result, RetentionPolicy,
};
use rusqlite::{Connection, OptionalExtension, params};

pub(crate) const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS meeting_memory_config(singleton INTEGER PRIMARY KEY CHECK(singleton=1),identity TEXT NOT NULL,dimensions INTEGER);
CREATE TABLE IF NOT EXISTS meeting_memory_vectors(
 session_id INTEGER NOT NULL,sequence INTEGER NOT NULL,start_char INTEGER NOT NULL,end_char INTEGER NOT NULL,vector BLOB NOT NULL,
 PRIMARY KEY(session_id,sequence,start_char),
 FOREIGN KEY(session_id,sequence) REFERENCES meeting_segments(session_id,sequence) ON DELETE CASCADE);
CREATE TABLE IF NOT EXISTS meeting_memory_titles(
 session_id INTEGER PRIMARY KEY,sequence INTEGER NOT NULL DEFAULT 0,title TEXT NOT NULL,
 FOREIGN KEY(session_id,sequence) REFERENCES meeting_segments(session_id,sequence) ON DELETE CASCADE);
CREATE TRIGGER IF NOT EXISTS meeting_memory_forget_title BEFORE DELETE ON meeting_segments WHEN OLD.sequence=0
BEGIN UPDATE meeting_sessions SET title='Meeting' WHERE id=OLD.session_id AND title=(SELECT title FROM meeting_memory_titles WHERE session_id=OLD.session_id); END;
CREATE TRIGGER IF NOT EXISTS meeting_memory_source_change AFTER UPDATE OF text ON meeting_segments
BEGIN DELETE FROM meeting_memory_vectors WHERE session_id=NEW.session_id AND sequence=NEW.sequence; END;
"#;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MeetingMemoryPassage {
    pub session_id: i64,
    pub sequence: u64,
    pub start_char: usize,
    pub end_char: usize,
    pub text: String,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MeetingTitleSource {
    pub session_id: i64,
    pub original_title: String,
    pub text: String,
}
pub struct MeetingMemoryRepository<'a> {
    connection: &'a Connection,
}
impl<'a> MeetingMemoryRepository<'a> {
    pub(crate) fn new(connection: &'a Connection) -> Self {
        Self { connection }
    }
    fn enabled(&self) -> Result<bool> {
        Ok(HistoryRepository::new(self.connection).retention()? != RetentionPolicy::Disabled)
    }
    pub fn configure_model(&self, identity: &str) -> Result<()> {
        if identity.is_empty() || identity.len() > 1024 {
            return Err(invalid("invalid model identity"));
        }
        let tx = self.connection.unchecked_transaction()?;
        if HistoryRepository::new(&tx).retention()? == RetentionPolicy::Disabled {
            return Ok(());
        }
        let current: Option<String> = tx
            .query_row(
                "SELECT identity FROM meeting_memory_config WHERE singleton=1",
                [],
                |r| r.get(0),
            )
            .optional()?;
        if current.as_deref() != Some(identity) {
            tx.execute("DELETE FROM meeting_memory_vectors", [])?;
            tx.execute("INSERT INTO meeting_memory_config(singleton,identity,dimensions) VALUES(1,?1,NULL) ON CONFLICT(singleton) DO UPDATE SET identity=excluded.identity,dimensions=NULL",[identity])?;
        }
        tx.commit()?;
        Ok(())
    }
    /// One bounded passage per job; overlap preserves meaning at chunk edges.
    pub fn pending_passage(&self) -> Result<Option<MeetingMemoryPassage>> {
        if !self.enabled()? {
            return Ok(None);
        }
        Ok(self.connection.query_row("WITH progress AS (SELECT s.session_id,s.sequence,COALESCE((SELECT MAX(v.end_char) FROM meeting_memory_vectors v WHERE v.session_id=s.session_id AND v.sequence=s.sequence),0) AS frontier,length(s.text) AS total FROM meeting_segments s) SELECT p.session_id,p.sequence,MAX(0,p.frontier-120),MIN(p.total,MAX(0,p.frontier-120)+1000),substr(s.text,MAX(0,p.frontier-120)+1,1000) FROM progress p JOIN meeting_segments s ON s.session_id=p.session_id AND s.sequence=p.sequence WHERE p.frontier<p.total ORDER BY p.session_id DESC,p.sequence LIMIT 1",[],|r|Ok(MeetingMemoryPassage{session_id:r.get(0)?,sequence:r.get(1)?,start_char:r.get(2)?,end_char:r.get(3)?,text:r.get(4)?})).optional()?)
    }
    /// Commits only if source, policy, identity, and dimension still match.
    pub fn index_passage(
        &self,
        identity: &str,
        passage: &MeetingMemoryPassage,
        vector: &[f32],
    ) -> Result<bool> {
        let values = normalize(vector)?;
        if passage.end_char <= passage.start_char
            || passage.end_char > i64::MAX as usize
            || passage.sequence > i64::MAX as u64
            || passage.end_char - passage.start_char != passage.text.chars().count()
            || passage.text.chars().count() > 1000
        {
            return Err(invalid("invalid passage"));
        }
        let tx = self.connection.unchecked_transaction()?;
        if HistoryRepository::new(&tx).retention()? == RetentionPolicy::Disabled {
            return Ok(false);
        }
        let Some((current, dimension)) = tx
            .query_row(
                "SELECT identity,dimensions FROM meeting_memory_config WHERE singleton=1",
                [],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<usize>>(1)?)),
            )
            .optional()?
        else {
            return Ok(false);
        };
        if current != identity {
            return Ok(false);
        }
        if dimension.is_some_and(|d| d != values.len()) {
            return Err(invalid("embedding dimension mismatch"));
        }
        let same:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM meeting_segments WHERE session_id=?1 AND sequence=?2 AND substr(text,?3,?4)=?5)",params![passage.session_id,passage.sequence,passage.start_char as i64+1,(passage.end_char-passage.start_char) as i64,passage.text],|r|r.get(0))?;
        if !same {
            return Ok(false);
        }
        let bytes: Vec<u8> = values.iter().flat_map(|v| v.to_le_bytes()).collect();
        tx.execute("INSERT INTO meeting_memory_vectors(session_id,sequence,start_char,end_char,vector) VALUES(?1,?2,?3,?4,?5) ON CONFLICT(session_id,sequence,start_char) DO UPDATE SET end_char=excluded.end_char,vector=excluded.vector",params![passage.session_id,passage.sequence,passage.start_char as i64,passage.end_char as i64,bytes])?;
        tx.execute(
            "UPDATE meeting_memory_config SET dimensions=?1 WHERE singleton=1",
            [values.len() as i64],
        )?;
        tx.commit()?;
        Ok(true)
    }
    /// Uses the first bounded excerpt, once enough audio is committed or a short session ends.
    pub fn pending_title(&self) -> Result<Option<MeetingTitleSource>> {
        if !self.enabled()? {
            return Ok(None);
        }
        Ok(self.connection.query_row("SELECT m.id,m.title,substr((SELECT group_concat(excerpt,' ') FROM (SELECT substr(text,1,1000) excerpt FROM meeting_segments WHERE session_id=m.id ORDER BY sequence LIMIT 12)),1,6000) AS excerpt FROM meeting_sessions m WHERE (m.committed_sample>=480000 OR m.ended_at_ms IS NOT NULL) AND length(trim(excerpt))>0 AND NOT EXISTS(SELECT 1 FROM meeting_memory_titles t WHERE t.session_id=m.id) ORDER BY m.id DESC LIMIT 1",[],|r|Ok(MeetingTitleSource{session_id:r.get(0)?,original_title:r.get(1)?,text:r.get(2)?})).optional()?)
    }
    pub fn save_title(&self, source: &MeetingTitleSource, title: &str) -> Result<bool> {
        if title.trim().is_empty() || title.len() > 512 || title.chars().any(char::is_control) {
            return Err(invalid("invalid title"));
        }
        let tx = self.connection.unchecked_transaction()?;
        if HistoryRepository::new(&tx).retention()? == RetentionPolicy::Disabled {
            return Ok(false);
        }
        let changed=tx.execute("UPDATE meeting_sessions SET title=?2 WHERE id=?1 AND title=?3 AND substr((SELECT group_concat(excerpt,' ') FROM (SELECT substr(text,1,1000) excerpt FROM meeting_segments WHERE session_id=?1 ORDER BY sequence LIMIT 12)),1,6000)=?4 AND NOT EXISTS(SELECT 1 FROM meeting_memory_titles WHERE session_id=?1)",params![source.session_id,title,source.original_title,source.text])?;
        if changed > 0 {
            tx.execute(
                "INSERT INTO meeting_memory_titles(session_id,title) VALUES(?1,?2)",
                params![source.session_id, title],
            )?;
        }
        tx.commit()?;
        Ok(changed > 0)
    }
    /// Streaming top-k scan: memory bounded by result count, not transcript/index size.
    /// No remote requests occur here. Keyword search is always available without vectors.
    pub fn search(
        &self,
        query: &str,
        embedding: Option<(&str, &[f32])>,
        limit: usize,
        cancelled: impl Fn() -> bool,
    ) -> Result<Vec<MeetingRecord>> {
        if query.len() > 1024 {
            return Err(invalid("query is too long"));
        }
        if !self.enabled()? || cancelled() {
            return Ok(vec![]);
        }
        let limit = limit.min(100);
        if limit == 0 {
            return Ok(vec![]);
        }
        let mut lexical = MeetingRepository::new(self.connection).list(query, None, limit)?;
        let Some((identity, vector)) = embedding else {
            return Ok(lexical);
        };
        if query.trim().is_empty() {
            return Ok(lexical);
        }
        let vector = normalize(vector)?;
        let valid:bool=self.connection.query_row("SELECT EXISTS(SELECT 1 FROM meeting_memory_config WHERE singleton=1 AND identity=?1 AND dimensions=?2)",params![identity,vector.len() as i64],|r|r.get(0))?;
        if !valid {
            return Ok(lexical);
        }
        let mut best: Vec<(i64, f32)> = Vec::new();
        let mut statement = self
            .connection
            .prepare("SELECT session_id,vector FROM meeting_memory_vectors")?;
        let mut rows = statement.query([])?;
        while let Some(row) = rows.next()? {
            if cancelled() {
                return Ok(vec![]);
            }
            let id: i64 = row.get(0)?;
            let bytes: Vec<u8> = row.get(1)?;
            if bytes.len() != vector.len() * 4 {
                continue;
            }
            let score = bytes
                .as_chunks::<4>()
                .0
                .iter()
                .zip(&vector)
                .map(|(a, b)| f32::from_le_bytes(*a) * b)
                .sum::<f32>();
            if !score.is_finite() || score <= 0.0 {
                continue;
            }
            if let Some(found) = best.iter_mut().find(|(i, _)| *i == id) {
                found.1 = found.1.max(score);
            } else {
                best.push((id, score));
            }
            best.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| b.0.cmp(&a.0)));
            best.truncate(limit);
        }
        for (id, _) in best {
            if lexical.len() >= limit {
                break;
            }
            if !lexical.iter().any(|r| r.id == id)
                && let Some(record) = MeetingRepository::new(self.connection).get(id)?
            {
                lexical.push(record);
            }
        }
        if !self.enabled()? || cancelled() {
            return Ok(vec![]);
        }
        Ok(lexical)
    }
}
fn invalid(reason: &'static str) -> PersistenceError {
    PersistenceError::Validation {
        field: "meeting_memory",
        reason,
    }
}
fn normalize(vector: &[f32]) -> Result<Vec<f32>> {
    if vector.is_empty() || vector.len() > 4096 || vector.iter().any(|v| !v.is_finite()) {
        return Err(invalid("invalid embedding"));
    }
    let norm = vector
        .iter()
        .map(|v| f64::from(*v).powi(2))
        .sum::<f64>()
        .sqrt();
    if norm <= 0.0 || !norm.is_finite() {
        return Err(invalid("invalid embedding"));
    }
    Ok(vector
        .iter()
        .map(|v| (f64::from(*v) / norm) as f32)
        .collect())
}
