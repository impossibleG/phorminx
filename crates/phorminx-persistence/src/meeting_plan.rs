//! Meeting plans: a meeting scheduled ahead (or opened live) with its own
//! instructions, short "important items" and a briefing. Materials live in a
//! linked investigation, so text, screenshots (OCR) and voice notes reuse that
//! storage. Additive tables only; older versions ignore them.
use rusqlite::{Connection, OptionalExtension, Row, params};

use crate::{PersistenceError, Result};

pub const MAX_PLAN_TITLE_CHARS: usize = 200;
pub const MAX_PLAN_TEXT_BYTES: usize = 32 * 1024;
pub const MAX_PLAN_ITEM_CHARS: usize = 500;
pub const MAX_PLAN_ITEMS: usize = 20;

pub(crate) const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS meeting_plans (
    id INTEGER PRIMARY KEY,
    title TEXT NOT NULL,
    scheduled_at_ms INTEGER,
    reminder_minutes INTEGER CHECK(reminder_minutes IS NULL OR reminder_minutes BETWEEN 0 AND 1440),
    reminded INTEGER NOT NULL DEFAULT 0,
    instructions TEXT NOT NULL DEFAULT '',
    briefing TEXT NOT NULL DEFAULT '',
    investigation_id INTEGER REFERENCES investigations(id) ON DELETE SET NULL,
    session_id INTEGER REFERENCES meeting_sessions(id) ON DELETE SET NULL,
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS meeting_plans_schedule_idx ON meeting_plans(scheduled_at_ms);
CREATE INDEX IF NOT EXISTS meeting_plans_session_idx ON meeting_plans(session_id);
CREATE TABLE IF NOT EXISTS meeting_plan_items (
    id INTEGER PRIMARY KEY,
    plan_id INTEGER NOT NULL REFERENCES meeting_plans(id) ON DELETE CASCADE,
    position INTEGER NOT NULL,
    text TEXT NOT NULL,
    done INTEGER NOT NULL DEFAULT 0,
    created_at_ms INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS meeting_plan_items_plan_idx ON meeting_plan_items(plan_id, position);
"#;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MeetingPlanItem {
    pub id: i64,
    pub text: String,
    pub done: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MeetingPlanRecord {
    pub id: i64,
    pub title: String,
    pub scheduled_at_ms: Option<i64>,
    pub reminder_minutes: Option<u32>,
    pub reminded: bool,
    pub instructions: String,
    pub briefing: String,
    pub investigation_id: Option<i64>,
    pub session_id: Option<i64>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub items: Vec<MeetingPlanItem>,
}

/// The editable part of a plan. Items are replaced as a whole list.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MeetingPlanDraft {
    pub title: String,
    pub scheduled_at_ms: Option<i64>,
    pub reminder_minutes: Option<u32>,
    pub instructions: String,
    pub briefing: String,
    pub items: Vec<(String, bool)>,
}

const COLUMNS: &str = "id,title,scheduled_at_ms,reminder_minutes,reminded,instructions,briefing,investigation_id,session_id,created_at_ms,updated_at_ms";

fn record(row: &Row<'_>) -> rusqlite::Result<MeetingPlanRecord> {
    Ok(MeetingPlanRecord {
        id: row.get(0)?,
        title: row.get(1)?,
        scheduled_at_ms: row.get(2)?,
        reminder_minutes: row.get(3)?,
        reminded: row.get(4)?,
        instructions: row.get(5)?,
        briefing: row.get(6)?,
        investigation_id: row.get(7)?,
        session_id: row.get(8)?,
        created_at_ms: row.get(9)?,
        updated_at_ms: row.get(10)?,
        items: Vec::new(),
    })
}

pub struct MeetingPlanRepository<'a> {
    connection: &'a Connection,
}

impl<'a> MeetingPlanRepository<'a> {
    pub(crate) fn new(connection: &'a Connection) -> Self {
        Self { connection }
    }

