//! Reports and comments this computer sent to phorminx.net, so the Feedback
//! page can list them and the app can ask whether a report was resolved. The
//! id is the random UUID sent with the item; nothing here identifies the user.

use rusqlite::{Connection, OptionalExtension, params};

use crate::{PersistenceError, Result};

pub(crate) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS feedback_reports (
    id               TEXT PRIMARY KEY NOT NULL,
    kind             TEXT NOT NULL CHECK (kind IN ('report', 'comment')),
    sent_at_ms       INTEGER NOT NULL,
    text             TEXT NOT NULL,
    status           TEXT NOT NULL,
    resolution_note  TEXT,
    resolved_at_ms   INTEGER,
    checked_at_ms    INTEGER,
    notified         INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS feedback_reports_sent_idx ON feedback_reports(sent_at_ms DESC);
";

const MAX_TEXT_BYTES: usize = 12_000;
const MAX_NOTE_BYTES: usize = 1_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeedbackRecord {
    pub id: String,
    /// `report` or `comment`.
    pub kind: String,
    pub sent_at_ms: i64,
    pub text: String,
    /// Reports: `open` or `solved`. Comments: `shared` or `removed`.
    pub status: String,
    pub resolution_note: Option<String>,
    pub resolved_at_ms: Option<i64>,
    pub checked_at_ms: Option<i64>,
    pub notified: bool,
}

pub struct FeedbackRepository<'a> {
    connection: &'a Connection,
}

fn valid_id(id: &str) -> bool {
    (8..=80).contains(&id.len()) && id.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
}

fn row(row: &rusqlite::Row<'_>) -> rusqlite::Result<FeedbackRecord> {
    Ok(FeedbackRecord {
        id: row.get(0)?,
        kind: row.get(1)?,
        sent_at_ms: row.get(2)?,
        text: row.get(3)?,
        status: row.get(4)?,
        resolution_note: row.get(5)?,
        resolved_at_ms: row.get(6)?,
        checked_at_ms: row.get(7)?,
        notified: row.get::<_, i64>(8)? != 0,
    })
}

const COLUMNS: &str =
    "id,kind,sent_at_ms,text,status,resolution_note,resolved_at_ms,checked_at_ms,notified";

impl<'a> FeedbackRepository<'a> {
    pub(crate) fn new(connection: &'a Connection) -> Self {
        Self { connection }
    }

    /// Remembers something just sent. Sending the same id again keeps the first record.
    pub fn record(&self, id: &str, kind: &str, text: &str, now_ms: i64) -> Result<()> {
        if !valid_id(id) || !matches!(kind, "report" | "comment") || text.len() > MAX_TEXT_BYTES {
            return Err(PersistenceError::Validation {
                field: "feedback",
                reason: "invalid sent feedback",
            });
        }
        let status = if kind == "report" { "open" } else { "shared" };
        self.connection.execute(
            "INSERT OR IGNORE INTO feedback_reports(id,kind,sent_at_ms,text,status) VALUES(?1,?2,?3,?4,?5)",
            params![id.to_ascii_lowercase(), kind, now_ms, text, status],
        )?;
        Ok(())
    }

