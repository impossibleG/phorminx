//! Additive storage for investigations: long-lived collections of captured
//! text, screenshots, links, voice notes and pinned assistant answers.
//!
//! Investigations are explicit user documents. Dictation history retention
//! does not apply: rows are kept until the user deletes the item or the whole
//! investigation. Image bytes never enter the database; items reference a
//! SHA-256 file name that the host stores in its data folder.
use rusqlite::{Connection, OptionalExtension, params};

use crate::{PersistenceError, Result};

pub const MAX_INVESTIGATION_TITLE_CHARS: usize = 200;
pub const MAX_ITEM_TITLE_CHARS: usize = 300;
pub const MAX_ITEM_BODY_BYTES: usize = 256 * 1024;
pub const MAX_ITEM_URL_BYTES: usize = 4096;
pub const MAX_ITEMS_PER_INVESTIGATION: usize = 5_000;
pub const MAX_NODES_PER_INVESTIGATION: usize = 6_000;
pub const MAX_EDGES_PER_INVESTIGATION: usize = 12_000;
pub const MAX_MESSAGE_BYTES: usize = 512 * 1024;
const MAX_LAYOUT_BYTES: usize = 4096;
const MAX_OCR_LINES: usize = 2_000;
const ACTIVE_KEY: &str = "active_investigation";
const CAPTURE_SHORTCUT_KEY: &str = "investigation_capture_shortcut";

pub(crate) const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS investigations (
 id INTEGER PRIMARY KEY AUTOINCREMENT,
 title TEXT NOT NULL,
 created_at_ms INTEGER NOT NULL,
 updated_at_ms INTEGER NOT NULL,
 layout_json TEXT NOT NULL DEFAULT '{}'
);
CREATE INDEX IF NOT EXISTS investigations_updated_idx ON investigations(updated_at_ms DESC, id DESC);
CREATE TABLE IF NOT EXISTS investigation_items (
 id INTEGER PRIMARY KEY AUTOINCREMENT,
 investigation_id INTEGER NOT NULL REFERENCES investigations(id) ON DELETE CASCADE,
 kind TEXT NOT NULL CHECK(kind IN ('text','note','image','link','voice','answer')),
 title TEXT NOT NULL DEFAULT '',
 body TEXT NOT NULL DEFAULT '',
 url TEXT,
 link_title TEXT,
 link_description TEXT,
 link_site TEXT,
 image_sha256 TEXT CHECK(image_sha256 IS NULL OR length(image_sha256)=64),
 image_width INTEGER,
 image_height INTEGER,
 favicon_sha256 TEXT CHECK(favicon_sha256 IS NULL OR length(favicon_sha256)=64),
 ocr_state TEXT CHECK(ocr_state IS NULL OR ocr_state IN ('pending','done','failed')),
 fetch_state TEXT CHECK(fetch_state IS NULL OR fetch_state IN ('pending','done','failed')),
 source TEXT NOT NULL DEFAULT 'typed',
 created_at_ms INTEGER NOT NULL,
 updated_at_ms INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS investigation_items_order_idx ON investigation_items(investigation_id, created_at_ms, id);
CREATE INDEX IF NOT EXISTS investigation_items_ocr_idx ON investigation_items(ocr_state, id) WHERE ocr_state='pending';
CREATE INDEX IF NOT EXISTS investigation_items_fetch_idx ON investigation_items(fetch_state, id) WHERE fetch_state='pending';
CREATE TABLE IF NOT EXISTS investigation_ocr_lines (
 item_id INTEGER NOT NULL REFERENCES investigation_items(id) ON DELETE CASCADE,
 line_index INTEGER NOT NULL,
 text TEXT NOT NULL,
 confidence REAL NOT NULL,
 left_px INTEGER NOT NULL,
 top_px INTEGER NOT NULL,
 width_px INTEGER NOT NULL,
 height_px INTEGER NOT NULL,
 PRIMARY KEY(item_id, line_index)
);
CREATE TABLE IF NOT EXISTS investigation_nodes (
 id INTEGER PRIMARY KEY AUTOINCREMENT,
 investigation_id INTEGER NOT NULL REFERENCES investigations(id) ON DELETE CASCADE,
 item_id INTEGER UNIQUE REFERENCES investigation_items(id) ON DELETE CASCADE,
 label TEXT NOT NULL DEFAULT '',
 x REAL NOT NULL,
 y REAL NOT NULL
);
CREATE INDEX IF NOT EXISTS investigation_nodes_owner_idx ON investigation_nodes(investigation_id, id);
CREATE TABLE IF NOT EXISTS investigation_edges (
 id INTEGER PRIMARY KEY AUTOINCREMENT,
 investigation_id INTEGER NOT NULL REFERENCES investigations(id) ON DELETE CASCADE,
 from_node INTEGER NOT NULL REFERENCES investigation_nodes(id) ON DELETE CASCADE,
 to_node INTEGER NOT NULL REFERENCES investigation_nodes(id) ON DELETE CASCADE,
 label TEXT NOT NULL DEFAULT '',
 status TEXT NOT NULL CHECK(status IN ('accepted','suggested')),
 rationale TEXT NOT NULL DEFAULT '',
 created_at_ms INTEGER NOT NULL,
 CHECK(from_node <> to_node),
 UNIQUE(from_node, to_node)
);
CREATE INDEX IF NOT EXISTS investigation_edges_owner_idx ON investigation_edges(investigation_id, id);
CREATE TABLE IF NOT EXISTS investigation_messages (
 id INTEGER PRIMARY KEY AUTOINCREMENT,
 investigation_id INTEGER NOT NULL REFERENCES investigations(id) ON DELETE CASCADE,
 role TEXT NOT NULL CHECK(role IN ('user','assistant')),
 status TEXT NOT NULL CHECK(status IN ('streaming','complete','cancelled','failed')),
 text TEXT NOT NULL,
 created_at_ms INTEGER NOT NULL,
 updated_at_ms INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS investigation_messages_owner_idx ON investigation_messages(investigation_id, id);
"#;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum InvestigationItemKind {
    Text,
    Note,
    Image,
    Link,
    Voice,
    Answer,
}

impl InvestigationItemKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Note => "note",
            Self::Image => "image",
            Self::Link => "link",
            Self::Voice => "voice",
            Self::Answer => "answer",
        }
    }
    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "text" => Self::Text,
            "note" => Self::Note,
            "image" => Self::Image,
            "link" => Self::Link,
            "voice" => Self::Voice,
            "answer" => Self::Answer,
            _ => return None,
        })
    }
}