    pub fn create(&self, draft: &MeetingPlanDraft, now_ms: i64) -> Result<i64> {
        validate(draft)?;
        let transaction = self.connection.unchecked_transaction()?;
        transaction.execute(
            "INSERT INTO meeting_plans(title,scheduled_at_ms,reminder_minutes,instructions,briefing,created_at_ms,updated_at_ms) \
             VALUES (?1,?2,?3,?4,?5,?6,?6)",
            params![
                draft.title.trim(),
                draft.scheduled_at_ms,
                draft.reminder_minutes,
                draft.instructions,
                draft.briefing,
                now_ms
            ],
        )?;
        let id = transaction.last_insert_rowid();
        write_items(&transaction, id, &draft.items, now_ms)?;
        transaction.commit()?;
        Ok(id)
    }

    /// Saves every editable field. A changed time re-arms the reminder.
    pub fn update(&self, id: i64, draft: &MeetingPlanDraft, now_ms: i64) -> Result<bool> {
        validate(draft)?;
        let transaction = self.connection.unchecked_transaction()?;
        let changed = transaction.execute(
            "UPDATE meeting_plans SET title=?2,\
             reminded=CASE WHEN scheduled_at_ms IS ?3 AND reminder_minutes IS ?4 THEN reminded ELSE 0 END,\
             scheduled_at_ms=?3,reminder_minutes=?4,instructions=?5,briefing=?6,updated_at_ms=?7 WHERE id=?1",
            params![
                id,
                draft.title.trim(),
                draft.scheduled_at_ms,
                draft.reminder_minutes,
                draft.instructions,
                draft.briefing,
                now_ms
            ],
        )? > 0;
        if changed {
            transaction.execute("DELETE FROM meeting_plan_items WHERE plan_id=?1", [id])?;
            write_items(&transaction, id, &draft.items, now_ms)?;
        }
        transaction.commit()?;
        Ok(changed)
    }

    /// Replaces only the important items, for edits made during a meeting.
    pub fn set_items(&self, id: i64, items: &[(String, bool)], now_ms: i64) -> Result<bool> {
        validate_items(items)?;
        let transaction = self.connection.unchecked_transaction()?;
        let changed = transaction.execute(
            "UPDATE meeting_plans SET updated_at_ms=?2 WHERE id=?1",
            params![id, now_ms],
        )? > 0;
        if changed {
            transaction.execute("DELETE FROM meeting_plan_items WHERE plan_id=?1", [id])?;
            write_items(&transaction, id, items, now_ms)?;
        }
        transaction.commit()?;
        Ok(changed)
    }

    pub fn set_briefing(&self, id: i64, briefing: &str, now_ms: i64) -> Result<bool> {
        bounded("meeting_plan_briefing", briefing, MAX_PLAN_TEXT_BYTES)?;
        Ok(self.connection.execute(
            "UPDATE meeting_plans SET briefing=?2,updated_at_ms=?3 WHERE id=?1",
            params![id, briefing, now_ms],
        )? > 0)
    }

    pub fn link_investigation(&self, id: i64, investigation: i64) -> Result<bool> {
        Ok(self.connection.execute(
            "UPDATE meeting_plans SET investigation_id=?2 WHERE id=?1",
            params![id, investigation],
        )? > 0)
    }

    pub fn link_session(&self, id: i64, session: i64) -> Result<bool> {
        Ok(self.connection.execute(
            "UPDATE meeting_plans SET session_id=?2 WHERE id=?1",
            params![id, session],
        )? > 0)
    }

    pub fn mark_reminded(&self, id: i64) -> Result<bool> {
        Ok(self
            .connection
            .execute("UPDATE meeting_plans SET reminded=1 WHERE id=?1", [id])?
            > 0)
    }

    pub fn delete(&self, id: i64) -> Result<bool> {
        Ok(self
            .connection
            .execute("DELETE FROM meeting_plans WHERE id=?1", [id])?
            > 0)
    }

    pub fn get(&self, id: i64) -> Result<Option<MeetingPlanRecord>> {
        let plan = self
            .connection
            .query_row(
                &format!("SELECT {COLUMNS} FROM meeting_plans WHERE id=?1"),
                [id],
                record,
            )
            .optional()?;
        plan.map(|plan| self.with_items(plan)).transpose()
    }

