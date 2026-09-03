use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use phorminx_persistence::{HistoryTextVariant, Persistence};
use phorminx_ui::HistoryVariant;

const HISTORY_READER_BUSY_TIMEOUT: Duration = Duration::from_millis(250);
const HISTORY_READER_SHUTDOWN_WAIT: Duration = Duration::from_millis(350);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum HistoryLoadIntent {
    Detail,
    Copy,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct HistoryLoadKey {
    pub id: i64,
    pub variant: HistoryVariant,
    pub intent: HistoryLoadIntent,
}

#[derive(Debug)]
pub(crate) struct HistoryLoadResult {
    pub key: HistoryLoadKey,
    pub value: Result<Option<String>, String>,
}

#[derive(Clone, Copy, Debug)]
struct Request {
    ticket: u64,
    key: HistoryLoadKey,
}

#[derive(Debug, Default)]
struct MailboxState {
    generation: u64,
    pending: Option<Request>,
    result: Option<(u64, HistoryLoadResult)>,
    shutdown: bool,
}

#[derive(Debug, Default)]
struct Mailbox {
    state: Mutex<MailboxState>,
    ready: Condvar,
}

/// A latest-request-wins, single-worker reader for exact history text.
///
/// Both request and result mailboxes contain at most one item. Replacing or
/// invalidating a request advances the generation, so a slow prior query can
/// never publish text to a later detail view or clipboard intent.
pub(crate) struct HistoryLoader {
    database_path: PathBuf,
    mailbox: Arc<Mailbox>,
    thread: Option<JoinHandle<()>>,
    done: Option<mpsc::Receiver<()>>,
    current: Option<HistoryLoadKey>,
}

impl HistoryLoader {
    /// Creates a dormant reader. No connection or thread exists until History
    /// asks for exact text, so application startup and retention-disabled use
    /// pay no reader cost and cannot be failed by this optional feature.
    pub(crate) fn new(database_path: PathBuf) -> Self {
        Self {
            database_path,
            mailbox: Arc::new(Mailbox::default()),
            thread: None,
            done: None,
            current: None,
        }
    }

    fn start_worker(&mut self) -> Result<(), String> {
        if self
            .thread
            .as_ref()
            .is_some_and(std::thread::JoinHandle::is_finished)
        {
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
            self.done = None;
        }
        if self.thread.is_some() {
            return Ok(());
        }
        let database_path = self.database_path.clone();
        let worker_mailbox = Arc::clone(&self.mailbox);
        let (done_tx, done_rx) = mpsc::sync_channel(1);
        let thread = thread::Builder::new()
            .name("phorminx-history-loader".to_owned())
            .spawn(move || {
                let _done = WorkerDone(done_tx);
                let persistence = match Persistence::open_with_busy_timeout(
                    database_path,
                    HISTORY_READER_BUSY_TIMEOUT,
                ) {
                    Ok(persistence) => persistence,
                    Err(error) => {
                        publish_startup_error(&worker_mailbox, error.to_string());
                        return;
                    }
                };
                worker_loop(&persistence, &worker_mailbox);
            })
            .map_err(|_| "the background history reader could not start".to_owned())?;
        self.thread = Some(thread);
        self.done = Some(done_rx);
        Ok(())
    }

    pub(crate) fn request(&mut self, key: HistoryLoadKey) -> Result<(), String> {
        {
            let mut state = lock_state(&self.mailbox);
            state.generation = state.generation.wrapping_add(1);
            let ticket = state.generation;
            state.pending = Some(Request { ticket, key });
            state.result = None;
            state.shutdown = false;
        }
        self.current = Some(key);
        if let Err(error) = self.start_worker() {
            self.invalidate();
            return Err(error);
        }
        self.mailbox.ready.notify_one();
        Ok(())
    }

    pub(crate) const fn has_current(&self) -> bool {
        self.current.is_some()
    }

    pub(crate) fn invalidate(&mut self) {
        let mut state = lock_state(&self.mailbox);
        state.generation = state.generation.wrapping_add(1);
        state.pending = None;
        state.result = None;
        self.current = None;
    }

    pub(crate) fn take_result(&mut self) -> Option<HistoryLoadResult> {
        let mut state = lock_state(&self.mailbox);
        let (ticket, result) = state.result.take()?;
        if ticket != state.generation || self.current != Some(result.key) {
            return None;
        }
        self.current = None;
        Some(result)
    }

    fn stop(&mut self) {
        {
            let mut state = lock_state(&self.mailbox);
            state.generation = state.generation.wrapping_add(1);
            state.pending = None;
            state.result = None;
            state.shutdown = true;
            self.current = None;
            self.mailbox.ready.notify_one();
        }
        let completed = self
            .done
            .as_ref()
            .is_none_or(|done| done.recv_timeout(HISTORY_READER_SHUTDOWN_WAIT).is_ok());
        if let Some(thread) = self.thread.take()
            && completed
        {
            let _ = thread.join();
        }
        self.done = None;
    }
}

impl Drop for HistoryLoader {
    fn drop(&mut self) {
        self.stop();
    }
}

struct WorkerDone(mpsc::SyncSender<()>);

impl Drop for WorkerDone {
    fn drop(&mut self) {
        let _ = self.0.try_send(());
    }
}

fn publish_startup_error(mailbox: &Mailbox, error: String) {
    let mut state = lock_state(mailbox);
    let Some(request) = state.pending.take() else {
        return;
    };
    if !state.shutdown && state.generation == request.ticket {
        state.result = Some((
            request.ticket,
            HistoryLoadResult {
                key: request.key,
                value: Err(error),
            },
        ));
    }
}

fn worker_loop(persistence: &Persistence, mailbox: &Mailbox) {
    loop {
        let request = {
            let mut state = lock_state(mailbox);
            while state.pending.is_none() && !state.shutdown {
                state = mailbox
                    .ready
                    .wait(state)
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
            }
            if state.shutdown {
                return;
            }
            state.pending.take().expect("pending request was checked")
        };

        let value = persistence
            .history()
            .text_variant(request.key.id, map_variant(request.key.variant))
            .map_err(|error| error.to_string());
        let mut state = lock_state(mailbox);
        if !state.shutdown && state.generation == request.ticket {
            state.result = Some((
                request.ticket,
                HistoryLoadResult {
                    key: request.key,
                    value,
                },
            ));
        }
    }
}

fn lock_state(mailbox: &Mailbox) -> std::sync::MutexGuard<'_, MailboxState> {
    mailbox
        .state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

const fn map_variant(variant: HistoryVariant) -> HistoryTextVariant {
    match variant {
        HistoryVariant::Output => HistoryTextVariant::SelectedOutput,
        HistoryVariant::Raw => HistoryTextVariant::Raw,
        HistoryVariant::Normalized => HistoryTextVariant::Normalized,
        HistoryVariant::Cleaned => HistoryTextVariant::Cleaned,
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use phorminx_persistence::{DictationDraft, RetentionPolicy, TimingMetadata};

    use super::*;

    fn database_with_history() -> (tempfile::TempDir, PathBuf, i64) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("history.sqlite3");
        let persistence = Persistence::open(&path).unwrap();
        persistence
            .history()
            .set_retention(RetentionPolicy::Indefinite, 1)
            .unwrap();
        let id = persistence
            .history()
            .insert(&DictationDraft {
                created_at_ms: 1,
                raw_text: "raw".into(),
                normalized_text: None,
                cleaned_text: None,
                selected_output: "selected".into(),
                language: None,
                target_executable: None,
                timings: TimingMetadata::default(),
                warnings: Vec::new(),
            })
            .unwrap()
            .unwrap();
        (directory, path, id)
    }

    fn wait_result(loader: &mut HistoryLoader) -> HistoryLoadResult {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if let Some(result) = loader.take_result() {
                return result;
            }
            assert!(Instant::now() < deadline, "history result timed out");
            thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn latest_request_wins_and_invalidation_drops_completed_text() {
        let (_directory, path, id) = database_with_history();
        let mut loader = HistoryLoader::new(path);
        loader
            .request(HistoryLoadKey {
                id,
                variant: HistoryVariant::Raw,
                intent: HistoryLoadIntent::Detail,
            })
            .unwrap();
        loader
            .request(HistoryLoadKey {
                id,
                variant: HistoryVariant::Output,
                intent: HistoryLoadIntent::Copy,
            })
            .unwrap();
        let result = wait_result(&mut loader);
        assert_eq!(result.key.intent, HistoryLoadIntent::Copy);
        assert_eq!(result.value.unwrap(), Some("selected".into()));

        loader
            .request(HistoryLoadKey {
                id,
                variant: HistoryVariant::Output,
                intent: HistoryLoadIntent::Detail,
            })
            .unwrap();
        loader.invalidate();
        thread::sleep(Duration::from_millis(25));
        assert!(loader.take_result().is_none());
    }

    #[test]
    fn dormant_loader_does_not_touch_an_invalid_database_until_requested() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("private-history.sqlite3");
        std::fs::write(&path, "not sqlite").unwrap();
        let mut loader = HistoryLoader::new(path.clone());
        assert!(loader.thread.is_none());

        loader
            .request(HistoryLoadKey {
                id: 1,
                variant: HistoryVariant::Output,
                intent: HistoryLoadIntent::Detail,
            })
            .unwrap();
        let error = wait_result(&mut loader).value.unwrap_err();
        assert!(!error.contains(path.to_string_lossy().as_ref()));
        assert!(!error.contains("not sqlite"));
    }

    #[test]
    fn a_stale_generation_cannot_surface_a_detail_or_copy_result() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("history.sqlite3");
        let mut loader = HistoryLoader::new(path);
        let stale_key = HistoryLoadKey {
            id: 1,
            variant: HistoryVariant::Raw,
            intent: HistoryLoadIntent::Copy,
        };
        let current_key = HistoryLoadKey {
            id: 2,
            variant: HistoryVariant::Output,
            intent: HistoryLoadIntent::Detail,
        };
        loader.current = Some(current_key);
        let mut state = lock_state(&loader.mailbox);
        state.generation = 2;
        state.result = Some((
            1,
            HistoryLoadResult {
                key: stale_key,
                value: Ok(Some("must not escape".into())),
            },
        ));
        drop(state);

        assert!(loader.take_result().is_none());
        assert_eq!(loader.current, Some(current_key));
    }

    #[test]
    fn locked_database_does_not_block_request_or_invalidation() {
        let (_directory, path, id) = database_with_history();
        let lock = rusqlite::Connection::open(&path).unwrap();
        lock.execute_batch("PRAGMA locking_mode=EXCLUSIVE; BEGIN EXCLUSIVE;")
            .unwrap();
        let mut loader = HistoryLoader::new(path);
        let started = Instant::now();
        loader
            .request(HistoryLoadKey {
                id,
                variant: HistoryVariant::Output,
                intent: HistoryLoadIntent::Copy,
            })
            .unwrap();
        loader.invalidate();
        assert!(started.elapsed() < Duration::from_millis(50));
        let shutdown_started = Instant::now();
        drop(loader);
        assert!(shutdown_started.elapsed() < Duration::from_secs(1));
        drop(lock);
    }

    #[test]
    fn deletion_before_copy_completion_returns_no_clipboard_payload() {
        let (_directory, path, id) = database_with_history();
        let deleting = Persistence::open(&path).unwrap();
        deleting.history().clear().unwrap();
        let mut loader = HistoryLoader::new(path);
        loader
            .request(HistoryLoadKey {
                id,
                variant: HistoryVariant::Output,
                intent: HistoryLoadIntent::Copy,
            })
            .unwrap();
        assert_eq!(wait_result(&mut loader).value.unwrap(), None);
    }
}
