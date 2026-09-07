//! Low-priority local search work, isolated from recording and paste delivery.
//! There is one worker, one pending query, and at most one local embedding request.
use phorminx_ollama::{
    CancellationToken, ClientError, ClientTimeouts, KeepAlive, ModelName, OllamaClient,
    OllamaEndpoint,
};
use phorminx_persistence::{
    EmbeddedPassage, LibraryIndexStats, LibraryPassage, LibrarySearchHit, Persistence,
    PersistenceInterrupt,
};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LibrarySearchConfig {
    pub model: Option<String>,
    pub model_identity: Option<String>,
    pub history_enabled: bool,
}
pub fn embedding_identity(model: &str, digest: &str) -> String {
    format!("{model}@{digest}:passage-v1")
}
#[derive(Clone, Debug)]
pub struct LibrarySearchUpdate {
    /// Zero is a background index status, never a replacement for search results.
    pub generation: u64,
    pub query: String,
    pub hits: Vec<LibrarySearchHit>,
    pub stats: LibraryIndexStats,
    pub status: String,
    pub failed: bool,
    pub semantic_available: bool,
}
#[derive(Clone)]
struct Query {
    generation: u64,
    text: String,
    semantic: bool,
}
#[derive(Default)]
struct Mailbox {
    config: LibrarySearchConfig,
    revision: u64,
    query: Option<Query>,
    rebuild: bool,
    refresh: bool,
    cancel: CancellationToken,
    result: Option<LibrarySearchUpdate>,
    progress: Option<LibrarySearchUpdate>,
}
struct Shared {
    mail: Mutex<Mailbox>,
    wake: Condvar,
    paused: AtomicBool,
    stopped: AtomicBool,
    invalidated: AtomicBool,
    interrupt: Mutex<Option<PersistenceInterrupt>>,
}
#[derive(Clone)]
pub struct LibrarySearchHandle {
    shared: Arc<Shared>,
}
pub struct LibrarySearchWorker {
    handle: LibrarySearchHandle,
}
impl LibrarySearchHandle {
    /// One-shot notice that retained source text changed or expired. The UI
    /// should discard displayed hits and resubmit its current query.
    pub fn take_invalidated(&self) -> bool {
        self.shared.invalidated.swap(false, Ordering::AcqRel)
    }
    /// Returns true when an effective change cancels in-flight work. A caller
    /// displaying a query should resubmit that query with its current generation.
    pub fn configure(&self, config: LibrarySearchConfig) -> bool {
        if let Ok(mut mail) = self.shared.mail.lock() {
            if mail.config == config {
                return false;
            }
            mail.cancel.cancel();
            mail.config = config;
            mail.revision = mail.revision.wrapping_add(1);
            mail.refresh = true;
            mail.result = None;
            mail.progress = None;
            self.shared.wake.notify_one();
            return true;
        }
        false
    }
    pub fn set_paused(&self, paused: bool) {
        if self.shared.paused.swap(paused, Ordering::AcqRel) != paused {
            if paused && let Ok(mail) = self.shared.mail.lock() {
                mail.cancel.cancel();
            }
            if paused
                && let Ok(interrupt) = self.shared.interrupt.lock()
                && let Some(interrupt) = interrupt.as_ref()
            {
                interrupt.interrupt();
            }
            self.shared.wake.notify_one();
        }
    }
    pub fn search(&self, generation: u64, query: String, semantic: bool) {
        if let Ok(mut mail) = self.shared.mail.lock() {
            mail.cancel.cancel();
            mail.query = Some(Query {
                generation,
                text: query.chars().take(1024).collect(),
                semantic,
            });
            mail.result = None;
            self.shared.wake.notify_one();
        }
    }
    pub fn refresh(&self) {
        if let Ok(mut mail) = self.shared.mail.lock() {
            mail.cancel.cancel();
            mail.result = None;
            mail.progress = None;
            mail.refresh = true;
            self.shared.wake.notify_one();
        }
    }
    pub fn rebuild(&self) {
        if let Ok(mut mail) = self.shared.mail.lock() {
            mail.cancel.cancel();
            mail.rebuild = true;
            self.shared.wake.notify_one();
        }
    }
    fn cancelled(&self, cancel: &CancellationToken) -> bool {
        cancel.is_cancelled()
            || self.shared.stopped.load(Ordering::Acquire)
            || self.shared.paused.load(Ordering::Acquire)
            || crate::performance_runtime::production_workload_coordinator().is_busy()
    }
}
impl LibrarySearchWorker {
    pub fn spawn(database_path: PathBuf) -> Result<Self, String> {
        let handle = LibrarySearchHandle {
            shared: Arc::new(Shared {
                mail: Mutex::new(Mailbox::default()),
                wake: Condvar::new(),
                paused: AtomicBool::new(true),
                stopped: AtomicBool::new(false),
                invalidated: AtomicBool::new(false),
                interrupt: Mutex::new(None),
            }),
        };
        let background = handle.clone();
        thread::Builder::new()
            .name("phorminx-library".into())
            .spawn(move || run(database_path, background))
            .map_err(|_| "Library worker could not start".to_owned())?;
        Ok(Self { handle })
    }
    pub fn handle(&self) -> LibrarySearchHandle {
        self.handle.clone()
    }
    pub fn take_invalidated(&self) -> bool {
        self.handle.take_invalidated()
    }
    pub fn try_recv(&self) -> Option<LibrarySearchUpdate> {
        let mut mail = self.handle.shared.mail.lock().ok()?;
        mail.result.take().or_else(|| mail.progress.take())
    }
}
impl Drop for LibrarySearchWorker {
    fn drop(&mut self) {
        self.handle.shared.stopped.store(true, Ordering::Release);
        if let Ok(mail) = self.handle.shared.mail.lock() {
            mail.cancel.cancel();
        }
        self.handle.shared.wake.notify_one();
    }
}