/// Background work state for OCR (images) and preview fetches (links).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkState {
    Pending,
    Done,
    Failed,
}

impl WorkState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Done => "done",
            Self::Failed => "failed",
        }
    }
    fn parse(value: Option<String>) -> Option<Self> {
        match value.as_deref() {
            Some("pending") => Some(Self::Pending),
            Some("done") => Some(Self::Done),
            Some("failed") => Some(Self::Failed),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InvestigationRecord {
    pub id: i64,
    pub title: String,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub item_count: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageReference {
    pub sha256: String,
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InvestigationItemRecord {
    pub id: i64,
    pub investigation_id: i64,
    pub kind: InvestigationItemKind,
    pub title: String,
    pub body: String,
    pub url: Option<String>,
    pub link_title: Option<String>,
    pub link_description: Option<String>,
    pub link_site: Option<String>,
    pub image: Option<ImageReference>,
    pub favicon_sha256: Option<String>,
    pub ocr_state: Option<WorkState>,
    pub fetch_state: Option<WorkState>,
    pub source: String,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewInvestigationItem {
    pub kind: InvestigationItemKind,
    pub title: String,
    pub body: String,
    pub url: Option<String>,
    pub image: Option<ImageReference>,
    pub source: String,
}

impl NewInvestigationItem {
    pub fn new(kind: InvestigationItemKind, body: impl Into<String>) -> Self {
        Self {
            kind,
            title: String::new(),
            body: body.into(),
            url: None,
            image: None,
            source: "typed".into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct OcrLineRecord {
    pub text: String,
    pub confidence: f32,
    pub left: u32,
    pub top: u32,
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LinkPreviewUpdate {
    pub title: Option<String>,
    pub description: Option<String>,
    pub site: Option<String>,
    pub image: Option<ImageReference>,
    pub favicon_sha256: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct InvestigationNodeRecord {
    pub id: i64,
    pub item_id: Option<i64>,
    pub label: String,
    pub x: f64,
    pub y: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EdgeStatus {
    Accepted,
    Suggested,
}

impl EdgeStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::Suggested => "suggested",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InvestigationEdgeRecord {
    pub id: i64,
    pub from_node: i64,
    pub to_node: i64,
    pub label: String,
    pub status: EdgeStatus,
    pub rationale: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InvestigationMessageStatus {
    Streaming,
    Complete,
    Cancelled,
    Failed,
}

impl InvestigationMessageStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Streaming => "streaming",
            Self::Complete => "complete",
            Self::Cancelled => "cancelled",
            Self::Failed => "failed",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InvestigationMessageRecord {
    pub id: i64,
    pub role: String,
    pub status: InvestigationMessageStatus,
    pub text: String,
    pub created_at_ms: i64,
}

/// A queued background job: the item and the investigation folder it uses.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingWork {
    pub item_id: i64,
    pub investigation_id: i64,
    /// Image hash for OCR jobs, URL for link jobs.
    pub target: String,
}

pub struct InvestigationRepository<'a> {
    connection: &'a Connection,
}

const ITEM_COLUMNS: &str = "id,investigation_id,kind,title,body,url,link_title,link_description,link_site,image_sha256,image_width,image_height,favicon_sha256,ocr_state,fetch_state,source,created_at_ms,updated_at_ms";

fn item_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<InvestigationItemRecord> {
    let kind: String = row.get(2)?;
    let image = row
        .get::<_, Option<String>>(9)?
        .map(|sha256| -> rusqlite::Result<ImageReference> {
            Ok(ImageReference {
                sha256,
                width: row.get::<_, Option<u32>>(10)?.unwrap_or(0),
                height: row.get::<_, Option<u32>>(11)?.unwrap_or(0),
            })
        })
        .transpose()?;
    Ok(InvestigationItemRecord {
        id: row.get(0)?,
        investigation_id: row.get(1)?,
        kind: InvestigationItemKind::parse(&kind).ok_or(rusqlite::Error::InvalidQuery)?,
        title: row.get(3)?,
        body: row.get(4)?,
        url: row.get(5)?,
        link_title: row.get(6)?,
        link_description: row.get(7)?,
        link_site: row.get(8)?,
        image,
        favicon_sha256: row.get(12)?,
        ocr_state: WorkState::parse(row.get(13)?),
        fetch_state: WorkState::parse(row.get(14)?),
        source: row.get(15)?,
        created_at_ms: row.get(16)?,
        updated_at_ms: row.get(17)?,
    })
}

impl<'a> InvestigationRepository<'a> {
    pub(crate) fn new(connection: &'a Connection) -> Self {
        Self { connection }
    }

    pub fn create(&self, title: &str, now_ms: i64) -> Result<i64> {
        let title = clean_title(
            title,
            "Untitled investigation",
            MAX_INVESTIGATION_TITLE_CHARS,
        );
        self.connection.execute(
            "INSERT INTO investigations(title,created_at_ms,updated_at_ms) VALUES (?1,?2,?2)",
            params![title, now_ms],
        )?;
        Ok(self.connection.last_insert_rowid())
    }

    pub fn list(&self) -> Result<Vec<InvestigationRecord>> {
        let mut statement = self.connection.prepare(
            "SELECT i.id,i.title,i.created_at_ms,i.updated_at_ms,\
             (SELECT COUNT(*) FROM investigation_items WHERE investigation_id=i.id) \
             FROM investigations i ORDER BY i.updated_at_ms DESC, i.id DESC LIMIT 1000",
        )?;
        Ok(statement
            .query_map([], investigation_from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn get(&self, id: i64) -> Result<Option<InvestigationRecord>> {
        Ok(self
            .connection
            .query_row(
                "SELECT i.id,i.title,i.created_at_ms,i.updated_at_ms,\
                 (SELECT COUNT(*) FROM investigation_items WHERE investigation_id=i.id) \
                 FROM investigations i WHERE i.id=?1",
                [id],
                investigation_from_row,
            )
            .optional()?)
    }

    pub fn rename(&self, id: i64, title: &str, now_ms: i64) -> Result<bool> {
        let title = clean_title(
            title,
            "Untitled investigation",
            MAX_INVESTIGATION_TITLE_CHARS,
        );
        Ok(self.connection.execute(
            "UPDATE investigations SET title=?2,updated_at_ms=?3 WHERE id=?1",
            params![id, title, now_ms],
        )? > 0)
    }

    /// Removes the investigation and every row that belongs to it. The caller
    /// deletes the investigation's image folder.
    pub fn delete(&self, id: i64) -> Result<bool> {
        let tx = self.connection.unchecked_transaction()?;
        let deleted = tx.execute("DELETE FROM investigations WHERE id=?1", [id])? > 0;
        tx.execute(
            "DELETE FROM persistence_settings WHERE key=?1 AND value=?2",
            params![ACTIVE_KEY, id.to_string()],
        )?;
        tx.commit()?;
        Ok(deleted)
    }

    /// The investigation that quick capture writes into, if it still exists.
    pub fn active(&self) -> Result<Option<i64>> {
        let value: Option<String> = self
            .connection
            .query_row(
                "SELECT value FROM persistence_settings WHERE key=?1",
                [ACTIVE_KEY],
                |row| row.get(0),
            )
            .optional()?;
        let Some(id) = value.and_then(|value| value.parse::<i64>().ok()) else {
            return Ok(None);
        };
        Ok(self.get(id)?.map(|record| record.id))
    }

    pub fn set_active(&self, id: Option<i64>) -> Result<()> {
        match id {
            Some(id) => {
                if self.get(id)?.is_none() {
                    return Err(invalid("investigation", "does not exist"));
                }
                self.connection.execute(
                    "INSERT INTO persistence_settings(key,value) VALUES (?1,?2) \
                     ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                    params![ACTIVE_KEY, id.to_string()],
                )?;
            }
            None => {
                self.connection.execute(
                    "DELETE FROM persistence_settings WHERE key=?1",
                    [ACTIVE_KEY],
                )?;
            }
        }
        Ok(())
    }

    /// The quick-capture shortcut text. `None` means never configured (the
    /// host applies its default); an empty string means turned off.
    pub fn capture_shortcut(&self) -> Result<Option<String>> {
        Ok(self
            .connection
            .query_row(
                "SELECT value FROM persistence_settings WHERE key=?1",
                [CAPTURE_SHORTCUT_KEY],
                |row| row.get(0),
            )
            .optional()?)
    }

    pub fn set_capture_shortcut(&self, shortcut: &str) -> Result<()> {
        bounded("investigation_capture_shortcut", shortcut, 80)?;
        self.connection.execute(
            "INSERT INTO persistence_settings(key,value) VALUES (?1,?2)              ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            params![CAPTURE_SHORTCUT_KEY, shortcut],
        )?;
        Ok(())
    }

    pub fn add_item(
        &self,
        investigation_id: i64,
        item: &NewInvestigationItem,
        now_ms: i64,
    ) -> Result<i64> {
        bounded("investigation_item_body", &item.body, MAX_ITEM_BODY_BYTES)?;
        if let Some(url) = &item.url {
            bounded("investigation_item_url", url, MAX_ITEM_URL_BYTES)?;
        }
        if let Some(image) = &item.image {
            check_sha(&image.sha256)?;
        }
        if item.source.len() > 32 {
            return Err(invalid("investigation_item_source", "too long"));
        }
        match item.kind {
            InvestigationItemKind::Image if item.image.is_none() => {
                return Err(invalid(
                    "investigation_item",
                    "an image item needs an image",
                ));
            }
            InvestigationItemKind::Link if item.url.is_none() => {
                return Err(invalid("investigation_item", "a link item needs a URL"));
            }
            _ => {}
        }
        let tx = self.connection.unchecked_transaction()?;
        let count: i64 = tx.query_row(
            "SELECT COUNT(*) FROM investigation_items WHERE investigation_id=?1",
            [investigation_id],
            |row| row.get(0),
        )?;
        if count as usize >= MAX_ITEMS_PER_INVESTIGATION {
            return Err(PersistenceError::CollectionLimitExceeded {
                field: "investigation_items",
                max_items: MAX_ITEMS_PER_INVESTIGATION,
                actual_items: count as usize + 1,
            });
        }
        let ocr = (item.kind == InvestigationItemKind::Image).then_some("pending");
        let fetch = (item.kind == InvestigationItemKind::Link).then_some("pending");
        let changed = tx.execute(
            "INSERT INTO investigation_items(investigation_id,kind,title,body,url,image_sha256,\
             image_width,image_height,ocr_state,fetch_state,source,created_at_ms,updated_at_ms) \
             SELECT id,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?12 FROM investigations WHERE id=?1",
            params![
                investigation_id,
                item.kind.as_str(),
                clean_title(&item.title, "", MAX_ITEM_TITLE_CHARS),
                item.body,
                item.url,
                item.image.as_ref().map(|image| image.sha256.as_str()),
                item.image.as_ref().map(|image| image.width),
                item.image.as_ref().map(|image| image.height),
                ocr,
                fetch,
                item.source,
                now_ms,
            ],
        )?;
        if changed == 0 {
            return Err(invalid("investigation", "does not exist"));
        }
        let id = tx.last_insert_rowid();
        touch(&tx, investigation_id, now_ms)?;
        tx.commit()?;
        Ok(id)
    }

    pub fn item(&self, id: i64) -> Result<Option<InvestigationItemRecord>> {
        Ok(self
            .connection
            .query_row(
                &format!("SELECT {ITEM_COLUMNS} FROM investigation_items WHERE id=?1"),
                [id],
                item_from_row,
            )
            .optional()?)
    }

    /// Items in chronological order. `query` is a case-insensitive exact
    /// phrase searched in titles, text, extracted text and link previews.
    pub fn items(
        &self,
        investigation_id: i64,
        kind: Option<InvestigationItemKind>,
        query: Option<&str>,
    ) -> Result<Vec<InvestigationItemRecord>> {
        let query = query.map(str::trim).filter(|query| !query.is_empty());
        if let Some(query) = query {
            bounded("investigation_search", query, 1024)?;
        }
        let mut statement = self.connection.prepare(&format!(
            "SELECT {ITEM_COLUMNS} FROM investigation_items WHERE investigation_id=?1 \
             AND (?2 IS NULL OR kind=?2) AND (?3 IS NULL OR phorminx_find(title,?3)>0 \
             OR phorminx_find(body,?3)>0 OR phorminx_find(COALESCE(url,''),?3)>0 \
             OR phorminx_find(COALESCE(link_title,''),?3)>0 \
             OR phorminx_find(COALESCE(link_description,''),?3)>0 \
             OR phorminx_find(COALESCE(link_site,''),?3)>0) \
             ORDER BY created_at_ms, id LIMIT {MAX_ITEMS_PER_INVESTIGATION}"
        ))?;
        Ok(statement
            .query_map(
                params![
                    investigation_id,
                    kind.map(InvestigationItemKind::as_str),
                    query
                ],
                item_from_row,
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn update_item_text(&self, id: i64, title: &str, body: &str, now_ms: i64) -> Result<bool> {
        bounded("investigation_item_body", body, MAX_ITEM_BODY_BYTES)?;
        let tx = self.connection.unchecked_transaction()?;
        let Some(owner) = item_owner(&tx, id)? else {
            return Ok(false);
        };
        tx.execute(
            "UPDATE investigation_items SET title=?2,body=?3,updated_at_ms=?4 WHERE id=?1",
            params![
                id,
                clean_title(title, "", MAX_ITEM_TITLE_CHARS),
                body,
                now_ms
            ],
        )?;
        touch(&tx, owner, now_ms)?;
        tx.commit()?;
        Ok(true)
    }

    /// Deletes one item and returns image hashes that no other item in the
    /// same investigation still references, so the caller can remove them.
    pub fn delete_item(&self, id: i64, now_ms: i64) -> Result<Option<(i64, Vec<String>)>> {
        let tx = self.connection.unchecked_transaction()?;
        let Some(item) = tx
            .query_row(
                &format!("SELECT {ITEM_COLUMNS} FROM investigation_items WHERE id=?1"),
                [id],
                item_from_row,
            )
            .optional()?
        else {
            return Ok(None);
        };
        tx.execute("DELETE FROM investigation_items WHERE id=?1", [id])?;
        let mut orphaned = Vec::new();
        for sha in item
            .image
            .iter()
            .map(|image| image.sha256.clone())
            .chain(item.favicon_sha256.clone())
        {
            let used: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM investigation_items WHERE investigation_id=?1 \
                 AND (image_sha256=?2 OR favicon_sha256=?2))",
                params![item.investigation_id, sha],
                |row| row.get(0),
            )?;
            if !used && !orphaned.contains(&sha) {
                orphaned.push(sha);
            }
        }
        touch(&tx, item.investigation_id, now_ms)?;
        tx.commit()?;
        Ok(Some((item.investigation_id, orphaned)))
    }

    /// Every image hash an investigation references (screenshots, previews
    /// and favicons), used to clean up files that lost their item.
    pub fn image_hashes(&self, investigation_id: i64) -> Result<Vec<String>> {
        let mut statement = self.connection.prepare(
            "SELECT image_sha256 FROM investigation_items WHERE investigation_id=?1 AND image_sha256 IS NOT NULL \
             UNION SELECT favicon_sha256 FROM investigation_items WHERE investigation_id=?1 AND favicon_sha256 IS NOT NULL",
        )?;
        Ok(statement
            .query_map([investigation_id], |row| row.get(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn next_pending_ocr(&self) -> Result<Option<PendingWork>> {
        Ok(self
            .connection
            .query_row(
                "SELECT id,investigation_id,image_sha256 FROM investigation_items \
                 WHERE ocr_state='pending' AND image_sha256 IS NOT NULL ORDER BY id LIMIT 1",
                [],
                |row| {
                    Ok(PendingWork {
                        item_id: row.get(0)?,
                        investigation_id: row.get(1)?,
                        target: row.get(2)?,
                    })
                },
            )
            .optional()?)
    }

    pub fn pending_ocr_count(&self) -> Result<u64> {
        Ok(self.connection.query_row(
            "SELECT COUNT(*) FROM investigation_items WHERE ocr_state='pending'",
            [],
            |row| row.get(0),
        )?)
    }

    /// Stores recognised lines. The joined text becomes the item's editable
    /// text unless the user already typed something there.
    pub fn save_ocr(&self, item_id: i64, lines: &[OcrLineRecord], now_ms: i64) -> Result<bool> {
        let lines = &lines[..lines.len().min(MAX_OCR_LINES)];
        let mut text = String::new();
        for line in lines {
            if text.len() + line.text.len() + 1 > MAX_ITEM_BODY_BYTES {
                break;
            }
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str(&line.text);
        }
        let tx = self.connection.unchecked_transaction()?;
        let changed = tx.execute(
            "UPDATE investigation_items SET ocr_state='done',updated_at_ms=?3,\
             body=CASE WHEN body='' THEN ?2 ELSE body END WHERE id=?1 AND ocr_state='pending'",
            params![item_id, text, now_ms],
        )?;
        if changed == 0 {
            return Ok(false);
        }
        tx.execute(
            "DELETE FROM investigation_ocr_lines WHERE item_id=?1",
            [item_id],
        )?;
        for (index, line) in lines.iter().enumerate() {
            bounded("investigation_ocr_line", &line.text, 16 * 1024)?;
            tx.execute(
                "INSERT INTO investigation_ocr_lines(item_id,line_index,text,confidence,left_px,top_px,width_px,height_px) \
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
                params![
                    item_id,
                    index as i64,
                    line.text,
                    f64::from(line.confidence),
                    line.left,
                    line.top,
                    line.width,
                    line.height
                ],
            )?;
        }
        tx.commit()?;
        Ok(true)
    }

    pub fn fail_ocr(&self, item_id: i64, now_ms: i64) -> Result<bool> {
        Ok(self.connection.execute(
            "UPDATE investigation_items SET ocr_state='failed',updated_at_ms=?2 WHERE id=?1 AND ocr_state='pending'",
            params![item_id, now_ms],
        )? > 0)
    }

    /// Queues an image for another recognition attempt.
    pub fn retry_ocr(&self, item_id: i64, now_ms: i64) -> Result<bool> {
        Ok(self.connection.execute(
            "UPDATE investigation_items SET ocr_state='pending',updated_at_ms=?2 \
             WHERE id=?1 AND kind='image' AND ocr_state IN ('failed','done')",
            params![item_id, now_ms],
        )? > 0)
    }

    pub fn ocr_lines(&self, item_id: i64) -> Result<Vec<OcrLineRecord>> {
        let mut statement = self.connection.prepare(
            "SELECT text,confidence,left_px,top_px,width_px,height_px FROM investigation_ocr_lines \
             WHERE item_id=?1 ORDER BY line_index",
        )?;
        Ok(statement
            .query_map([item_id], |row| {
                Ok(OcrLineRecord {
                    text: row.get(0)?,
                    confidence: row.get::<_, f64>(1)? as f32,
                    left: row.get(2)?,
                    top: row.get(3)?,
                    width: row.get(4)?,
                    height: row.get(5)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn next_pending_fetch(&self) -> Result<Option<PendingWork>> {
        Ok(self
            .connection
            .query_row(
                "SELECT id,investigation_id,url FROM investigation_items \
                 WHERE fetch_state='pending' AND url IS NOT NULL ORDER BY id LIMIT 1",
                [],
                |row| {
                    Ok(PendingWork {
                        item_id: row.get(0)?,
                        investigation_id: row.get(1)?,
                        target: row.get(2)?,
                    })
                },
            )
            .optional()?)
    }

    pub fn save_link_preview(
        &self,
        item_id: i64,
        preview: &LinkPreviewUpdate,
        now_ms: i64,
    ) -> Result<bool> {
        if let Some(image) = &preview.image {
            check_sha(&image.sha256)?;
        }
        if let Some(sha) = &preview.favicon_sha256 {
            check_sha(sha)?;
        }
        let clip = |value: &Option<String>, limit| {
            value
                .as_deref()
                .map(|value| clean_title(value, "", limit))
                .filter(|value| !value.is_empty())
        };
        Ok(self.connection.execute(
            "UPDATE investigation_items SET fetch_state='done',link_title=?2,link_description=?3,\
             link_site=?4,image_sha256=?5,image_width=?6,image_height=?7,favicon_sha256=?8,updated_at_ms=?9 \
             WHERE id=?1 AND fetch_state='pending'",
            params![
                item_id,
                clip(&preview.title, MAX_ITEM_TITLE_CHARS),
                clip(&preview.description, 1000),
                clip(&preview.site, 120),
                preview.image.as_ref().map(|image| image.sha256.as_str()),
                preview.image.as_ref().map(|image| image.width),
                preview.image.as_ref().map(|image| image.height),
                preview.favicon_sha256,
                now_ms,
            ],
        )? > 0)
    }

    pub fn fail_fetch(&self, item_id: i64, now_ms: i64) -> Result<bool> {
        Ok(self.connection.execute(
            "UPDATE investigation_items SET fetch_state='failed',updated_at_ms=?2 WHERE id=?1 AND fetch_state='pending'",
            params![item_id, now_ms],
        )? > 0)
    }

    pub fn retry_fetch(&self, item_id: i64, now_ms: i64) -> Result<bool> {
        Ok(self.connection.execute(
            "UPDATE investigation_items SET fetch_state='pending',updated_at_ms=?2 \
             WHERE id=?1 AND kind='link' AND fetch_state='failed'",
            params![item_id, now_ms],
        )? > 0)
    }

    pub fn layout(&self, investigation_id: i64) -> Result<String> {
        Ok(self
            .connection
            .query_row(
                "SELECT layout_json FROM investigations WHERE id=?1",
                [investigation_id],
                |row| row.get(0),
            )
            .optional()?
            .unwrap_or_else(|| "{}".into()))
    }

    /// Saves the diagram viewport (an opaque, bounded JSON object).
    pub fn save_layout(&self, investigation_id: i64, layout_json: &str) -> Result<bool> {
        bounded("investigation_layout", layout_json, MAX_LAYOUT_BYTES)?;
        serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(layout_json)?;
        Ok(self.connection.execute(
            "UPDATE investigations SET layout_json=?2 WHERE id=?1",
            params![investigation_id, layout_json],
        )? > 0)
    }

    pub fn nodes(&self, investigation_id: i64) -> Result<Vec<InvestigationNodeRecord>> {
        let mut statement = self.connection.prepare(
            "SELECT id,item_id,label,x,y FROM investigation_nodes WHERE investigation_id=?1 ORDER BY id",
        )?;
        Ok(statement
            .query_map([investigation_id], |row| {
                Ok(InvestigationNodeRecord {
                    id: row.get(0)?,
                    item_id: row.get(1)?,
                    label: row.get(2)?,
                    x: row.get(3)?,
                    y: row.get(4)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Places an item on the diagram (or moves it if already placed) and
    /// returns its node id.
    pub fn place_item(&self, item_id: i64, x: f64, y: f64, now_ms: i64) -> Result<i64> {
        let (x, y) = (coordinate(x)?, coordinate(y)?);
        let tx = self.connection.unchecked_transaction()?;
        let owner =
            item_owner(&tx, item_id)?.ok_or(invalid("investigation_item", "does not exist"))?;
        let existing: Option<i64> = tx
            .query_row(
                "SELECT id FROM investigation_nodes WHERE item_id=?1",
                [item_id],
                |row| row.get(0),
            )
            .optional()?;
        let id = match existing {
            Some(id) => {
                tx.execute(
                    "UPDATE investigation_nodes SET x=?2,y=?3 WHERE id=?1",
                    params![id, x, y],
                )?;
                id
            }
            None => {
                check_node_capacity(&tx, owner)?;
                tx.execute(
                    "INSERT INTO investigation_nodes(investigation_id,item_id,x,y) VALUES (?1,?2,?3,?4)",
                    params![owner, item_id, x, y],
                )?;
                tx.last_insert_rowid()
            }
        };
        touch(&tx, owner, now_ms)?;
        tx.commit()?;
        Ok(id)
    }

    pub fn add_concept(
        &self,
        investigation_id: i64,
        label: &str,
        x: f64,
        y: f64,
        now_ms: i64,
    ) -> Result<i64> {
        let (x, y) = (coordinate(x)?, coordinate(y)?);
        let tx = self.connection.unchecked_transaction()?;
        check_node_capacity(&tx, investigation_id)?;
        let changed = tx.execute(
            "INSERT INTO investigation_nodes(investigation_id,label,x,y) SELECT id,?2,?3,?4 FROM investigations WHERE id=?1",
            params![
                investigation_id,
                clean_title(label, "Concept", MAX_ITEM_TITLE_CHARS),
                x,
                y
            ],
        )?;
        if changed == 0 {
            return Err(invalid("investigation", "does not exist"));
        }
        let id = tx.last_insert_rowid();
        touch(&tx, investigation_id, now_ms)?;
        tx.commit()?;
        Ok(id)
    }

    pub fn move_node(&self, node_id: i64, x: f64, y: f64) -> Result<bool> {
        Ok(self.connection.execute(
            "UPDATE investigation_nodes SET x=?2,y=?3 WHERE id=?1",
            params![node_id, coordinate(x)?, coordinate(y)?],
        )? > 0)
    }

    pub fn rename_node(&self, node_id: i64, label: &str) -> Result<bool> {
        Ok(self.connection.execute(
            "UPDATE investigation_nodes SET label=?2 WHERE id=?1 AND item_id IS NULL",
            params![node_id, clean_title(label, "Concept", MAX_ITEM_TITLE_CHARS)],
        )? > 0)
    }

    /// Removes a node from the diagram. An item node returns its item to the
    /// unplaced tray; the item itself is kept. Connected edges are removed.
    pub fn delete_node(&self, node_id: i64) -> Result<bool> {
        Ok(self
            .connection
            .execute("DELETE FROM investigation_nodes WHERE id=?1", [node_id])?
            > 0)
    }

    pub fn node_owner(&self, node_id: i64) -> Result<Option<i64>> {
        Ok(self
            .connection
            .query_row(
                "SELECT investigation_id FROM investigation_nodes WHERE id=?1",
                [node_id],
                |row| row.get(0),
            )
            .optional()?)
    }

    pub fn item_owner(&self, item_id: i64) -> Result<Option<i64>> {
        item_owner(self.connection, item_id)
    }

    /// Connects two nodes of the same investigation. Returns `None` when the
    /// pair (in either direction) is already connected.
    pub fn add_edge(
        &self,
        from_node: i64,
        to_node: i64,
        status: EdgeStatus,
        label: &str,
        rationale: &str,
        now_ms: i64,
    ) -> Result<Option<i64>> {
        if from_node == to_node {
            return Err(invalid(
                "investigation_edge",
                "a node cannot connect to itself",
            ));
        }
        bounded("investigation_edge_rationale", rationale, 2048)?;
        let tx = self.connection.unchecked_transaction()?;
        let owners: (Option<i64>, Option<i64>) = (
            tx.query_row(
                "SELECT investigation_id FROM investigation_nodes WHERE id=?1",
                [from_node],
                |row| row.get(0),
            )
            .optional()?,
            tx.query_row(
                "SELECT investigation_id FROM investigation_nodes WHERE id=?1",
                [to_node],
                |row| row.get(0),
            )
            .optional()?,
        );
        let owner = match owners {
            (Some(a), Some(b)) if a == b => a,
            _ => {
                return Err(invalid(
                    "investigation_edge",
                    "nodes must share an investigation",
                ));
            }
        };
        let exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM investigation_edges WHERE (from_node=?1 AND to_node=?2) OR (from_node=?2 AND to_node=?1))",
            params![from_node, to_node],
            |row| row.get(0),
        )?;
        if exists {
            return Ok(None);
        }
        let count: i64 = tx.query_row(
            "SELECT COUNT(*) FROM investigation_edges WHERE investigation_id=?1",
            [owner],
            |row| row.get(0),
        )?;
        if count as usize >= MAX_EDGES_PER_INVESTIGATION {
            return Err(PersistenceError::CollectionLimitExceeded {
                field: "investigation_edges",
                max_items: MAX_EDGES_PER_INVESTIGATION,
                actual_items: count as usize + 1,
            });
        }
        tx.execute(
            "INSERT INTO investigation_edges(investigation_id,from_node,to_node,label,status,rationale,created_at_ms) \
             VALUES (?1,?2,?3,?4,?5,?6,?7)",
            params![
                owner,
                from_node,
                to_node,
                clean_title(label, "", 120),
                status.as_str(),
                rationale,
                now_ms
            ],
        )?;
        let id = tx.last_insert_rowid();
        touch(&tx, owner, now_ms)?;
        tx.commit()?;
        Ok(Some(id))
    }

    pub fn edges(&self, investigation_id: i64) -> Result<Vec<InvestigationEdgeRecord>> {
        let mut statement = self.connection.prepare(
            "SELECT id,from_node,to_node,label,status,rationale FROM investigation_edges \
             WHERE investigation_id=?1 ORDER BY id",
        )?;
        Ok(statement
            .query_map([investigation_id], |row| {
                let status: String = row.get(4)?;
                Ok(InvestigationEdgeRecord {
                    id: row.get(0)?,
                    from_node: row.get(1)?,
                    to_node: row.get(2)?,
                    label: row.get(3)?,
                    status: if status == "suggested" {
                        EdgeStatus::Suggested
                    } else {
                        EdgeStatus::Accepted
                    },
                    rationale: row.get(5)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn edge_owner(&self, edge_id: i64) -> Result<Option<i64>> {
        Ok(self
            .connection
            .query_row(
                "SELECT investigation_id FROM investigation_edges WHERE id=?1",
                [edge_id],
                |row| row.get(0),
            )
            .optional()?)
    }

    /// Turns a suggested (dashed) connection into a real one.
    pub fn accept_edge(&self, edge_id: i64) -> Result<bool> {
        Ok(self.connection.execute(
            "UPDATE investigation_edges SET status='accepted' WHERE id=?1 AND status='suggested'",
            [edge_id],
        )? > 0)
    }

    pub fn delete_edge(&self, edge_id: i64) -> Result<bool> {
        Ok(self
            .connection
            .execute("DELETE FROM investigation_edges WHERE id=?1", [edge_id])?
            > 0)
    }

    pub fn clear_suggestions(&self, investigation_id: i64) -> Result<usize> {
        Ok(self.connection.execute(
            "DELETE FROM investigation_edges WHERE investigation_id=?1 AND status='suggested'",
            [investigation_id],
        )?)
    }

    pub fn append_message(
        &self,
        investigation_id: i64,
        role: &str,
        status: InvestigationMessageStatus,
        text: &str,
        now_ms: i64,
    ) -> Result<i64> {
        if !matches!(role, "user" | "assistant") {
            return Err(invalid("investigation_message", "unknown role"));
        }
        bounded("investigation_message", text, MAX_MESSAGE_BYTES)?;
        let changed = self.connection.execute(
            "INSERT INTO investigation_messages(investigation_id,role,status,text,created_at_ms,updated_at_ms) \
             SELECT id,?2,?3,?4,?5,?5 FROM investigations WHERE id=?1",
            params![investigation_id, role, status.as_str(), text, now_ms],
        )?;
        if changed == 0 {
            return Err(invalid("investigation", "does not exist"));
        }
        Ok(self.connection.last_insert_rowid())
    }

    /// Updates a streaming answer. Finished messages are never rewritten.
    pub fn update_message(
        &self,
        id: i64,
        text: &str,
        status: InvestigationMessageStatus,
        now_ms: i64,
    ) -> Result<bool> {
        bounded("investigation_message", text, MAX_MESSAGE_BYTES)?;
        Ok(self.connection.execute(
            "UPDATE investigation_messages SET text=?2,status=?3,updated_at_ms=?4 WHERE id=?1 AND status='streaming'",
            params![id, text, status.as_str(), now_ms],
        )? > 0)
    }

    pub fn messages(
        &self,
        investigation_id: i64,
        limit: usize,
    ) -> Result<Vec<InvestigationMessageRecord>> {
        let mut statement = self.connection.prepare(
            "SELECT id,role,status,text,created_at_ms FROM (SELECT * FROM investigation_messages \
             WHERE investigation_id=?1 ORDER BY id DESC LIMIT ?2) ORDER BY id",
        )?;
        Ok(statement
            .query_map(params![investigation_id, limit.min(500) as i64], |row| {
                let status: String = row.get(2)?;
                Ok(InvestigationMessageRecord {
                    id: row.get(0)?,
                    role: row.get(1)?,
                    status: match status.as_str() {
                        "streaming" => InvestigationMessageStatus::Streaming,
                        "complete" => InvestigationMessageStatus::Complete,
                        "cancelled" => InvestigationMessageStatus::Cancelled,
                        _ => InvestigationMessageStatus::Failed,
                    },
                    text: row.get(3)?,
                    created_at_ms: row.get(4)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn message(&self, id: i64) -> Result<Option<(i64, InvestigationMessageRecord)>> {
        Ok(self
            .connection
            .query_row(
                "SELECT investigation_id,id,role,status,text,created_at_ms FROM investigation_messages WHERE id=?1",
                [id],
                |row| {
                    let status: String = row.get(3)?;
                    Ok((
                        row.get(0)?,
                        InvestigationMessageRecord {
                            id: row.get(1)?,
                            role: row.get(2)?,
                            status: if status == "complete" {
                                InvestigationMessageStatus::Complete
                            } else if status == "streaming" {
                                InvestigationMessageStatus::Streaming
                            } else if status == "cancelled" {
                                InvestigationMessageStatus::Cancelled
                            } else {
                                InvestigationMessageStatus::Failed
                            },
                            text: row.get(4)?,
                            created_at_ms: row.get(5)?,
                        },
                    ))
                },
            )
            .optional()?)
    }

    pub fn clear_messages(&self, investigation_id: i64) -> Result<usize> {
        Ok(self.connection.execute(
            "DELETE FROM investigation_messages WHERE investigation_id=?1",
            [investigation_id],
        )?)
    }

    /// Answers that were streaming when Phorminx stopped can never finish.
    pub fn fail_interrupted_messages(&self, now_ms: i64) -> Result<usize> {
        Ok(self.connection.execute(
            "UPDATE investigation_messages SET status='failed',updated_at_ms=?1 WHERE status='streaming'",
            [now_ms],
        )?)
    }
}

fn investigation_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<InvestigationRecord> {
    Ok(InvestigationRecord {
        id: row.get(0)?,
        title: row.get(1)?,
        created_at_ms: row.get(2)?,
        updated_at_ms: row.get(3)?,
        item_count: row.get(4)?,
    })
}

fn item_owner(connection: &Connection, item_id: i64) -> Result<Option<i64>> {
    Ok(connection
        .query_row(
            "SELECT investigation_id FROM investigation_items WHERE id=?1",
            [item_id],
            |row| row.get(0),
        )
        .optional()?)
}

fn touch(connection: &Connection, investigation_id: i64, now_ms: i64) -> Result<()> {
    connection.execute(
        "UPDATE investigations SET updated_at_ms=MAX(updated_at_ms,?2) WHERE id=?1",
        params![investigation_id, now_ms],
    )?;
    Ok(())
}

fn check_node_capacity(connection: &Connection, investigation_id: i64) -> Result<()> {
    let count: i64 = connection.query_row(
        "SELECT COUNT(*) FROM investigation_nodes WHERE investigation_id=?1",
        [investigation_id],
        |row| row.get(0),
    )?;
    if count as usize >= MAX_NODES_PER_INVESTIGATION {
        return Err(PersistenceError::CollectionLimitExceeded {
            field: "investigation_nodes",
            max_items: MAX_NODES_PER_INVESTIGATION,
            actual_items: count as usize + 1,
        });
    }
    Ok(())
}

/// Single-line, control-free, character-bounded titles.
fn clean_title(value: &str, fallback: &str, max_chars: usize) -> String {
    let cleaned: String = value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .filter(|c| !c.is_control())
        .take(max_chars)
        .collect();
    if cleaned.is_empty() {
        fallback.to_owned()
    } else {
        cleaned
    }
}

fn coordinate(value: f64) -> Result<f64> {
    if value.is_finite() && value.abs() <= 1_000_000.0 {
        Ok(value)
    } else {
        Err(invalid("investigation_node", "coordinate out of range"))
    }
}

fn check_sha(value: &str) -> Result<()> {
    if value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        Ok(())
    } else {
        Err(invalid(
            "investigation_image",
            "expected a lowercase SHA-256 name",
        ))
    }
}

fn invalid(field: &'static str, reason: &'static str) -> PersistenceError {
    PersistenceError::Validation { field, reason }
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