    pub fn for_session(&self, session: i64) -> Result<Option<MeetingPlanRecord>> {
        let plan = self
            .connection
            .query_row(
                &format!(
                    "SELECT {COLUMNS} FROM meeting_plans WHERE session_id=?1 ORDER BY id DESC LIMIT 1"
                ),
                [session],
                record,
            )
            .optional()?;
        plan.map(|plan| self.with_items(plan)).transpose()
    }

    /// Plans not yet started, soonest first; unscheduled drafts last.
    pub fn upcoming(&self, limit: usize) -> Result<Vec<MeetingPlanRecord>> {
        let mut statement = self.connection.prepare(&format!(
            "SELECT {COLUMNS} FROM meeting_plans WHERE session_id IS NULL \
             ORDER BY scheduled_at_ms IS NULL, scheduled_at_ms, id LIMIT ?1"
        ))?;
        let plans = statement
            .query_map([limit.min(500) as i64], record)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        plans
            .into_iter()
            .map(|plan| self.with_items(plan))
            .collect()
    }

    /// Scheduled plans whose reminder time has come and that were not reminded.
    pub fn due_reminders(&self, now_ms: i64) -> Result<Vec<MeetingPlanRecord>> {
        let mut statement = self.connection.prepare(&format!(
            "SELECT {COLUMNS} FROM meeting_plans WHERE session_id IS NULL AND reminded=0 \
             AND scheduled_at_ms IS NOT NULL AND reminder_minutes IS NOT NULL \
             AND scheduled_at_ms - reminder_minutes*60000 <= ?1 AND scheduled_at_ms + 3600000 > ?1 \
             ORDER BY scheduled_at_ms LIMIT 5"
        ))?;
        let plans = statement
            .query_map([now_ms], record)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        plans
            .into_iter()
            .map(|plan| self.with_items(plan))
            .collect()
    }

    fn with_items(&self, mut plan: MeetingPlanRecord) -> Result<MeetingPlanRecord> {
        let mut statement = self.connection.prepare(
            "SELECT id,text,done FROM meeting_plan_items WHERE plan_id=?1 ORDER BY position,id",
        )?;
        plan.items = statement
            .query_map([plan.id], |row| {
                Ok(MeetingPlanItem {
                    id: row.get(0)?,
                    text: row.get(1)?,
                    done: row.get(2)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(plan)
    }
}

fn write_items(
    connection: &Connection,
    plan: i64,
    items: &[(String, bool)],
    now_ms: i64,
) -> Result<()> {
    for (position, (text, done)) in items.iter().enumerate() {
        connection.execute(
            "INSERT INTO meeting_plan_items(plan_id,position,text,done,created_at_ms) VALUES (?1,?2,?3,?4,?5)",
            params![plan, position as i64, text.trim(), done, now_ms],
        )?;
    }
    Ok(())
}

fn validate(draft: &MeetingPlanDraft) -> Result<()> {
    let title = draft.title.trim();
    if title.is_empty() || title.chars().count() > MAX_PLAN_TITLE_CHARS {
        return Err(invalid("meeting_plan_title", "must be 1 to 200 characters"));
    }
    if draft.reminder_minutes.is_some_and(|minutes| minutes > 1440) {
        return Err(invalid("meeting_plan_reminder", "must be at most a day"));
    }
    bounded(
        "meeting_plan_instructions",
        &draft.instructions,
        MAX_PLAN_TEXT_BYTES,
    )?;
    bounded(
        "meeting_plan_briefing",
        &draft.briefing,
        MAX_PLAN_TEXT_BYTES,
    )?;
    validate_items(&draft.items)
}

fn validate_items(items: &[(String, bool)]) -> Result<()> {
    if items.len() > MAX_PLAN_ITEMS {
        return Err(PersistenceError::CollectionLimitExceeded {
            field: "meeting_plan_items",
            max_items: MAX_PLAN_ITEMS,
            actual_items: items.len(),
        });
    }
    if items.iter().any(|(text, _)| {
        text.trim().is_empty()
            || text.chars().count() > MAX_PLAN_ITEM_CHARS
            || text.chars().any(|c| c.is_control() && c != '\n')
    }) {
        return Err(invalid("meeting_plan_item", "must be 1 to 500 characters"));
    }
    Ok(())
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