fn publish(
    handle: &LibrarySearchHandle,
    revision: u64,
    cancel: &CancellationToken,
    update: LibrarySearchUpdate,
) {
    if handle.cancelled(cancel) {
        return;
    }
    if let Ok(mut mail) = handle.shared.mail.lock() {
        if mail.revision != revision || cancel.is_cancelled() {
            return;
        }
        if update.generation == 0 {
            mail.progress = Some(update);
        } else if mail
            .query
            .as_ref()
            .is_none_or(|q| q.generation == update.generation)
        {
            mail.result = Some(update);
        }
    }
}

fn invalidate_sources(handle: &LibrarySearchHandle) {
    if let Ok(mut mail) = handle.shared.mail.lock() {
        mail.result = None;
        mail.progress = None;
    }
    handle.shared.invalidated.store(true, Ordering::Release);
}

fn enforce_retention(
    database: &Persistence,
    now_ms: i64,
) -> Result<bool, phorminx_persistence::PersistenceError> {
    database
        .history()
        .purge_expired(now_ms)
        .map(|removed| removed > 0)
}

fn unix_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis().min(i64::MAX as u128) as i64)
}
fn validated_model(
    client: &OllamaClient,
    config: &LibrarySearchConfig,
    cancel: &CancellationToken,
) -> Result<(ModelName, String), ()> {
    let name = ModelName::parse(config.model.clone().ok_or(())?).map_err(|_| ())?;
    let catalog = client.discover(cancel).map_err(|_| ())?;
    let installed = catalog.models().iter().find(|m| m.name == name).ok_or(())?;
    let digest = installed
        .digest
        .as_deref()
        .filter(|d| !d.is_empty())
        .ok_or(())?;
    let identity = embedding_identity(name.as_str(), digest);
    if config.model_identity.as_deref() != Some(&identity) {
        return Err(());
    }
    client
        .validate_local_embedding_model(&name, cancel)
        .map_err(|_| ())?;
    Ok((name, identity))
}
fn run(path: PathBuf, handle: LibrarySearchHandle) {
    // Short deadlines bound shutdown and cancelled-service residue. Dropping the
    // UI never joins this worker or blocks recording on a library operation.
    let Ok(client) = OllamaClient::new(
        OllamaEndpoint::default(),
        ClientTimeouts {
            connect: Duration::from_secs(1),
            response_headers: Duration::from_secs(8),
            response_body: Duration::from_secs(3),
            overall: Duration::from_secs(12),
        },
    ) else {
        return;
    };
    let mut db = None;
    let mut applied_revision = u64::MAX;
    let mut stats = LibraryIndexStats::default();
    let mut exhausted = false;
    let mut next_index = Instant::now();
    let mut last_version = 0_i64;
    let mut safe_passage_chars = 1000_usize;
    let mut next_retention = Instant::now();
    let mut next_source_check = Instant::now();
    let mut observed_source_version = None;
    let mut retention_failed = false;
    while !handle.shared.stopped.load(Ordering::Acquire) {
        let (config, revision, query, rebuild, refresh, cancel) = {
            let Ok(mail) = handle.shared.mail.lock() else {
                return;
            };
            let Ok((mut mail, _)) = handle
                .shared
                .wake
                .wait_timeout(mail, Duration::from_millis(200))
            else {
                return;
            };
            if handle.shared.stopped.load(Ordering::Acquire) {
                return;
            }
            if handle.shared.paused.load(Ordering::Acquire)
                || crate::performance_runtime::production_workload_coordinator().is_busy()
            {
                continue;
            }
            let cancel = CancellationToken::new();
            mail.cancel = cancel.clone();
            (
                mail.config.clone(),
                mail.revision,
                mail.query.take(),
                std::mem::take(&mut mail.rebuild),
                std::mem::take(&mut mail.refresh),
                cancel,
            )
        };
        if db.is_none() {
            db = Persistence::open_with_busy_timeout(&path, Duration::from_millis(100)).ok();
            if let Some(db) = db.as_ref()
                && let Ok(mut interrupt) = handle.shared.interrupt.lock()
            {
                *interrupt = Some(db.interrupt_handle());
            }
        }
        let Some(database) = db.as_ref() else {
            publish(
                &handle,
                revision,
                &cancel,
                LibrarySearchUpdate {
                    generation: query.as_ref().map_or(0, |q| q.generation),
                    query: query.map_or_else(String::new, |q| q.text),
                    hits: vec![],
                    stats: stats.clone(),
                    status: "Library storage is unavailable. Dictation is unaffected.".into(),
                    failed: true,
                    semantic_available: false,
                },
            );
            continue;
        };
        if retention_failed && Instant::now() < next_retention {
            if let Some(query) = query {
                requeue(&handle, revision, query);
            }
            continue;
        }
        if query.is_some() || Instant::now() >= next_retention {
            match enforce_retention(database, unix_millis()) {
                Ok(true) => {
                    invalidate_sources(&handle);
                    exhausted = false;
                    stats = database.library().stats().unwrap_or_default();
                }
                Ok(false) => {}
                Err(_) => {
                    // Fail closed: don't show potentially expired data while a
                    // retention transaction cannot complete; retry when idle.
                    if !retention_failed {
                        invalidate_sources(&handle);
                    }
                    retention_failed = true;
                    next_retention = Instant::now() + Duration::from_secs(1);
                    if let Some(query) = query {
                        requeue(&handle, revision, query);
                    }
                    continue;
                }
            }
            retention_failed = false;
            next_retention = Instant::now() + Duration::from_secs(60);
        }
        if query.is_some() || refresh || Instant::now() >= next_source_check {
            if let Ok(version) = database.library().data_version() {
                if observed_source_version.is_some_and(|old| old != version) {
                    invalidate_sources(&handle);
                    exhausted = false;
                    stats = database.library().stats().unwrap_or_default();
                }
                observed_source_version = Some(version);
            }
            next_source_check = Instant::now() + Duration::from_secs(2);
        }
        if applied_revision != revision || rebuild {
            let operation = if config.history_enabled && config.model.is_some() {
                if rebuild {
                    let _ = database.library().clear_index();
                }
                if let Some(identity) = config.model_identity.as_deref() {
                    database.library().configure_model(identity).map(|_| ())
                } else {
                    Ok(())
                }
            } else {
                database.library().clear_index()
            };
            if operation.is_err() {
                if let Some(query) = query {
                    requeue(&handle, revision, query);
                }
                continue;
            }
            applied_revision = revision;
            exhausted = false;
            next_index = Instant::now();
            safe_passage_chars = 1000;
            stats = database.library().stats().unwrap_or_default();
        }
        if refresh {
            exhausted = false;
            stats = database.library().stats().unwrap_or_default();
        }
        if !config.history_enabled {
            publish(
                &handle,
                revision,
                &cancel,
                LibrarySearchUpdate {
                    generation: query.as_ref().map_or(0, |q| q.generation),
                    query: query.map_or_else(String::new, |q| q.text),
                    hits: vec![],
                    stats: LibraryIndexStats::default(),
                    status: "History is off. No dictations are indexed.".into(),
                    failed: false,
                    semantic_available: false,
                },
            );
            continue;
        }
        if let Some(query) = query {
            let source_version = database.library().data_version().unwrap_or_default();
            let mut vector = None;
            let mut identity = None;
            let mut unavailable = false;
            if query.semantic && !query.text.trim().is_empty() {
                match validated_model(&client, &config, &cancel) {
                    Ok((model, id)) if !handle.cancelled(&cancel) => {
                        match client.embed(
                            &model,
                            std::slice::from_ref(&query.text),
                            KeepAlive::UnloadAfterRequest,
                            &cancel,
                        ) {
                            Ok(mut vectors) => {
                                vector = vectors.pop();
                                identity = Some(id);
                            }
                            Err(_) => unavailable = true,
                        }
                    }
                    _ => unavailable = true,
                }
            }
            if handle.cancelled(&cancel) {
                requeue(&handle, revision, query);
                continue;
            }
            let embedding = identity.as_deref().zip(vector.as_deref());
            let result = database
                .library()
                .search_cancellable(&query.text, embedding, 50, || handle.cancelled(&cancel));
            if handle.cancelled(&cancel)
                || database.library().data_version().unwrap_or_default() != source_version
            {
                requeue(&handle, revision, query);
                continue;
            }
            let failed = result.is_err();
            publish(&handle,revision,&cancel,LibrarySearchUpdate{generation:query.generation,query:query.text,hits:result.unwrap_or_default(),stats:stats.clone(),status:if failed{"Search is unavailable. Dictation is unaffected."}else if unavailable{"Showing keyword matches. Choose an installed local embedding model to search by meaning."}else{"Search complete."}.into(),failed,semantic_available:embedding.is_some()});
            continue;
        }
        if Instant::now() < next_index || config.model.is_none() {
            continue;
        }
        next_index = Instant::now() + Duration::from_secs(2);
        // data_version is constant-time and changes only for another connection's
        // writes; a completed library is not rescanned on every idle tick.
        let version = database.library().data_version().unwrap_or(last_version);
        if exhausted && version == last_version {
            continue;
        }
        last_version = version;
        let pending = match database.library().pending_passages(1) {
            Ok(pending) => pending,
            Err(_) => {
                next_index = Instant::now() + Duration::from_secs(30);
                publish(
                    &handle,
                    revision,
                    &cancel,
                    LibrarySearchUpdate {
                        generation: 0,
                        query: String::new(),
                        hits: vec![],
                        stats: stats.clone(),
                        status:
                            "Library storage is temporarily unavailable. Dictation is unaffected."
                                .into(),
                        failed: true,
                        semantic_available: false,
                    },
                );
                continue;
            }
        };
        if pending.is_empty() {
            exhausted = true;
            stats = database.library().stats().unwrap_or_default();
            publish(
                &handle,
                revision,
                &cancel,
                LibrarySearchUpdate {
                    generation: 0,
                    query: String::new(),
                    hits: vec![],
                    stats: stats.clone(),
                    status: "Library index is up to date.".into(),
                    failed: false,
                    semantic_available: true,
                },
            );
            continue;
        }
        let result = (|| {
            let (model, identity) = validated_model(&client, &config, &cancel)?;
            if handle.cancelled(&cancel) {
                return Err(());
            }
            let passage = pending.into_iter().next().ok_or(())?;
            let entry = embed_passage(
                &client,
                &model,
                passage,
                &mut safe_passage_chars,
                &cancel,
                || handle.cancelled(&cancel),
            )
            .map_err(|_| ())?;
            if handle.cancelled(&cancel) {
                return Err(());
            }
            database
                .library()
                .index_passages(&identity, &[entry])
                .map_err(|_| ())?;
            Ok::<(), ()>(())
        })();
        if handle.cancelled(&cancel) {
            continue;
        }
        stats = database.library().stats().unwrap_or_default();
        if result.is_err() {
            next_index = Instant::now() + Duration::from_secs(30);
        }
        publish(&handle,revision,&cancel,LibrarySearchUpdate{generation:0,query:String::new(),hits:vec![],stats:stats.clone(),status:if result.is_ok(){"Indexing saved dictations while idle."}else{"Semantic indexing is unavailable. Keyword search still works; select an installed local embedding model."}.into(),failed:result.is_err(),semantic_available:result.is_ok()});
    }
}
fn embed_passage(
    client: &OllamaClient,
    model: &ModelName,
    mut passage: LibraryPassage,
    safe_chars: &mut usize,
    cancel: &CancellationToken,
    cancelled: impl Fn() -> bool,
) -> Result<EmbeddedPassage, ClientError> {
    loop {
        passage.text = passage.text.chars().take(*safe_chars).collect();
        passage.end_char = passage.start_char + passage.text.chars().count();
        passage.end_byte = passage.start_byte + passage.text.len();
        if cancelled() {
            return Err(ClientError::Cancelled);
        }
        match client.embed(
            model,
            std::slice::from_ref(&passage.text),
            KeepAlive::UnloadAfterRequest,
            cancel,
        ) {
            Ok(mut vectors) => {
                if cancelled() {
                    return Err(ClientError::Cancelled);
                }
                return Ok(EmbeddedPassage {
                    passage,
                    vector: vectors.pop().ok_or(ClientError::InvalidEmbeddings)?,
                });
            }
            Err(ClientError::EmbeddingContextExceeded) if *safe_chars > 64 => {
                *safe_chars = (*safe_chars / 2).max(64);
            }
            Err(error) => return Err(error),
        }
    }
}
fn requeue(handle: &LibrarySearchHandle, revision: u64, query: Query) {
    if let Ok(mut mail) = handle.shared.mail.lock()
        && mail.revision == revision
        && mail.query.is_none()
    {
        mail.query = Some(query);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn handle() -> LibrarySearchHandle {
        LibrarySearchHandle {
            shared: Arc::new(Shared {
                mail: Mutex::new(Mailbox::default()),
                wake: Condvar::new(),
                paused: AtomicBool::new(false),
                stopped: AtomicBool::new(false),
                invalidated: AtomicBool::new(false),
                interrupt: Mutex::new(None),
            }),
        }
    }
    #[test]
    fn pending_queries_coalesce_and_cancel_previous() {
        let h = handle();
        let cancel = h.shared.mail.lock().unwrap().cancel.clone();
        for n in 1..100 {
            h.search(n, format!("query {n}"), true);
        }
        let mail = h.shared.mail.lock().unwrap();
        assert!(cancel.is_cancelled());
        assert_eq!(mail.query.as_ref().unwrap().generation, 99);
    }
    #[test]
    fn configure_is_idempotent_and_invalidates_old_results() {
        let h = handle();
        let config = LibrarySearchConfig {
            history_enabled: true,
            ..Default::default()
        };
        h.configure(config.clone());
        let rev = h.shared.mail.lock().unwrap().revision;
        h.configure(config);
        assert_eq!(h.shared.mail.lock().unwrap().revision, rev);
        h.configure(Default::default());
        assert_eq!(h.shared.mail.lock().unwrap().revision, rev + 1);
    }
    #[test]
    fn pause_cancels_without_taking_a_recording_lease() {
        let h = handle();
        let cancel = h.shared.mail.lock().unwrap().cancel.clone();
        h.set_paused(true);
        assert!(cancel.is_cancelled());
        assert!(h.shared.paused.load(Ordering::Acquire));
        h.set_paused(false);
        assert!(!h.shared.paused.load(Ordering::Acquire));
    }
    #[test]
    fn cancelled_query_never_overwrites_newer_request() {
        let h = handle();
        h.search(2, "new".into(), false);
        requeue(
            &h,
            0,
            Query {
                generation: 1,
                text: "old".into(),
                semantic: true,
            },
        );
        assert_eq!(
            h.shared
                .mail
                .lock()
                .unwrap()
                .query
                .as_ref()
                .unwrap()
                .generation,
            2
        );
    }
    #[test]
    fn embedding_identity_includes_model_and_digest() {
        assert_ne!(
            embedding_identity("a", "one"),
            embedding_identity("a", "two")
        );
        assert_ne!(
            embedding_identity("a", "one"),
            embedding_identity("b", "one")
        );
    }
    #[test]
    fn natural_expiry_removes_vectors_and_invalidation_is_one_shot() {
        use phorminx_persistence::{DictationDraft, RetentionPolicy, TimingMetadata};
        let temp = tempfile::tempdir().unwrap();
        let db = Persistence::open(temp.path().join("expiry-synthetic.db")).unwrap();
        db.history()
            .set_retention(RetentionPolicy::Hours24, 0)
            .unwrap();
        let text = "Expiring synthetic saved note";
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
            .unwrap();
        db.library().configure_model("test-identity").unwrap();
        let passage = db.library().pending_passages(1).unwrap().remove(0);
        db.library()
            .index_passages(
                "test-identity",
                &[EmbeddedPassage {
                    passage,
                    vector: vec![1., 0.],
                }],
            )
            .unwrap();
        assert!(!enforce_retention(&db, 86_400_010).unwrap());
        assert_eq!(db.library().stats().unwrap().indexed_passages, 1);
        assert!(enforce_retention(&db, 86_400_011).unwrap());
        assert_eq!(db.library().stats().unwrap().indexed_passages, 0);
        assert!(db.library().search("saved", None, 10).unwrap().is_empty());
        let handle = handle();
        invalidate_sources(&handle);
        assert!(handle.take_invalidated());
        assert!(!handle.take_invalidated());
    }
    #[test]
    fn retention_lock_failure_invalidates_once_then_recovers_query() {
        use phorminx_persistence::{DictationDraft, RetentionPolicy, TimingMetadata};
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("locked-expiry.db");
        let db = Persistence::open(&path).unwrap();
        let now = unix_millis();
        db.history()
            .set_retention(RetentionPolicy::Hours24, now)
            .unwrap();
        let text = "Synthetic current note";
        db.history()
            .insert(&DictationDraft {
                created_at_ms: now,
                raw_text: text.into(),
                normalized_text: None,
                cleaned_text: None,
                selected_output: text.into(),
                language: None,
                target_executable: None,
                timings: TimingMetadata::default(),
                warnings: vec![],
            })
            .unwrap();
        let worker = LibrarySearchWorker::spawn(path.clone()).unwrap();
        let handle = worker.handle();
        handle.configure(LibrarySearchConfig {
            history_enabled: true,
            ..Default::default()
        });
        handle.set_paused(false);
        handle.search(1, "current".into(), false);
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(update) = worker.try_recv()
                && update.generation == 1
            {
                assert_eq!(update.hits.len(), 1);
                break;
            }
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(20));
        }
        worker.take_invalidated();
        let writer = rusqlite::Connection::open(path).unwrap();
        writer.execute_batch("BEGIN IMMEDIATE").unwrap();
        handle.search(2, "current".into(), false);
        while !worker.take_invalidated() {
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(20));
        }
        thread::sleep(Duration::from_millis(250));
        assert!(!worker.take_invalidated());
        assert!(worker.try_recv().is_none());
        writer.execute_batch("COMMIT").unwrap();
        loop {
            if let Some(update) = worker.try_recv()
                && update.generation == 2
            {
                assert_eq!(update.hits.len(), 1);
                assert!(!update.failed);
                break;
            }
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(20));
        }
        drop(worker);
        thread::sleep(Duration::from_millis(250));
    }
    #[test]
    fn background_worker_keeps_keyword_search_available_without_a_model_and_obeys_pause() {
        use phorminx_persistence::{DictationDraft, RetentionPolicy, TimingMetadata};
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("worker-synthetic.db");
        let db = Persistence::open(&path).unwrap();
        db.history()
            .set_retention(RetentionPolicy::Indefinite, 0)
            .unwrap();
        let text = "The synthetic library contains a database backup plan.";
        let id = db
            .history()
            .insert(&DictationDraft {
                created_at_ms: 1,
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
            .unwrap();
        let worker = LibrarySearchWorker::spawn(path).unwrap();
        let handle = worker.handle();
        handle.configure(LibrarySearchConfig {
            history_enabled: true,
            ..Default::default()
        });
        for generation in 1..=20 {
            handle.search(generation, "backup".into(), false);
        }
        thread::sleep(Duration::from_millis(250));
        assert!(worker.try_recv().is_none());
        handle.set_paused(false);
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(update) = worker.try_recv()
                && update.generation != 0
            {
                assert_eq!(update.generation, 20);
                assert!(!update.failed);
                assert_eq!(update.hits[0].history_id, id);
                break;
            }
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(20));
        }
        db.history()
            .set_retention(RetentionPolicy::Disabled, 0)
            .unwrap();
        handle.configure(LibrarySearchConfig::default());
        handle.search(21, "backup".into(), false);
        loop {
            if let Some(update) = worker.try_recv()
                && update.generation == 21
            {
                assert!(update.hits.is_empty());
                assert_eq!(update.stats.indexed_passages, 0);
                break;
            }
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(20));
        }
        drop(worker);
        thread::sleep(Duration::from_millis(250));
    }
    #[test]
    #[ignore = "requires an explicitly installed local embedding model; synthetic data only"]
    fn native_local_embeddings_retrieve_saved_topic_without_keyword_overlap() {
        let total_started = Instant::now();
        use phorminx_persistence::{DictationDraft, RetentionPolicy, TimingMetadata};
        let temp = tempfile::tempdir().unwrap();
        let db = Persistence::open(temp.path().join("synthetic.db")).unwrap();
        db.history()
            .set_retention(RetentionPolicy::Indefinite, 0)
            .unwrap();
        let client = OllamaClient::default();
        let cancel = CancellationToken::new();
        let model = ModelName::parse(
            std::env::var("PHORMINX_EMBEDDING_TEST_MODEL").expect("explicit test model required"),
        )
        .unwrap();
        client
            .validate_local_embedding_model(&model, &cancel)
            .unwrap();
        let catalog = client.discover(&cancel).unwrap();
        let installed = catalog.models().iter().find(|m| m.name == model).unwrap();
        let identity = embedding_identity(model.as_str(), installed.digest.as_deref().unwrap());
        db.library().configure_model(&identity).unwrap();
        let texts = [
            "The production database needs a local backup before we migrate the customer records.",
            "Our restaurant reservation is Friday evening.\nPlease confirm the vegetarian menu.",
            "The mountain hiking trail closes during winter snowstorms.",
        ];
        let mut ids = Vec::new();
        for text in texts {
            ids.push(
                db.history()
                    .insert(&DictationDraft {
                        created_at_ms: 1,
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
                    .unwrap(),
            );
        }
        let pending = db.library().pending_passages(16).unwrap();
        let inputs = pending.iter().map(|p| p.text.clone()).collect::<Vec<_>>();
        let vectors = client
            .embed(&model, &inputs, KeepAlive::UnloadAfterRequest, &cancel)
            .unwrap();
        let batch = pending
            .into_iter()
            .zip(vectors)
            .map(|(passage, vector)| EmbeddedPassage { passage, vector })
            .collect::<Vec<_>>();
        db.library().index_passages(&identity, &batch).unwrap();
        for (query, expected) in [
            (
                "safeguarding stored information ahead of a system upgrade",
                ids[0],
            ),
            ("dinner booking and dietary requirements", ids[1]),
            ("outdoor walking routes in cold weather", ids[2]),
        ] {
            assert!(db.library().search(query, None, 10).unwrap().is_empty());
            let embedding_started = Instant::now();
            let vector = client
                .embed(
                    &model,
                    &[query.into()],
                    KeepAlive::UnloadAfterRequest,
                    &cancel,
                )
                .unwrap();
            let embedding_ms = embedding_started.elapsed().as_millis();
            let search_started = Instant::now();
            let hits = db
                .library()
                .search(query, Some((&identity, &vector[0])), 10)
                .unwrap();
            assert_eq!(hits[0].history_id, expected);
            assert!(hits[0].semantic);
            eprintln!(
                "synthetic semantic query={expected} rank=1 cosine={:.4} embedding_ms={embedding_ms} search_us={}",
                hits[0].score,
                search_started.elapsed().as_micros()
            );
        }
        assert_eq!(db.library().stats().unwrap().indexed_passages, 3);
        // Exercise the actual adaptive production helper through a Unicode/code
        // prefix and retrieve a passage near the end, not merely the first chunk.
        let long_text = format!(
            "{}\n{}",
            "日本語電算機 function(x); ".repeat(90),
            "The astronauts are practicing spacecraft navigation and orbital launch procedures. "
                .repeat(5)
        );
        let long_id = db
            .history()
            .insert(&DictationDraft {
                created_at_ms: 2,
                raw_text: long_text.clone(),
                normalized_text: None,
                cleaned_text: None,
                selected_output: long_text.clone(),
                language: None,
                target_executable: None,
                timings: TimingMetadata::default(),
                warnings: vec![],
            })
            .unwrap()
            .unwrap();
        let mut safe_chars = 1000;
        let mut frontier = 0;
        let mut steps = 0;
        let indexing_started = Instant::now();
        while let Some(passage) = db.library().pending_passages(1).unwrap().pop() {
            assert_eq!(passage.history_id, long_id);
            let entry = embed_passage(&client, &model, passage, &mut safe_chars, &cancel, || false)
                .unwrap();
            assert!(entry.passage.start_char <= frontier);
            assert!(entry.passage.end_char > frontier);
            frontier = entry.passage.end_char;
            assert_eq!(db.library().index_passages(&identity, &[entry]).unwrap(), 1);
            steps += 1;
            assert!(steps < 100);
        }
        assert_eq!(frontier, long_text.chars().count());
        assert_eq!(db.library().stats().unwrap().pending_documents, 0);
        eprintln!(
            "synthetic long-note covered_chars={frontier} passages={steps} safe_passage_chars={safe_chars} index_ms={}",
            indexing_started.elapsed().as_millis()
        );
        let query = "crew training for interplanetary missions";
        assert!(db.library().search(query, None, 10).unwrap().is_empty());
        let embedding_started = Instant::now();
        let vector = client
            .embed(
                &model,
                &[query.into()],
                KeepAlive::UnloadAfterRequest,
                &cancel,
            )
            .unwrap();
        let embedding_ms = embedding_started.elapsed().as_millis();
        let search_started = Instant::now();
        let hits = db
            .library()
            .search(query, Some((&identity, &vector[0])), 10)
            .unwrap();
        assert_eq!(hits[0].history_id, long_id);
        assert!(hits[0].passage.contains("astronauts"));
        eprintln!(
            "synthetic semantic query=4 rank=1 cosine={:.4} embedding_ms={embedding_ms} search_us={} passage_start={} total_ms={}",
            hits[0].score,
            search_started.elapsed().as_micros(),
            hits[0].start_char,
            total_started.elapsed().as_millis()
        );
    }
}