    /// Newest first.
    pub fn list(&self, limit: usize) -> Result<Vec<FeedbackRecord>> {
        let mut statement = self.connection.prepare(&format!(
            "SELECT {COLUMNS} FROM feedback_reports ORDER BY sent_at_ms DESC, id LIMIT ?1"
        ))?;
        let records = statement
            .query_map([limit.min(500) as i64], row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(records)
    }

    pub fn get(&self, id: &str) -> Result<Option<FeedbackRecord>> {
        Ok(self
            .connection
            .query_row(
                &format!("SELECT {COLUMNS} FROM feedback_reports WHERE id = ?1"),
                [id.to_ascii_lowercase()],
                row,
            )
            .optional()?)
    }

    /// Open reports not checked since `checked_before_ms`.
    pub fn due_for_check(&self, checked_before_ms: i64) -> Result<Vec<String>> {
        let mut statement = self.connection.prepare(
            "SELECT id FROM feedback_reports WHERE kind = 'report' AND status = 'open' \
             AND (checked_at_ms IS NULL OR checked_at_ms < ?1) ORDER BY sent_at_ms",
        )?;
        let ids = statement
            .query_map([checked_before_ms], |row| row.get(0))?
            .collect::<rusqlite::Result<Vec<String>>>()?;
        Ok(ids)
    }

    /// Stores what phorminx.net said about a report. Returns true when this
    /// is the first time it is seen solved, so the user is told once.
    pub fn update_status(
        &self,
        id: &str,
        status: &str,
        note: Option<&str>,
        resolved_at_ms: Option<i64>,
        now_ms: i64,
    ) -> Result<bool> {
        if !matches!(status, "open" | "solved")
            || note.is_some_and(|note| note.len() > MAX_NOTE_BYTES)
        {
            return Err(PersistenceError::Validation {
                field: "feedback_status",
                reason: "invalid status",
            });
        }
        let before = self.get(id)?;
        self.connection.execute(
            "UPDATE feedback_reports SET status = ?2, resolution_note = ?3, resolved_at_ms = ?4, checked_at_ms = ?5 WHERE id = ?1 AND kind = 'report'",
            params![id.to_ascii_lowercase(), status, note, resolved_at_ms, now_ms],
        )?;
        Ok(status == "solved"
            && before.is_some_and(|before| before.status != "solved" && !before.notified))
    }

    pub fn mark_checked(&self, id: &str, now_ms: i64) -> Result<()> {
        self.connection.execute(
            "UPDATE feedback_reports SET checked_at_ms = ?2 WHERE id = ?1",
            params![id.to_ascii_lowercase(), now_ms],
        )?;
        Ok(())
    }

    pub fn mark_notified(&self, id: &str) -> Result<()> {
        self.connection.execute(
            "UPDATE feedback_reports SET notified = 1 WHERE id = ?1",
            [id.to_ascii_lowercase()],
        )?;
        Ok(())
    }

    /// A shared comment the user removed from phorminx.net.
    pub fn mark_removed(&self, id: &str) -> Result<()> {
        self.connection.execute(
            "UPDATE feedback_reports SET status = 'removed' WHERE id = ?1 AND kind = 'comment'",
            [id.to_ascii_lowercase()],
        )?;
        Ok(())
    }

    /// Forgets one item on this computer only.
    pub fn delete(&self, id: &str) -> Result<bool> {
        Ok(self.connection.execute(
            "DELETE FROM feedback_reports WHERE id = ?1",
            [id.to_ascii_lowercase()],
        )? > 0)
    }
}

#[cfg(test)]
mod tests {
    use crate::Persistence;

    const ID: &str = "0f8c7a52-3b1e-4d2a-9c4e-1a2b3c4d5e6f";
    const COMMENT: &str = "aa8c7a52-3b1e-4d2a-9c4e-1a2b3c4d5e6f";

    #[test]
    fn reports_are_recorded_checked_and_announced_once() {
        let directory = tempfile::tempdir().unwrap();
        let store = Persistence::open(directory.path().join("phorminx.db")).unwrap();
        let feedback = store.feedback();
        feedback
            .record(ID, "report", "Recording stopped.", 10)
            .unwrap();
        feedback
            .record(&ID.to_uppercase(), "report", "duplicate", 11)
            .unwrap();
        feedback.record(COMMENT, "comment", "Love it.", 12).unwrap();
        assert!(feedback.record("not an id", "report", "x", 1).is_err());
        assert!(feedback.record(ID, "ticket", "x", 1).is_err());
        let listed = feedback.list(10).unwrap();
        assert_eq!(
            listed
                .iter()
                .map(|record| record.id.as_str())
                .collect::<Vec<_>>(),
            [COMMENT, ID]
        );
        assert_eq!(listed[1].text, "Recording stopped.");
        assert_eq!(
            (listed[0].status.as_str(), listed[1].status.as_str()),
            ("shared", "open")
        );

        assert_eq!(feedback.due_for_check(100).unwrap(), [ID]);
        assert!(!feedback.update_status(ID, "open", None, None, 50).unwrap());
        assert!(
            feedback.due_for_check(40).unwrap().is_empty(),
            "checked recently"
        );
        assert!(
            feedback
                .update_status(ID, "solved", Some("Fixed in 0.2.4."), Some(60), 70)
                .unwrap()
        );
        assert!(
            !feedback
                .update_status(ID, "solved", Some("Fixed in 0.2.4."), Some(60), 80)
                .unwrap(),
            "told once"
        );
        let solved = feedback.get(ID).unwrap().unwrap();
        assert_eq!(solved.resolution_note.as_deref(), Some("Fixed in 0.2.4."));
        feedback.mark_notified(ID).unwrap();
        assert!(feedback.get(ID).unwrap().unwrap().notified);
        assert!(
            feedback.due_for_check(i64::MAX).unwrap().is_empty(),
            "solved reports are not checked again"
        );
        assert!(feedback.update_status(ID, "closed", None, None, 1).is_err());

        feedback.mark_removed(COMMENT).unwrap();
        assert_eq!(feedback.get(COMMENT).unwrap().unwrap().status, "removed");
        assert!(feedback.delete(ID).unwrap());
        assert!(feedback.get(ID).unwrap().is_none());
    }
}
