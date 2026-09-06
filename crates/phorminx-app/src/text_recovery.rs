//! A bounded mailbox coalesces coherent snapshots away from capture and inference.
//! Text payloads intentionally have no Debug implementation.
use phorminx_persistence::{MAX_TERMINAL_TEXT_BYTES, Persistence};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{Arc, Condvar, Mutex},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

pub fn process_prefix() -> String {
    static PREFIX: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    PREFIX
        .get_or_init(|| {
            format!(
                "{}-{}-",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos()
            )
        })
        .clone()
}
enum Change {
    Save { epoch: i64, text: String },
    Discard,
}
#[derive(Default)]
struct Mailbox {
    changes: BTreeMap<u64, Change>,
    eligibility: Option<(u64, Option<i64>)>,
    retired_through: Option<u64>,
    stopping: bool,
}
#[derive(Clone)]
pub struct RecoveryJournal {
    shared: Arc<(Mutex<Mailbox>, Condvar)>,
    path: PathBuf,
    prefix: String,
    thread: Arc<Mutex<Option<thread::JoinHandle<()>>>>,
}
pub struct CheckpointClock {
    last: Instant,
    id: Option<u64>,
}
impl Default for CheckpointClock {
    fn default() -> Self {
        Self {
            last: Instant::now() - Duration::from_secs(3),
            id: None,
        }
    }
}
impl RecoveryJournal {
    pub fn start(path: PathBuf) -> std::io::Result<Self> {
        static INITIALIZED: std::sync::OnceLock<Mutex<std::collections::HashSet<PathBuf>>> =
            std::sync::OnceLock::new();
        let mut initialized = INITIALIZED
            .get_or_init(Default::default)
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if !initialized.contains(&path) {
            let db = Persistence::open_with_busy_timeout(&path, Duration::from_millis(250))
                .map_err(|_| std::io::Error::other("text recovery startup failed"))?;
            db.recovery()
                .begin_process()
                .map_err(|_| std::io::Error::other("text recovery startup failed"))?;
            initialized.insert(path.clone());
        }
        drop(initialized);
        static RUNTIME: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let prefix = format!(
            "{}{}-",
            process_prefix(),
            RUNTIME.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        );
        let journal = Self {
            shared: Arc::new((Mutex::new(Mailbox::default()), Condvar::new())),
            path,
            prefix,
            thread: Arc::new(Mutex::new(None)),
        };
        let worker = journal.clone();
        let handle = thread::Builder::new()
            .name("phorminx-text-recovery".into())
            .spawn(move || worker.run())?;
        *journal.thread.lock().unwrap_or_else(|e| e.into_inner()) = Some(handle);
        Ok(journal)
    }
    pub fn begin(&self, id: u64, epoch: Option<i64>) {
        if let Ok(mut state) = self.shared.0.lock() {
            state.eligibility = Some((id, epoch));
        }
    }
    /// Caller is the recognition worker, never the capture callback. At most one
    /// 16 MiB snapshot per session can wait; at most four sessions can be queued.
    pub fn checkpoint(&self, clock: &mut CheckpointClock, id: u64, text: &str, force: bool) {
        if clock.id != Some(id) {
            clock.id = Some(id);
            clock.last = Instant::now() - Duration::from_secs(3);
        }
        if text.is_empty()
            || text.len() > MAX_TERMINAL_TEXT_BYTES
            || (!force && clock.last.elapsed() < Duration::from_secs(2))
        {
            return;
        }
        let (lock, wake) = &*self.shared;
        if let Ok(mut state) = lock.lock() {
            let Some((active, Some(epoch))) = state.eligibility else {
                return;
            };
            if active != id {
                return;
            }
            if state.retired_through.is_some_and(|last| id <= last) || state.stopping {
                return;
            }
            if state.changes.len() >= 4 && !state.changes.contains_key(&id) {
                return;
            }
            state.changes.insert(
                id,
                Change::Save {
                    epoch,
                    text: text.to_owned(),
                },
            );
            clock.last = Instant::now();
            wake.notify_one();
        }
    }
    pub fn discard(&self, id: u64) {
        let (lock, wake) = &*self.shared;
        if let Ok(mut state) = lock.lock() {
            if state.stopping {
                return;
            }
            // Terminal controls never disappear. Only an exceptional blocked
            // storage writer can backpressure this control path; audio callbacks
            // never call it. Pending transcript memory remains strictly bounded.
            while state.changes.len() >= 4 && !state.changes.contains_key(&id) && !state.stopping {
                state = match wake.wait(state) {
                    Ok(state) => state,
                    Err(_) => return,
                };
            }
            if state.stopping {
                return;
            }
            state.retired_through = Some(state.retired_through.map_or(id, |old| old.max(id)));
            state.changes.insert(id, Change::Discard);
            wake.notify_one();
        }
        failed_ids()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&format!("{}{id}", self.prefix));
    }
    pub fn interrupted(&self, id: u64) {
        if self
            .shared
            .0
            .lock()
            .is_ok_and(|state| state.retired_through.is_some_and(|last| id <= last))
        {
            return;
        }
        // Kept in its own key until the writer has completed any preceding save.
        // Visibility uses a process-local failed-ID set, so no snapshot is lost.
        failed_ids()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(format!("{}{id}", self.prefix));
    }
    pub fn stop(&self) {
        let active = self
            .shared
            .0
            .lock()
            .ok()
            .and_then(|state| state.eligibility.map(|(id, _)| id));
        if let Some(id) = active
            && !is_interrupted(&format!("{}{id}", self.prefix))
        {
            self.discard(id);
        }
        let (lock, wake) = &*self.shared;
        if let Ok(mut state) = lock.lock() {
            state.stopping = true;
            wake.notify_one();
        }
        if let Ok(mut handle) = self.thread.lock()
            && let Some(handle) = handle.take()
        {
            let _ = handle.join();
        }
    }
    fn run(self) {
        let Ok(db) = Persistence::open_with_busy_timeout(&self.path, Duration::from_millis(250))
        else {
            if let Ok(mut state) = self.shared.0.lock() {
                state.stopping = true;
                self.shared.1.notify_all();
            }
            eprintln!("event=text_recovery_unavailable");
            return;
        };
        loop {
            let (lock, wake) = &*self.shared;
            let Ok(mut state) = lock.lock() else {
                return;
            };
            while state.changes.is_empty() && !state.stopping {
                state = match wake.wait(state) {
                    Ok(state) => state,
                    Err(_) => return,
                };
            }
            if state.changes.is_empty() && state.stopping {
                break;
            }
            let changes = std::mem::take(&mut state.changes);
            wake.notify_all();
            drop(state);
            for (id, change) in changes {
                let key = format!("{}{id}", self.prefix);
                let result = match change {
                    Change::Save { epoch, text } => {
                        db.recovery().save(&key, epoch, now_ms(), &text).map(|_| ())
                    }
                    Change::Discard => db.recovery().discard(&key),
                };
                if result.is_err() {
                    eprintln!("event=text_recovery_write_failed");
                }
            }
        }
    }
}
fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}
fn failed_ids() -> &'static Mutex<std::collections::HashSet<String>> {
    static FAILED: std::sync::OnceLock<Mutex<std::collections::HashSet<String>>> =
        std::sync::OnceLock::new();
    FAILED.get_or_init(Default::default)
}
pub fn is_interrupted(session: &str) -> bool {
    !session.starts_with(&process_prefix())
        || failed_ids()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains(session)
}
pub fn forget_interrupted(session: &str) {
    failed_ids()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(session);
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use phorminx_persistence::RetentionPolicy;
    fn fixture() -> (tempfile::TempDir, Persistence, RecoveryJournal) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("recovery.db");
        let db = Persistence::open(&path).unwrap();
        db.history()
            .set_retention(RetentionPolicy::Indefinite, now_ms())
            .unwrap();
        let journal = RecoveryJournal::start(path).unwrap();
        (dir, db, journal)
    }
    #[test]
    fn interrupted_text_is_coherent_after_shutdown_and_reopen() {
        let (_dir, db, journal) = fixture();
        let mut clock = CheckpointClock::default();
        journal.begin(1, Some(db.recovery().epoch().unwrap()));
        journal.checkpoint(&mut clock, 1, "an uncertain suffix", true);
        journal.checkpoint(&mut clock, 1, "a corrected complete sentence", true);
        journal.interrupted(1);
        journal.stop();
        let key = format!("{}1", journal.prefix);
        assert_eq!(
            db.recovery().text(&key).unwrap().as_deref(),
            Some("a corrected complete sentence")
        );
        assert!(is_interrupted(&key));
        assert!(journal.thread.lock().unwrap().is_none());
    }
    #[test]
    fn delivery_discard_wins_over_pending_and_late_snapshots() {
        let (_dir, db, journal) = fixture();
        let mut clock = CheckpointClock::default();
        journal.begin(1, Some(db.recovery().epoch().unwrap()));
        journal.checkpoint(&mut clock, 1, "private sentence", true);
        journal.discard(1);
        journal.checkpoint(&mut clock, 1, "stale sentence", true);
        journal.interrupted(1);
        journal.stop();
        assert!(db.recovery().list().unwrap().is_empty());
    }
    #[test]
    fn activation_epoch_survives_clear_before_first_recognition() {
        let (_dir, db, journal) = fixture();
        let mut clock = CheckpointClock::default();
        journal.begin(1, Some(db.recovery().epoch().unwrap()));
        db.history().clear().unwrap();
        journal.checkpoint(&mut clock, 1, "pre-clear private speech", true);
        journal.interrupted(1);
        journal.stop();
        assert!(db.recovery().list().unwrap().is_empty());
    }
    #[test]
    fn history_off_at_activation_cannot_recover_pre_enable_speech() {
        let (_dir, db, journal) = fixture();
        let mut clock = CheckpointClock::default();
        journal.begin(1, None);
        journal.checkpoint(&mut clock, 1, "not retained", true);
        journal.interrupted(1);
        journal.stop();
        assert!(db.recovery().list().unwrap().is_empty());
    }
    #[test]
    fn quit_discards_only_active_session_and_keeps_prior_failure() {
        let (_dir, db, journal) = fixture();
        let mut clock = CheckpointClock::default();
        let epoch = db.recovery().epoch().unwrap();
        journal.begin(1, Some(epoch));
        journal.checkpoint(&mut clock, 1, "failed earlier", true);
        journal.interrupted(1);
        journal.begin(2, Some(epoch));
        journal.checkpoint(&mut clock, 2, "active on quit", true);
        journal.stop();
        assert_eq!(db.recovery().list().unwrap().len(), 1);
        assert_eq!(
            db.recovery()
                .text(&format!("{}1", journal.prefix))
                .unwrap()
                .as_deref(),
            Some("failed earlier")
        );
    }
    #[test]
    fn runtime_reload_gets_unique_session_identity() {
        let (_dir, db, journal) = fixture();
        let mut clock = CheckpointClock::default();
        journal.begin(1, Some(db.recovery().epoch().unwrap()));
        journal.checkpoint(&mut clock, 1, "prior runtime", true);
        journal.interrupted(1);
        journal.stop();
        let next = RecoveryJournal::start(journal.path.clone()).unwrap();
        assert_ne!(journal.prefix, next.prefix);
        next.stop();
        assert_eq!(db.recovery().list().unwrap().len(), 1);
    }
    #[test]
    fn snapshot_mailbox_coalesces_hours_and_is_bounded_without_a_writer() {
        let journal = RecoveryJournal {
            shared: Arc::new((Mutex::new(Mailbox::default()), Condvar::new())),
            path: PathBuf::new(),
            prefix: "test-mailbox-".into(),
            thread: Arc::new(Mutex::new(None)),
        };
        let mut clock = CheckpointClock::default();
        for id in 1..=5 {
            journal.begin(id, Some(0));
            for _ in 0..14_400 {
                journal.checkpoint(&mut clock, id, "current coherent snapshot", true);
            }
        }
        let state = journal.shared.0.lock().unwrap();
        assert_eq!(state.changes.len(), 4);
        assert!(state.changes.values().all(
            |change| matches!(change,Change::Save {text,..} if text=="current coherent snapshot")
        ));
    }
}
