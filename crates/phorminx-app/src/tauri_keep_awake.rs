//! Tauri command boundary for Keep awake: one switch that holds a Windows
//! keep-awake request while it is on. Power settings are never changed; the
//! request ends when the switch goes off or Phorminx closes.

use std::sync::Mutex;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use phorminx_windows::KeepAwake;
use serde::Serialize;
use tauri::State;

#[derive(Default)]
pub struct KeepAwakeState {
    session: Mutex<Option<KeepAwakeSession>>,
}

struct KeepAwakeSession {
    // Held only for its Drop, which hands the request back to Windows.
    _request: KeepAwake,
    started: Instant,
    started_at_ms: i64,
    keep_screen_on: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KeepAwakeView {
    active: bool,
    keep_screen_on: bool,
    started_at_ms: Option<i64>,
    elapsed_ms: u64,
    /// How long the session lasted, when this call turned it off.
    ended_after_ms: Option<u64>,
}

impl KeepAwakeView {
    fn of(session: Option<&KeepAwakeSession>, ended_after_ms: Option<u64>) -> Self {
        Self {
            active: session.is_some(),
            keep_screen_on: session.is_some_and(|session| session.keep_screen_on),
            started_at_ms: session.map(|session| session.started_at_ms),
            elapsed_ms: session.map_or(0, |session| millis(session.started.elapsed().as_millis())),
            ended_after_ms,
        }
    }
}

#[tauri::command]
pub fn keep_awake_snapshot(state: State<'_, KeepAwakeState>) -> Result<KeepAwakeView, String> {
    let session = state.session.lock().map_err(|_| "Keep awake is unavailable.".to_owned())?;
    Ok(KeepAwakeView::of(session.as_ref(), None))
}

/// Turns Keep awake on or off. Changing the screen choice while it is on
/// renews the request but keeps the running time.
#[tauri::command]
pub fn keep_awake_set(
    active: bool,
    keep_screen_on: bool,
    state: State<'_, KeepAwakeState>,
) -> Result<KeepAwakeView, String> {
    let mut session = state.session.lock().map_err(|_| "Keep awake is unavailable.".to_owned())?;
    if !active {
        let ended = session.take().map(|ended| millis(ended.started.elapsed().as_millis()));
        return Ok(KeepAwakeView::of(None, ended));
    }
    if session.as_ref().is_some_and(|current| current.keep_screen_on == keep_screen_on) {
        return Ok(KeepAwakeView::of(session.as_ref(), None));
    }
    let (started, started_at_ms) = session
        .as_ref()
        .map_or_else(|| (Instant::now(), now_ms()), |current| (current.started, current.started_at_ms));
    // Release the old request first so only one is ever held.
    *session = None;
    let request = KeepAwake::start(keep_screen_on).map_err(|error| error.to_string())?;
    *session = Some(KeepAwakeSession { _request: request, started, started_at_ms, keep_screen_on });
    Ok(KeepAwakeView::of(session.as_ref(), None))
}

fn millis(value: u128) -> u64 {
    value.min(u64::MAX as u128) as u64
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}
