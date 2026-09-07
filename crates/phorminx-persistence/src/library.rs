//! Derived, disposable local search index. Original history remains authoritative.
use crate::{PersistenceError, Result};
use rusqlite::{Connection, OptionalExtension, params};

const CHARS: usize = 1000;
const OVERLAP: usize = 120;

/// Streaming case-insensitive substring matching keeps original scalar offsets
/// even when lowercase expands a character (for example İ into i + dot).
/// Memory is bounded by the query, never by the source dictation length.
pub(crate) fn unicode_match_offset(text: &str, query: &str) -> usize {
    let needle: Vec<char> = query.chars().flat_map(char::to_lowercase).collect();
    if needle.is_empty() {
        return 1;
    }
    let mut prefixes = vec![0_usize; needle.len()];
    let mut prefix = 0;
    for i in 1..needle.len() {
        while prefix > 0 && needle[i] != needle[prefix] {
            prefix = prefixes[prefix - 1];
        }
        if needle[i] == needle[prefix] {
            prefix += 1;
        }
        prefixes[i] = prefix;
    }
    let mut origins = std::collections::VecDeque::with_capacity(needle.len());
    let mut matched = 0;
    for (original, c) in text.chars().enumerate() {
        for folded in c.to_lowercase() {
            origins.push_back(original);
            if origins.len() > needle.len() {
                origins.pop_front();
            }
            while matched > 0 && folded != needle[matched] {
                matched = prefixes[matched - 1];
            }
            if folded == needle[matched] {
                matched += 1;
            }
            if matched == needle.len() {
                return origins.front().copied().unwrap_or_default() + 1;
            }
        }
    }
    0
}
pub(crate) const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS library_index_config (
 singleton INTEGER PRIMARY KEY CHECK(singleton=1), identity TEXT NOT NULL, dimensions INTEGER);
CREATE TABLE IF NOT EXISTS library_passages (
 history_id INTEGER NOT NULL REFERENCES dictation_history(id) ON DELETE CASCADE,
 start_char INTEGER NOT NULL, end_char INTEGER NOT NULL,
 vector BLOB NOT NULL, PRIMARY KEY(history_id,start_char));
