//! Asks phorminx.net, at most once a day per open report, whether the team
//! has solved it, and removes shared comments on request. Each request names
//! one ticket or comment by the random id this computer sent it with; nothing
//! else about the computer or its use is sent.

use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use phorminx_persistence::{FeedbackRecord, Persistence};
use serde::Deserialize;

const BASE: &str = "https://phorminx.net/api/v1";
/// A report is asked about again after this long.
pub const CHECK_INTERVAL: Duration = Duration::from_secs(20 * 60 * 60);

#[derive(Deserialize)]
struct TicketStatus {
    status: String,
    resolution_note: Option<String>,
}

/// A report the team solved since it was last checked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resolved {
    pub id: String,
    pub preview: String,
    pub note: Option<String>,
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as i64)
        .unwrap_or_default()
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(10)))
        .max_redirects(0)
        .https_only(true)
        .http_status_as_error(false)
        .build()
        .new_agent()
}

/// What the service says about one ticket: `Ok(None)` when it no longer
/// knows it (tickets are kept for a year).
fn fetch_status(id: &str) -> Result<Option<TicketStatus>, String> {
    let mut response = agent()
        .get(format!("{BASE}/feedback/{id}"))
        .call()
        .map_err(|_| "phorminx.net could not be reached.".to_owned())?;
    match response.status().as_u16() {
        200 => {
            let text = response
                .body_mut()
                .with_config()
                .limit(4096)
                .read_to_string()
                .map_err(|_| "The ticket status could not be read.".to_owned())?;
            serde_json::from_str(&text)
                .map(Some)
                .map_err(|_| "The ticket status could not be read.".into())
        }
        404 => Ok(None),
        _ => Err("phorminx.net did not answer the status check.".into()),
    }
}

fn preview(text: &str) -> String {
    let line = text
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or_default()
        .trim();
    if line.chars().count() > 80 {
        format!("{}…", line.chars().take(79).collect::<String>())
    } else {
        line.to_owned()
    }
}

/// Checks reports that are due (or every open one when `all`) and returns
/// the ones newly solved, which the caller announces once.
pub fn check(database: &Path, all: bool) -> Result<Vec<Resolved>, String> {
    let store = Persistence::open(database)
        .map_err(|_| "Your feedback history could not be opened.".to_owned())?;
    let now = now_ms();
    let before = if all {
        i64::MAX
    } else {
        now - CHECK_INTERVAL.as_millis() as i64
    };
    let due = store
        .feedback()
        .due_for_check(before)
        .map_err(|error| error.to_string())?;
    let mut resolved = Vec::new();
    for id in due {
        match fetch_status(&id)? {
            Some(status) if status.status == "solved" || status.status == "open" => {
                let newly = store
                    .feedback()
                    .update_status(
                        &id,
                        &status.status,
                        status.resolution_note.as_deref(),
                        (status.status == "solved").then_some(now),
                        now,
                    )
                    .map_err(|error| error.to_string())?;
                if newly {
                    let record = store
                        .feedback()
                        .get(&id)
                        .map_err(|error| error.to_string())?;
                    resolved.push(Resolved {
                        id: id.clone(),
                        preview: record
                            .map(|record| preview(&record.text))
                            .unwrap_or_default(),
                        note: status.resolution_note,
                    });
                }
            }
            _ => store
                .feedback()
                .mark_checked(&id, now)
                .map_err(|error| error.to_string())?,
        }
    }
    Ok(resolved)
}

pub fn mark_notified(database: &Path, id: &str) {
    if let Ok(store) = Persistence::open(database) {
        let _ = store.feedback().mark_notified(id);
    }
}

pub fn history(database: &Path) -> Result<Vec<FeedbackRecord>, String> {
    Persistence::open(database)
        .and_then(|store| store.feedback().list(200))
        .map_err(|_| "Your feedback history could not be read.".into())
}

/// Takes a shared comment off phorminx.net for good.
pub fn remove_comment(database: &Path, id: &str) -> Result<(), String> {
    let store = Persistence::open(database)
        .map_err(|_| "Your feedback history could not be opened.".to_owned())?;
    let record = store
        .feedback()
        .get(id)
        .map_err(|error| error.to_string())?
        .filter(|record| record.kind == "comment")
        .ok_or("That comment is not in your feedback history.")?;
    let response = agent()
        .delete(format!("{BASE}/comments/{}", record.id))
        .call()
        .map_err(|_| "phorminx.net could not be reached. Try again later.".to_owned())?;
    // 404: already gone (rejected and pruned, or removed before).
    if !matches!(response.status().as_u16(), 200 | 404) {
        return Err("phorminx.net did not remove the comment. Try again later.".into());
    }
    store
        .feedback()
        .mark_removed(&record.id)
        .map_err(|error| error.to_string())
}

/// Forgets one item on this computer; it stays on phorminx.net.
pub fn forget(database: &Path, id: &str) -> Result<(), String> {
    Persistence::open(database)
        .and_then(|store| store.feedback().delete(id))
        .map(|_| ())
        .map_err(|_| "That item could not be removed from your history.".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn previews_are_one_short_line() {
        assert_eq!(preview("\n  First line  \nsecond"), "First line");
        assert_eq!(preview(&"x".repeat(100)).chars().count(), 80);
        assert_eq!(preview(""), "");
    }

    #[test]
    fn nothing_due_means_no_requests() {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("phorminx.db");
        let store = Persistence::open(&database).unwrap();
        store
            .feedback()
            .record(
                "0f8c7a52-3b1e-4d2a-9c4e-1a2b3c4d5e6f",
                "report",
                "x",
                now_ms(),
            )
            .unwrap();
        store
            .feedback()
            .update_status(
                "0f8c7a52-3b1e-4d2a-9c4e-1a2b3c4d5e6f",
                "open",
                None,
                None,
                now_ms(),
            )
            .unwrap();
        drop(store);
        // Checked just now, so a scheduled check sends nothing and finds nothing.
        assert!(check(&database, false).unwrap().is_empty());
        assert_eq!(history(&database).unwrap().len(), 1);
        forget(&database, "0f8c7a52-3b1e-4d2a-9c4e-1a2b3c4d5e6f").unwrap();
        assert!(history(&database).unwrap().is_empty());
        assert!(remove_comment(&database, "0f8c7a52-3b1e-4d2a-9c4e-1a2b3c4d5e6f").is_err());
    }
}