CREATE TABLE IF NOT EXISTS library_progress (
 history_id INTEGER PRIMARY KEY REFERENCES dictation_history(id) ON DELETE CASCADE,
 end_char INTEGER NOT NULL, end_byte INTEGER NOT NULL, next_char INTEGER NOT NULL, next_byte INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS library_documents (
 history_id INTEGER PRIMARY KEY REFERENCES dictation_history(id) ON DELETE CASCADE, chars INTEGER NOT NULL);
INSERT OR IGNORE INTO library_documents(history_id,chars)
 SELECT id,length(selected_output) FROM dictation_history WHERE id NOT IN (SELECT history_id FROM library_documents);
CREATE TRIGGER IF NOT EXISTS library_documents_insert AFTER INSERT ON dictation_history
 BEGIN INSERT INTO library_documents(history_id,chars) VALUES(NEW.id,length(NEW.selected_output)); END;
CREATE TRIGGER IF NOT EXISTS library_documents_delete AFTER DELETE ON dictation_history
 BEGIN DELETE FROM library_documents WHERE history_id=OLD.id; END;
CREATE TRIGGER IF NOT EXISTS library_documents_update AFTER UPDATE OF selected_output ON dictation_history
 BEGIN UPDATE library_documents SET chars=length(NEW.selected_output) WHERE history_id=OLD.id; END;
CREATE TRIGGER IF NOT EXISTS library_progress_delete AFTER DELETE ON dictation_history
 BEGIN DELETE FROM library_progress WHERE history_id=OLD.id; END;
CREATE TRIGGER IF NOT EXISTS library_progress_update AFTER UPDATE OF selected_output ON dictation_history
 BEGIN DELETE FROM library_progress WHERE history_id=OLD.id; END;
CREATE TRIGGER IF NOT EXISTS library_history_delete AFTER DELETE ON dictation_history
 BEGIN DELETE FROM library_passages WHERE history_id=OLD.id; END;
CREATE TRIGGER IF NOT EXISTS library_history_update AFTER UPDATE OF selected_output ON dictation_history
 BEGIN DELETE FROM library_passages WHERE history_id=OLD.id; END;
"#;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LibraryPassage {
    pub history_id: i64,
    pub start_char: usize,
    pub end_char: usize,
    pub start_byte: usize,
    pub end_byte: usize,
    pub text: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DictationDraft, Persistence, RetentionPolicy, TimingMetadata};
    fn db() -> Persistence {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .pragma_update(None, "foreign_keys", "ON")
            .unwrap();
        crate::register_read_guards(&connection).unwrap();
        crate::migration::apply(&connection).unwrap();
        let db = Persistence { connection };
        db.history()
            .set_retention(RetentionPolicy::Indefinite, 0)
            .unwrap();
        db
    }
    fn insert(db: &Persistence, text: &str) -> i64 {
        db.history()
            .insert(&DictationDraft {
                created_at_ms: 10,
                raw_text: text.into(),
                normalized_text: None,
                cleaned_text: None,
                selected_output: text.into(),
                language: None,
                target_executable: None,
                timings: TimingMetadata::default(),
                warnings: vec![],
            })
            .unwrap()
            .unwrap()
    }
    fn batch(db: &Persistence) -> Vec<EmbeddedPassage> {
        db.library()
            .pending_passages(16)
            .unwrap()
            .into_iter()
            .map(|passage| EmbeddedPassage {
                passage,
                vector: vec![1.0, 0.0],
            })
            .collect()
    }
    #[test]
    fn migration_is_additive_and_idempotent() {
        let db = db();
        insert(&db, "original");
        crate::migration::apply(&db.connection).unwrap();
        assert_eq!(db.schema_version().unwrap(), 1);
        assert_eq!(db.history().count().unwrap(), 1);
    }
    #[test]
    fn passage_progress_covers_long_unicode_without_gaps() {
        let db = db();
        let text = "ação 日本語🙂 ".repeat(500);
        insert(&db, &text);
        db.library().configure_model("a@digest-v1").unwrap();
        let mut frontier: usize = 0;
        for _ in 0..100 {
            let items = batch(&db);
            if items.is_empty() {
                break;
            }
            assert_eq!(
                items[0].passage.start_char,
                frontier.saturating_sub(OVERLAP)
            );
            frontier = items[0].passage.end_char;
            assert_eq!(
                db.library().index_passages("a@digest-v1", &items).unwrap(),
                1
            );
        }
        assert_eq!(frontier, text.chars().count());
        let stats = db.library().stats().unwrap();
        assert_eq!(stats.pending_documents, 0);
        assert_eq!(stats.indexed_passages, stats.total_passages);
    }
    #[test]
    fn expired_and_disabled_history_remove_vectors_and_reject_inflight() {
        let db = db();
        insert(&db, "private original");
        db.library().configure_model("a").unwrap();
        let items = batch(&db);
        db.library().index_passages("a", &items).unwrap();
        db.history()
            .set_retention(RetentionPolicy::Hours24, 200_000_000)
            .unwrap();
        assert_eq!(db.library().stats().unwrap().indexed_passages, 0);
        assert_eq!(db.library().index_passages("a", &items).unwrap(), 0);
        insert(&db, "new text");
        let items = batch(&db);
        db.history()
            .set_retention(RetentionPolicy::Disabled, 0)
            .unwrap();
        assert_eq!(db.library().index_passages("a", &items).unwrap(), 0);
        assert!(db.library().pending_passages(16).unwrap().is_empty());
    }
    #[test]
    fn legacy_connection_without_foreign_keys_still_cascades() {
        let db = db();
        let id = insert(&db, "saved");
        db.library().configure_model("a").unwrap();
        db.library().index_passages("a", &batch(&db)).unwrap();
        db.connection
            .pragma_update(None, "foreign_keys", "OFF")
            .unwrap();
        db.connection
            .execute("DELETE FROM dictation_history WHERE id=?1", [id])
            .unwrap();
        assert_eq!(db.library().stats().unwrap().indexed_passages, 0);
    }
    #[test]
    fn changed_model_cannot_mix_dimensions_or_accept_old_work() {
        let db = db();
        insert(&db, "saved");
        db.library().configure_model("a").unwrap();
        let items = batch(&db);
        db.library().index_passages("a", &items).unwrap();
        assert!(!db.library().configure_model("a").unwrap());
        assert!(db.library().configure_model("b").unwrap());
        assert_eq!(db.library().index_passages("a", &items).unwrap(), 0);
        let mut new = items.clone();
        new[0].vector = vec![1., 0., 0.];
        db.library().index_passages("b", &new).unwrap();
        assert!(db.library().index_passages("b", &items).is_err());
    }
    #[test]
    fn malformed_vectors_and_noncontiguous_passages_rejected() {
        let db = db();
        insert(&db, &"x".repeat(2000));
        db.library().configure_model("a").unwrap();
        for vector in [
            vec![],
            vec![0., 0.],
            vec![f32::NAN, 1.],
            vec![f32::INFINITY],
            vec![1.; 4097],
        ] {
            let mut items = batch(&db);
            items[0].vector = vector;
            assert!(db.library().index_passages("a", &items).is_err());
        }
        let mut items = batch(&db);
        items[0].passage.start_char = 1;
        items[0].passage.end_char = 1001;
        assert_eq!(db.library().index_passages("a", &items).unwrap(), 0);
    }
    #[test]
    fn keyword_and_semantic_rank_original_ids_without_duplicate_notes() {
        let db = db();
        let data = insert(&db, "Copy the production database locally.");
        let lunch = insert(&db, "Lunch reservation on Friday.");
        db.library().configure_model("a").unwrap();
        let mut items = batch(&db);
        for entry in &mut items {
            if entry.passage.history_id == lunch {
                entry.vector = vec![0., 1.];
            }
        }
        db.library().index_passages("a", &items).unwrap();
        let hits = db
            .library()
            .search("backup", Some(("a", &[1., 0.])), 10)
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].history_id, data);
        assert!(hits[0].semantic);
        let hits = db
            .library()
            .search("Lunch", Some(("a", &[1., 0.])), 10)
            .unwrap();
        assert_eq!(hits[0].history_id, lunch);
        assert!(!hits[0].semantic);
        assert_eq!(hits.len(), 2);
    }
    #[test]
    fn keyword_supports_portuguese_and_literal_sql_metacharacters() {
        let db = db();
        let id = insert(&db, "AÇÃO pública: 100% _ ' OR 1=1; 日本語");
        for query in ["ação", "100%", "_", "' OR 1=1;", "日本語"] {
            assert_eq!(
                db.library().search(query, None, 10).unwrap()[0].history_id,
                id
            );
        }
        assert!(
            db.library()
                .search("no match", None, 10)
                .unwrap()
                .is_empty()
        );
    }
    #[test]
    fn expanding_lowercase_preserves_original_excerpt_offsets() {
        let db = db();
        let text = format!("{} target after expansion", "İ".repeat(2000));
        insert(&db, &text);
        let hit = db.library().search("TARGET", None, 10).unwrap().remove(0);
        assert!(hit.passage.contains("target after expansion"));
        assert_eq!(hit.start_char, 1881);
        assert_eq!(
            text.chars()
                .skip(hit.start_char)
                .take(hit.end_char - hit.start_char)
                .collect::<String>(),
            hit.passage
        );
        assert_eq!(unicode_match_offset("aİx", "i\u{307}x"), 2);
        assert_eq!(unicode_match_offset("ababababac", "ababac"), 5);
    }
    #[test]
    fn changed_source_invalidates_vectors_and_stale_batch() {
        let db = db();
        let id = insert(&db, "saved");
        db.library().configure_model("a").unwrap();
        let items = batch(&db);
        db.library().index_passages("a", &items).unwrap();
        db.connection
            .execute(
                "UPDATE dictation_history SET selected_output='replacement' WHERE id=?1",
                [id],
            )
            .unwrap();
        assert_eq!(db.library().stats().unwrap().indexed_passages, 0);
        assert_eq!(db.library().index_passages("a", &items).unwrap(), 0);
    }
    #[test]
    fn bounds_and_cancellation_return_no_partial_stale_results() {
        let db = db();
        insert(&db, "saved");
        assert!(
            db.library()
                .search_cancellable("saved", None, 10, || true)
                .unwrap()
                .is_empty()
        );
        assert!(db.library().search(&"x".repeat(4097), None, 10).is_err());
        assert!(db.library().pending_passages(0).unwrap().is_empty());
    }
    #[test]
    fn adaptive_short_passages_keep_advancing_through_unicode_tail() {
        let db = db();
        let text = format!(
            "{} final destination",
            "🙂ação 日本語 code_42; ".repeat(100)
        );
        let id = insert(&db, &text);
        db.library().configure_model("a").unwrap();
        let mut frontier = 0;
        let mut steps = 0;
        loop {
            let mut items = batch(&db);
            if items.is_empty() {
                break;
            }
            let p = &mut items[0].passage;
            p.text = p.text.chars().take(64).collect();
            p.end_char = p.start_char + p.text.chars().count();
            p.end_byte = p.start_byte + p.text.len();
            assert!(p.start_char <= frontier);
            assert!(p.end_char > frontier);
            frontier = p.end_char;
            assert_eq!(db.library().index_passages("a", &items).unwrap(), 1);
            steps += 1;
            assert!(steps < 100);
        }
        assert_eq!(frontier, text.chars().count());
        assert_eq!(db.library().stats().unwrap().pending_documents, 0);
        assert_eq!(
            db.library().search("final destination", None, 10).unwrap()[0].history_id,
            id
        );
    }
    #[test]
    fn tail_search_and_byte_progress_are_bounded_for_a_large_dictation() {
        let db = db();
        let text = format!("{}faraway final anchor", "prefix🙂 ".repeat(100_000));
        let id = insert(&db, &text);
        db.library().configure_model("a").unwrap();
        let first = batch(&db);
        assert_eq!(first[0].passage.text.chars().count(), 1000);
        assert!(first[0].passage.text.len() <= 4000);
        let hits = db
            .library()
            .search("faraway final anchor", None, 10)
            .unwrap();
        assert_eq!(hits[0].history_id, id);
        assert!(hits[0].passage.contains("faraway final anchor"));
        assert!(hits[0].passage.chars().count() <= 1000);
    }
}
#[derive(Clone, Debug)]
pub struct EmbeddedPassage {
    pub passage: LibraryPassage,
    pub vector: Vec<f32>,
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LibraryIndexStats {
    pub indexed_passages: u64,
    /// Estimated while pending documents remain (small-context models adapt
    /// their passage size); exact and equal to indexed_passages when complete.
    pub total_passages: u64,
    pub pending_documents: u64,
    pub indexed_documents: u64,
    pub model_identity: Option<String>,
}
#[derive(Clone, Debug, PartialEq)]
pub struct LibrarySearchHit {
    pub history_id: i64,
    pub created_at_ms: i64,
    pub start_char: usize,
    pub end_char: usize,
    pub passage: String,
    pub score: f32,
    pub semantic: bool,
}
pub struct LibraryRepository<'a> {
    connection: &'a Connection,
}
fn invalid() -> PersistenceError {
    PersistenceError::Validation {
        field: "library_index",
        reason: "invalid index identity, passage, or vector",
    }
}
fn normalize(vector: &[f32]) -> Result<Vec<f32>> {
    if vector.is_empty() || vector.len() > 4096 || vector.iter().any(|x| !x.is_finite()) {
        return Err(invalid());
    }
    let norm = vector
        .iter()
        .map(|x| f64::from(*x).powi(2))
        .sum::<f64>()
        .sqrt();
    if norm <= 0.0 || !norm.is_finite() {
        return Err(invalid());
    }
    Ok(vector
        .iter()
        .map(|x| (f64::from(*x) / norm) as f32)
        .collect())
}
impl<'a> LibraryRepository<'a> {
    pub(crate) fn new(connection: &'a Connection) -> Self {
        Self { connection }
    }
    /// Constant-time change marker for writes from other database connections.
    pub fn data_version(&self) -> Result<i64> {
        self.connection
            .pragma_query_value(None, "data_version", |r| r.get(0))
            .map_err(Into::into)
    }
    /// Identity must include the installed model digest and preprocessing version.
    pub fn configure_model(&self, identity: &str) -> Result<bool> {
        if identity.is_empty() || identity.len() > 1024 || identity.chars().any(char::is_control) {
            return Err(invalid());
        }
        let tx = self.connection.unchecked_transaction()?;
        let old: Option<String> = tx
            .query_row(
                "SELECT identity FROM library_index_config WHERE singleton=1",
                [],
                |r| r.get(0),
            )
            .optional()?;
        let changed = old.as_deref() != Some(identity);
        if changed {
            tx.execute("DELETE FROM library_passages", [])?;
            tx.execute("DELETE FROM library_progress", [])?;
            tx.execute("INSERT INTO library_index_config(singleton,identity,dimensions) VALUES(1,?1,NULL) ON CONFLICT(singleton) DO UPDATE SET identity=excluded.identity, dimensions=NULL", [identity])?;
        }
        tx.commit()?;
        Ok(changed)
    }
    pub fn clear_index(&self) -> Result<()> {
        let tx = self.connection.unchecked_transaction()?;
        tx.execute("DELETE FROM library_passages", [])?;
        tx.execute("DELETE FROM library_progress", [])?;
        tx.execute("DELETE FROM library_index_config", [])?;
        tx.commit()?;
        Ok(())
    }
    /// One next passage per document, bounded in both count and Unicode characters.
    pub fn pending_passages(&self, limit: usize) -> Result<Vec<LibraryPassage>> {
        let mut stmt=self.connection.prepare("SELECT h.id,COALESCE(p.next_char,0),COALESCE(p.next_byte,0),substr(CAST(h.selected_output AS BLOB),COALESCE(p.next_byte,0)+1,4000) FROM dictation_history h JOIN library_documents d ON d.history_id=h.id LEFT JOIN library_progress p ON p.history_id=h.id WHERE COALESCE(p.end_char,0)<d.chars AND (SELECT value FROM persistence_settings WHERE key='history_retention')!='disabled' ORDER BY h.id DESC LIMIT ?1")?;
        let rows = stmt.query_map([limit.min(16) as i64], |r| {
            let bytes: Vec<u8> = r.get(3)?;
            let valid = match std::str::from_utf8(&bytes) {
                Ok(text) => text,
                Err(error) if error.error_len().is_none() => {
                    std::str::from_utf8(&bytes[..error.valid_up_to()])
                        .expect("validated UTF-8 prefix")
                }
                Err(_) => return Err(rusqlite::Error::InvalidQuery),
            };
            let text: String = valid.chars().take(CHARS).collect();
            let start_char: usize = r.get(1)?;
            let start_byte: usize = r.get(2)?;
            Ok(LibraryPassage {
                history_id: r.get(0)?,
                start_char,
                end_char: start_char + text.chars().count(),
                start_byte,
                end_byte: start_byte + text.len(),
                text,
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }
    /// A stale batch never resurrects deleted history or contaminates a new model.
    pub fn index_passages(&self, identity: &str, passages: &[EmbeddedPassage]) -> Result<usize> {
        if passages.len() > 16 {
            return Err(invalid());
        }
        let tx = self.connection.unchecked_transaction()?;
        let config: Option<(String, Option<usize>)> = tx
            .query_row(
                "SELECT identity,dimensions FROM library_index_config WHERE singleton=1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let Some((current, mut dimension)) = config else {
            return Ok(0);
        };
        let enabled: String = tx.query_row(
            "SELECT value FROM persistence_settings WHERE key='history_retention'",
            [],
            |r| r.get(0),
        )?;
        if current != identity || enabled == "disabled" {
            return Ok(0);
        }
        let mut inserted = 0;
        for entry in passages {
            let p = &entry.passage;
            if p.start_char > 16 * 1024 * 1024
                || p.end_char <= p.start_char
                || p.end_char - p.start_char > CHARS
                || p.text.chars().count() != p.end_char - p.start_char
                || p.start_byte > 16 * 1024 * 1024
                || p.end_byte.checked_sub(p.start_byte) != Some(p.text.len())
            {
                return Err(invalid());
            }
            let normalized = normalize(&entry.vector)?;
            if dimension.is_some_and(|d| d != normalized.len()) {
                return Err(invalid());
            }
            let source:Option<Vec<u8>>=tx.query_row("SELECT substr(CAST(selected_output AS BLOB),?2+1,?3) FROM dictation_history WHERE id=?1",params![p.history_id,p.start_byte,p.text.len()],|r|r.get(0)).optional()?;
            if source.as_deref() != Some(p.text.as_bytes()) {
                continue;
            }
            let (next_char, next_byte): (usize, usize) = tx
                .query_row(
                    "SELECT next_char,next_byte FROM library_progress WHERE history_id=?1",
                    [p.history_id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?
                .unwrap_or_default();
            if p.start_char != next_char || p.start_byte != next_byte {
                continue;
            }
            let blob: Vec<u8> = normalized.iter().flat_map(|x| x.to_le_bytes()).collect();
            inserted+=tx.execute("INSERT OR IGNORE INTO library_passages(history_id,start_char,end_char,vector) VALUES(?1,?2,?3,?4)",params![p.history_id,p.start_char,p.end_char,blob])?;
            let overlap = OVERLAP.min((p.end_char - p.start_char) / 4);
            let overlap_bytes: usize = p.text.chars().rev().take(overlap).map(char::len_utf8).sum();
            tx.execute("INSERT INTO library_progress(history_id,end_char,end_byte,next_char,next_byte) VALUES(?1,?2,?3,?4,?5) ON CONFLICT(history_id) DO UPDATE SET end_char=excluded.end_char,end_byte=excluded.end_byte,next_char=excluded.next_char,next_byte=excluded.next_byte",params![p.history_id,p.end_char,p.end_byte,p.end_char.saturating_sub(overlap),p.end_byte-overlap_bytes])?;
            dimension = Some(normalized.len());
        }
        tx.execute(
            "UPDATE library_index_config SET dimensions=?1 WHERE singleton=1",
            [dimension],
        )?;
        tx.commit()?;
        Ok(inserted)
    }
    pub fn stats(&self) -> Result<LibraryIndexStats> {
        let indexed_passages =
            self.connection
                .query_row("SELECT count(*) FROM library_passages", [], |r| r.get(0))?;
        let (estimated_passages,pending_documents,indexed_documents):(u64,u64,u64)=self.connection.query_row("SELECT COALESCE(SUM(CASE WHEN d.chars=0 THEN 0 WHEN d.chars<=1000 THEN 1 ELSE 1+(d.chars-1000+879)/880 END),0),COALESCE(SUM(CASE WHEN COALESCE(p.end_char,0)<d.chars THEN 1 ELSE 0 END),0),COALESCE(SUM(CASE WHEN p.end_char>=d.chars THEN 1 ELSE 0 END),0) FROM library_documents d LEFT JOIN library_progress p ON p.history_id=d.history_id",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
        let total_passages = if pending_documents == 0 {
            indexed_passages
        } else {
            estimated_passages.max(indexed_passages)
        };
        let model_identity = self
            .connection
            .query_row(
                "SELECT identity FROM library_index_config WHERE singleton=1",
                [],
                |r| r.get(0),
            )
            .optional()?;
        Ok(LibraryIndexStats {
            indexed_passages,
            total_passages,
            pending_documents,
            indexed_documents,
            model_identity,
        })
    }
    pub fn search(
        &self,
        query: &str,
        embedding: Option<(&str, &[f32])>,
        limit: usize,
    ) -> Result<Vec<LibrarySearchHit>> {
        self.search_cancellable(query, embedding, limit, || false)
    }
    /// Streaming exact vector scan uses bounded memory. Caller cancels on capture.
    pub fn search_cancellable(
        &self,
        query: &str,
        embedding: Option<(&str, &[f32])>,
        limit: usize,
        cancelled: impl Fn() -> bool,
    ) -> Result<Vec<LibrarySearchHit>> {
        let query = query.trim();
        if query.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        if query.len() > 4096 {
            return Err(invalid());
        }
        let limit = limit.min(100);
        let mut hits = Vec::new();
        // SQLite bounds excerpts in the query; a long original is not loaded into the UI.
        let mut stmt=self.connection.prepare("SELECT id,created_at_ms,MAX(0,phorminx_find(selected_output,?1)-121),substr(selected_output,MAX(1,phorminx_find(selected_output,?1)-120),1000) FROM dictation_history WHERE phorminx_find(selected_output,?1)>0 ORDER BY created_at_ms DESC,id DESC LIMIT ?2")?;
        let rows = stmt.query_map(params![query, limit], |r| {
            let passage: String = r.get(3)?;
            let start_char: usize = r.get(2)?;
            Ok(LibrarySearchHit {
                history_id: r.get(0)?,
                created_at_ms: r.get(1)?,
                start_char,
                end_char: start_char + passage.chars().count(),
                passage,
                score: 2.0,
                semantic: false,
            })
        })?;
        for hit in rows {
            if cancelled() {
                return Ok(Vec::new());
            }
            hits.push(hit?);
        }
        if let Some((identity, vector)) = embedding {
            let query_vector = normalize(vector)?;
            let config: Option<(String, Option<usize>)> = self
                .connection
                .query_row(
                    "SELECT identity,dimensions FROM library_index_config WHERE singleton=1",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            if config
                .as_ref()
                .is_some_and(|(id, d)| id == identity && *d == Some(query_vector.len()))
            {
                let mut stmt=self.connection.prepare("SELECT p.history_id,h.created_at_ms,p.start_char,p.end_char,CASE WHEN length(p.vector)<=16384 THEN p.vector ELSE NULL END FROM library_passages p JOIN dictation_history h ON h.id=p.history_id ORDER BY h.created_at_ms DESC,p.start_char ASC")?;
                let mut rows = stmt.query([])?;
                while let Some(row) = rows.next()? {
                    if cancelled() {
                        return Ok(Vec::new());
                    }
                    let Some(blob): Option<Vec<u8>> = row.get(4)? else {
                        continue;
                    };
                    if blob.len() != query_vector.len() * 4 {
                        continue;
                    }
                    let score: f32 = blob
                        .as_chunks::<4>()
                        .0
                        .iter()
                        .zip(&query_vector)
                        .map(|(bytes, q)| f32::from_le_bytes(*bytes) * q)
                        .sum();
                    if !score.is_finite() || score < 0.2 {
                        continue;
                    }
                    let history_id: i64 = row.get(0)?;
                    if hits
                        .iter()
                        .any(|h| h.history_id == history_id && h.score >= score)
                    {
                        continue;
                    }
                    hits.retain(|h| h.history_id != history_id);
                    hits.push(LibrarySearchHit {
                        history_id,
                        created_at_ms: row.get(1)?,
                        start_char: row.get(2)?,
                        end_char: row.get(3)?,
                        passage: String::new(),
                        score,
                        semantic: true,
                    });
                    hits.sort_by(|a, b| {
                        b.score
                            .total_cmp(&a.score)
                            .then(b.created_at_ms.cmp(&a.created_at_ms))
                    });
                    hits.truncate(limit);
                }
                for hit in hits.iter_mut().filter(|h| h.semantic) {
                    hit.passage=self.connection.query_row("SELECT substr(selected_output,?2+1,?3) FROM dictation_history WHERE id=?1",params![hit.history_id,hit.start_char,hit.end_char-hit.start_char],|r|r.get(0)).optional()?.unwrap_or_default();
                }
            }
        }
        hits.retain(|h| !h.passage.is_empty());
        hits.sort_by(|a, b| {
            b.score
                .total_cmp(&a.score)
                .then(b.created_at_ms.cmp(&a.created_at_ms))
        });
        hits.truncate(limit);
        Ok(hits)
    }
}
