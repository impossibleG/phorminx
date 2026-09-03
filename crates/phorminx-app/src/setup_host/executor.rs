use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use phorminx_setup::{
    ActionCommand, ActionFailure, ActionId, ActionKey, ActionPhase, ActionProgress, ActionState,
    CapabilityId, CapabilityRecord, ConsentCategory, Coordinator, DesiredConfiguration, EngineKind,
    Language, Planner, SetupAction, SetupPlan, Sha256Digest,
};
use phorminx_windows::SetupOperationLock;

use super::managed::{ArtifactFetcher, FetchError};
use super::{
    ManagedInstall, ManagedRoot, ManagedRootError, NormalizedProbeFact, PinnedCatalog,
    ProbeFactError,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ActionPresentation {
    pub id: ActionId,
    pub key: ActionKey,
    pub dependencies: BTreeSet<ActionId>,
    pub required_consent: BTreeSet<ConsentCategory>,
}

/// A fresh, process-local plan recomputed from trusted probe facts.
pub struct AuthorizedPlan {
    plan: SetupPlan,
    catalog: Arc<PinnedCatalog>,
}

impl AuthorizedPlan {
    #[must_use]
    pub fn actions(&self) -> Vec<ActionPresentation> {
        self.plan
            .actions()
            .iter()
            .map(|planned| ActionPresentation {
                id: planned.action().id().clone(),
                key: planned.action().key().clone(),
                dependencies: planned.dependencies().clone(),
                required_consent: planned.action().consent().clone(),
            })
            .collect()
    }

    pub fn authorize(&self, id: &ActionId) -> Result<AuthorizedAction, AuthorizationError> {
        let planned = self
            .plan
            .actions()
            .iter()
            .find(|planned| planned.action().id() == id)
            .ok_or(AuthorizationError::NotInCurrentPlan)?;
        Ok(AuthorizedAction {
            action: planned.action().clone(),
            dependencies: planned.dependencies().clone(),
            catalog: Arc::clone(&self.catalog),
        })
    }
}

/// Opaque authority for one action from the current recomputed plan.
#[derive(Clone)]
pub struct AuthorizedAction {
    action: SetupAction,
    dependencies: BTreeSet<ActionId>,
    catalog: Arc<PinnedCatalog>,
}

impl AuthorizedAction {
    #[must_use]
    pub const fn id(&self) -> &ActionId {
        self.action.id()
    }

    #[must_use]
    pub const fn required_consent(&self) -> &BTreeSet<ConsentCategory> {
        self.action.consent()
    }
}

#[derive(Clone)]
pub struct SetupAuthority {
    catalog: Arc<PinnedCatalog>,
}

impl SetupAuthority {
    #[must_use]
    pub fn new(catalog: PinnedCatalog) -> Self {
        Self {
            catalog: Arc::new(catalog),
        }
    }

    pub fn phorminx() -> Result<Self, super::CatalogError> {
        Ok(Self::new(PinnedCatalog::phorminx()?))
    }

    pub fn plan(
        &self,
        desired: &DesiredConfiguration,
        facts: impl IntoIterator<Item = NormalizedProbeFact>,
    ) -> Result<AuthorizedPlan, SetupExecutorError> {
        let records = facts
            .into_iter()
            .map(|fact| fact.into_record(&self.catalog))
            .collect::<Result<Vec<_>, _>>()?;
        let plan = Planner::plan(desired, records, self.catalog.policy())?;
        Ok(AuthorizedPlan {
            plan,
            catalog: Arc::clone(&self.catalog),
        })
    }
}

/// Host adapters for platform or external side effects. Implementations must
/// not infer consent; the executor invokes a method only after exact consent
/// equality has been checked against the authorized action.
pub trait ActionAdapters: Send + Sync + 'static {
    fn probe(
        &self,
        capability: &CapabilityId,
        cancel: &AtomicBool,
    ) -> Result<CapabilityRecord, HostActionError>;
    fn import_verified_assets(
        &self,
        artifacts: &BTreeSet<phorminx_setup::ArtifactDescriptor>,
        cancel: &AtomicBool,
    ) -> Result<(), HostActionError>;
    fn validate(
        &self,
        capability: &CapabilityId,
        cancel: &AtomicBool,
    ) -> Result<(), HostActionError>;
    fn activate_recognition(
        &self,
        engine: EngineKind,
        language: Language,
        cancel: &AtomicBool,
    ) -> Result<(), HostActionError>;
    fn grant_microphone_access(&self, cancel: &AtomicBool) -> Result<(), HostActionError>;
    fn select_microphone(&self, cancel: &AtomicBool) -> Result<(), HostActionError>;
    fn guided_ollama_install(&self, cancel: &AtomicBool) -> Result<(), HostActionError>;
    fn start_ollama(&self, cancel: &AtomicBool) -> Result<(), HostActionError>;
    fn pull_ollama_model(
        &self,
        digest: &Sha256Digest,
        cancel: &AtomicBool,
    ) -> Result<(), HostActionError>;
    fn apply_launch_at_login(
        &self,
        enabled: bool,
        cancel: &AtomicBool,
    ) -> Result<(), HostActionError>;
    fn run_benchmark(
        &self,
        protocol: &phorminx_setup::ContentFreeId,
        cancel: &AtomicBool,
    ) -> Result<(), HostActionError>;
    fn rollback(&self, action: &ActionKey, cancel: &AtomicBool) -> Result<(), HostActionError>;
    fn reconcile_external(
        &self,
        action: &ActionKey,
        cancel: &AtomicBool,
    ) -> Result<(), HostActionError>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HostActionError {
    pub failure: ActionFailure,
    pub retryable: bool,
}

impl HostActionError {
    #[must_use]
    pub const fn new(failure: ActionFailure, retryable: bool) -> Self {
        Self { failure, retryable }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ActionEvent {
    pub action_id: ActionId,
    pub state: ActionState,
}

struct ActiveWorker {
    cancel: Arc<AtomicBool>,
    thread: JoinHandle<()>,
}

#[derive(Clone)]
struct EventSink {
    latest: Arc<Mutex<Option<ActionEvent>>>,
    signal: SyncSender<()>,
}

impl EventSink {
    fn publish(&self, event: ActionEvent) {
        if let Ok(mut latest) = self.latest.lock() {
            *latest = Some(event);
            let _ = self.signal.try_send(());
        }
    }
}

pub struct SetupExecutor {
    root: ManagedRoot,
    adapters: Arc<dyn ActionAdapters>,
    fetcher: Arc<dyn ArtifactFetcher>,
    coordinator: Arc<Mutex<Coordinator>>,
    events: EventSink,
    event_rx: Receiver<()>,
    worker: Option<ActiveWorker>,
    succeeded: Arc<Mutex<BTreeSet<ActionId>>>,
    installs: Arc<Mutex<BTreeMap<ActionId, ManagedInstall>>>,
}

impl SetupExecutor {
    #[must_use]
    pub fn new(
        root: ManagedRoot,
        adapters: Arc<dyn ActionAdapters>,
        fetcher: Arc<dyn ArtifactFetcher>,
    ) -> Self {
        let (signal, event_rx) = mpsc::sync_channel(1);
        Self {
            root,
            adapters,
            fetcher,
            coordinator: Arc::new(Mutex::new(Coordinator::default())),
            events: EventSink {
                latest: Arc::new(Mutex::new(None)),
                signal,
            },
            event_rx,
            worker: None,
            succeeded: Arc::new(Mutex::new(BTreeSet::new())),
            installs: Arc::new(Mutex::new(BTreeMap::new())),
        }
    }

    pub fn start(
        &mut self,
        authorized: AuthorizedAction,
        granted_consent: BTreeSet<ConsentCategory>,
    ) -> Result<(), SetupExecutorError> {
        self.reap_finished()?;
        if self.worker.is_some() {
            return Err(SetupExecutorError::Busy);
        }
        if !authorized.dependencies.iter().all(|dependency| {
            self.succeeded
                .lock()
                .is_ok_and(|succeeded| succeeded.contains(dependency))
        }) {
            return Err(SetupExecutorError::Authorization(
                AuthorizationError::DependenciesIncomplete,
            ));
        }
        if granted_consent != *authorized.action.consent() {
            return Err(SetupExecutorError::Consent(ConsentError::ExactSetRequired));
        }
        let ticket = {
            let mut coordinator = self.lock_coordinator()?;
            let ticket = coordinator.start(authorized.action.clone())?;
            if !authorized.action.consent().is_empty() {
                coordinator.command(&ticket, ActionCommand::GrantConsent)?;
            }
            coordinator.command(&ticket, ActionCommand::Start)?;
            ticket
        };
        self.emit_current()?;

        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = Arc::clone(&cancel);
        let root = self.root.clone();
        let adapters = Arc::clone(&self.adapters);
        let fetcher = Arc::clone(&self.fetcher);
        let coordinator = Arc::clone(&self.coordinator);
        let events = self.events.clone();
        let succeeded = Arc::clone(&self.succeeded);
        let installs = Arc::clone(&self.installs);
        let action = authorized.action;
        let catalog = authorized.catalog;
        let worker_ticket = ticket.clone();
        let spawned = thread::Builder::new()
            .name("phorminx-setup-action".to_owned())
            .spawn(move || {
                let lock = SetupOperationLock::try_acquire();
                let result = match lock {
                    Ok(_lease) => execute_action(
                        &root,
                        adapters.as_ref(),
                        fetcher.as_ref(),
                        &catalog,
                        &action,
                        &worker_cancel,
                        &coordinator,
                        &events,
                        &worker_ticket,
                    ),
                    Err(_) => Err(HostActionError::new(
                        ActionFailure::ConsistencyFailure,
                        true,
                    )),
                };
                complete_action(
                    result,
                    &root,
                    &action,
                    adapters.as_ref(),
                    &worker_cancel,
                    &coordinator,
                    &events,
                    &worker_ticket,
                    &succeeded,
                    &installs,
                );
            });
        let thread = match spawned {
            Ok(thread) => thread,
            Err(error) => {
                let mut coordinator = self.lock_coordinator()?;
                let _ = coordinator.command(
                    &ticket,
                    ActionCommand::Fail {
                        failure: ActionFailure::PlatformOperationFailed,
                        retryable: true,
                    },
                );
                if matches!(
                    coordinator.active().map(|runtime| &runtime.state),
                    Some(ActionState::FailedPendingRollback { .. })
                ) {
                    let _ = coordinator.command(&ticket, ActionCommand::RequestRollback);
                    let _ = coordinator.command(&ticket, ActionCommand::RollbackCompleted);
                }
                emit_locked(&coordinator, &self.events);
                return Err(SetupExecutorError::Spawn(error));
            }
        };
        self.worker = Some(ActiveWorker { cancel, thread });
        Ok(())
    }

    pub fn cancel(&self) -> Result<(), SetupExecutorError> {
        let Some(worker) = &self.worker else {
            return Ok(());
        };
        worker.cancel.store(true, Ordering::Release);
        let mut coordinator = match self.coordinator.try_lock() {
            Ok(coordinator) => coordinator,
            Err(std::sync::TryLockError::WouldBlock) => return Ok(()),
            Err(std::sync::TryLockError::Poisoned(_)) => {
                return Err(SetupExecutorError::CoordinatorPoisoned);
            }
        };
        if let Some(ticket) = coordinator.active_ticket()
            && matches!(
                coordinator.active().map(|runtime| &runtime.state),
                Some(ActionState::Running { .. })
            )
        {
            coordinator.command(&ticket, ActionCommand::Cancel)?;
            emit_locked(&coordinator, &self.events);
        }
        Ok(())
    }

    pub fn try_event(&mut self) -> Result<Option<ActionEvent>, SetupExecutorError> {
        self.reap_finished()?;
        match self.event_rx.try_recv() {
            Ok(()) => self
                .events
                .latest
                .lock()
                .map_err(|_| SetupExecutorError::EventStatePoisoned)
                .map(|mut latest| latest.take()),
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => Err(SetupExecutorError::EventChannelClosed),
        }
    }

    pub fn current_state(&self) -> Result<Option<ActionEvent>, SetupExecutorError> {
        let coordinator = self.lock_coordinator()?;
        Ok(coordinator.active().map(|runtime| ActionEvent {
            action_id: runtime.action.id().clone(),
            state: runtime.state.clone(),
        }))
    }

    pub fn durable_installed(&self) -> Result<Vec<ManagedInstall>, SetupExecutorError> {
        self.root
            .installed()
            .map_err(SetupExecutorError::ManagedRoot)
    }

    pub fn shutdown(self) -> Result<ShutdownOutcome, SetupExecutorError> {
        self.shutdown_with_timeout(Duration::from_secs(2))
    }

    pub fn shutdown_with_timeout(
        mut self,
        timeout: Duration,
    ) -> Result<ShutdownOutcome, SetupExecutorError> {
        self.cancel()?;
        if let Some(worker) = self.worker.take() {
            let deadline = std::time::Instant::now() + timeout;
            while !worker.thread.is_finished() && std::time::Instant::now() < deadline {
                thread::sleep(Duration::from_millis(10));
            }
            if !worker.thread.is_finished() {
                drop(worker.thread);
                return Ok(ShutdownOutcome::DetachedAfterTimeout);
            }
            worker
                .thread
                .join()
                .map_err(|_| SetupExecutorError::WorkerPanicked)?;
        }
        Ok(ShutdownOutcome::Joined)
    }

    fn reap_finished(&mut self) -> Result<(), SetupExecutorError> {
        if self
            .worker
            .as_ref()
            .is_some_and(|worker| worker.thread.is_finished())
        {
            let worker = self.worker.take().expect("worker was present");
            worker
                .thread
                .join()
                .map_err(|_| SetupExecutorError::WorkerPanicked)?;
        }
        Ok(())
    }

    fn emit_current(&self) -> Result<(), SetupExecutorError> {
        let coordinator = self.lock_coordinator()?;
        emit_locked(&coordinator, &self.events);
        Ok(())
    }

    fn lock_coordinator(
        &self,
    ) -> Result<std::sync::MutexGuard<'_, Coordinator>, SetupExecutorError> {
        self.coordinator
            .lock()
            .map_err(|_| SetupExecutorError::CoordinatorPoisoned)
    }
}

impl Drop for SetupExecutor {
    fn drop(&mut self) {
        if let Some(worker) = self.worker.take() {
            worker.cancel.store(true, Ordering::Release);
            // Dropping JoinHandle detaches. A blocked external adapter must not
            // freeze application shutdown; its ticket loses all later authority.
            drop(worker.thread);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn execute_action(
    root: &ManagedRoot,
    adapters: &dyn ActionAdapters,
    fetcher: &dyn ArtifactFetcher,
    catalog: &PinnedCatalog,
    action: &SetupAction,
    cancel: &AtomicBool,
    coordinator: &Mutex<Coordinator>,
    events: &EventSink,
    ticket: &phorminx_setup::ActionTicket,
) -> Result<Option<ManagedInstall>, HostActionError> {
    if cancel.load(Ordering::Acquire) {
        return Err(cancelled_error());
    }
    let result = match action.key() {
        ActionKey::DownloadArtifact { artifact } => {
            let pinned = catalog
                .exact_artifact(artifact)
                .ok_or_else(|| HostActionError::new(ActionFailure::VerificationFailed, false))?;
            let install = root
                .install(
                    action,
                    pinned,
                    fetcher,
                    cancel,
                    &mut |phase, completed, total| {
                        let _ =
                            update_progress(coordinator, events, ticket, phase, completed, total);
                    },
                )
                .map_err(map_managed_error)?;
            Ok(Some(install))
        }
        key => {
            run_adapter_action(adapters, key, cancel)?;
            advance_remaining_phases(action, coordinator, events, ticket)?;
            Ok(None)
        }
    }?;
    if cancel.load(Ordering::Acquire) && result.is_some() {
        return Ok(result);
    }
    if cancel.load(Ordering::Acquire) && result.is_none() {
        return Err(cancelled_error());
    }
    advance_remaining_phases(action, coordinator, events, ticket)?;
    Ok(result)
}

fn run_adapter_action(
    adapters: &dyn ActionAdapters,
    key: &ActionKey,
    cancel: &AtomicBool,
) -> Result<(), HostActionError> {
    match key {
        ActionKey::Probe(capability) => adapters.probe(capability, cancel).map(|_| ()),
        ActionKey::ImportVerifiedAssets { artifacts } => {
            adapters.import_verified_assets(artifacts, cancel)
        }
        ActionKey::Validate(capability) => adapters.validate(capability, cancel),
        ActionKey::ActivateRecognition { engine, language } => {
            adapters.activate_recognition(*engine, *language, cancel)
        }
        ActionKey::GrantMicrophoneAccess => adapters.grant_microphone_access(cancel),
        ActionKey::SelectMicrophone => adapters.select_microphone(cancel),
        ActionKey::GuidedExternalInstall { tool } if tool.as_str() == "ollama" => {
            adapters.guided_ollama_install(cancel)
        }
        ActionKey::StartExternalTool { tool } if tool.as_str() == "ollama" => {
            adapters.start_ollama(cancel)
        }
        ActionKey::PullOllamaModel { digest } => adapters.pull_ollama_model(digest, cancel),
        ActionKey::ApplyLaunchAtLogin { enabled } => {
            adapters.apply_launch_at_login(*enabled, cancel)
        }
        ActionKey::RunBenchmark { protocol } => adapters.run_benchmark(protocol, cancel),
        ActionKey::DownloadArtifact { .. }
        | ActionKey::GuidedExternalInstall { .. }
        | ActionKey::StartExternalTool { .. } => Err(HostActionError::new(
            ActionFailure::ConsistencyFailure,
            false,
        )),
    }
}

fn advance_remaining_phases(
    action: &SetupAction,
    coordinator: &Mutex<Coordinator>,
    events: &EventSink,
    ticket: &phorminx_setup::ActionTicket,
) -> Result<(), HostActionError> {
    let current = coordinator
        .lock()
        .map_err(|_| consistency_error())?
        .active()
        .and_then(|runtime| match &runtime.state {
            ActionState::Running { progress } => Some(progress.phase),
            _ => None,
        })
        .ok_or_else(consistency_error)?;
    let phases = action.key().required_phases();
    let start = phases
        .iter()
        .position(|phase| *phase == current)
        .ok_or_else(consistency_error)?;
    for phase in &phases[start + 1..] {
        update_progress(coordinator, events, ticket, *phase, 0, None)?;
    }
    Ok(())
}

fn update_progress(
    coordinator: &Mutex<Coordinator>,
    events: &EventSink,
    ticket: &phorminx_setup::ActionTicket,
    phase: ActionPhase,
    completed: u64,
    total: Option<u64>,
) -> Result<(), HostActionError> {
    let progress = ActionProgress::new(phase, completed, total).map_err(|_| consistency_error())?;
    let mut coordinator = coordinator.lock().map_err(|_| consistency_error())?;
    coordinator
        .command(ticket, ActionCommand::Progress(progress))
        .map_err(|_| consistency_error())?;
    emit_locked(&coordinator, events);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn complete_action(
    result: Result<Option<ManagedInstall>, HostActionError>,
    root: &ManagedRoot,
    action: &SetupAction,
    adapters: &dyn ActionAdapters,
    cancel: &AtomicBool,
    coordinator: &Mutex<Coordinator>,
    events: &EventSink,
    ticket: &phorminx_setup::ActionTicket,
    succeeded: &Mutex<BTreeSet<ActionId>>,
    installs: &Mutex<BTreeMap<ActionId, ManagedInstall>>,
) {
    let Ok(mut coordinator) = coordinator.lock() else {
        return;
    };
    if cancel.load(Ordering::Acquire) {
        if matches!(
            coordinator.active().map(|r| &r.state),
            Some(ActionState::Running { .. })
        ) {
            let _ = coordinator.command(ticket, ActionCommand::Cancel);
        }
        if matches!(
            coordinator.active().map(|r| &r.state),
            Some(ActionState::Cancelling)
        ) {
            let _ = coordinator.command(ticket, ActionCommand::CancellationCompleted);
        }
        if matches!(
            coordinator.active().map(|r| &r.state),
            Some(ActionState::RollbackPending { .. })
        ) {
            let rollback = match &result {
                Ok(Some(install)) => root.rollback(install).map_err(map_managed_error),
                _ => adapters.rollback(action.key(), &AtomicBool::new(false)),
            };
            let command = match rollback {
                Ok(()) => ActionCommand::RollbackCompleted,
                Err(error) => ActionCommand::Fail {
                    failure: error.failure,
                    retryable: error.retryable,
                },
            };
            let _ = coordinator.command(ticket, command);
        }
        emit_locked(&coordinator, events);
        return;
    }
    match result {
        Ok(install) => {
            let succeeded_transition = coordinator.command(ticket, ActionCommand::Succeed).is_ok();
            if succeeded_transition {
                if let Some(install) = install
                    && let Ok(mut installs) = installs.lock()
                {
                    installs.insert(action.id().clone(), install);
                }
                if let Ok(mut succeeded) = succeeded.lock() {
                    succeeded.insert(action.id().clone());
                }
            }
        }
        Err(error) => {
            let _ = coordinator.command(
                ticket,
                ActionCommand::Fail {
                    failure: error.failure,
                    retryable: error.retryable,
                },
            );
            if matches!(
                coordinator.active().map(|runtime| &runtime.state),
                Some(ActionState::FailedPendingRollback { .. })
            ) {
                let _ = coordinator.command(ticket, ActionCommand::RequestRollback);
                let rollback = adapters.rollback(action.key(), cancel);
                let command = match rollback {
                    Ok(()) => ActionCommand::RollbackCompleted,
                    Err(rollback) => ActionCommand::Fail {
                        failure: rollback.failure,
                        retryable: rollback.retryable,
                    },
                };
                let _ = coordinator.command(ticket, command);
            } else if matches!(
                coordinator.active().map(|runtime| &runtime.state),
                Some(ActionState::FailedExternalSideEffectsMayRemain { .. })
            ) && adapters.reconcile_external(action.key(), cancel).is_ok()
            {
                let _ = coordinator.command(ticket, ActionCommand::ExternalStateReconciled);
            }
        }
    }
    emit_locked(&coordinator, events);
}

fn emit_locked(coordinator: &Coordinator, events: &EventSink) {
    if let Some(runtime) = coordinator.active() {
        events.publish(ActionEvent {
            action_id: runtime.action.id().clone(),
            state: runtime.state.clone(),
        });
    }
}

fn map_managed_error(error: ManagedRootError) -> HostActionError {
    let failure = match error {
        ManagedRootError::Cancelled => ActionFailure::Cancelled,
        ManagedRootError::Fetch(_) => ActionFailure::NetworkUnavailable,
        ManagedRootError::WrongSize { .. }
        | ManagedRootError::WrongDigest
        | ManagedRootError::UnsafeArchivePath
        | ManagedRootError::WrongArchiveRoot
        | ManagedRootError::LinkEntry
        | ManagedRootError::CaseCollision
        | ManagedRootError::TooManyEntries
        | ManagedRootError::ExpansionLimit
        | ManagedRootError::CorruptArchive
        | ManagedRootError::Zip(_) => ActionFailure::VerificationFailed,
        ManagedRootError::Io(ref error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
            ActionFailure::PermissionDenied
        }
        ManagedRootError::Io(_) => ActionFailure::PlatformOperationFailed,
        _ => ActionFailure::ConsistencyFailure,
    };
    HostActionError::new(
        failure,
        matches!(failure, ActionFailure::NetworkUnavailable),
    )
}

const fn cancelled_error() -> HostActionError {
    HostActionError::new(ActionFailure::Cancelled, true)
}

const fn consistency_error() -> HostActionError {
    HostActionError::new(ActionFailure::ConsistencyFailure, false)
}

#[derive(Default)]
pub struct HttpsFetcher;

impl ArtifactFetcher for HttpsFetcher {
    fn fetch(
        &self,
        url: &str,
        output: &mut dyn Write,
        cancel: &AtomicBool,
        progress: &mut dyn FnMut(u64),
    ) -> Result<(), FetchError> {
        if !url.starts_with("https://") {
            return Err(FetchError::Network);
        }
        let config = secure_http_config();
        let agent: ureq::Agent = config.into();
        let response = agent.get(url).call().map_err(|_| FetchError::Network)?;
        let mut reader = response.into_parts().1.into_reader();
        let mut buffer = [0_u8; 128 * 1024];
        let mut downloaded = 0_u64;
        loop {
            if cancel.load(Ordering::Acquire) {
                return Err(FetchError::Cancelled);
            }
            let count = reader.read(&mut buffer).map_err(|_| FetchError::Response)?;
            if count == 0 {
                break;
            }
            output.write_all(&buffer[..count]).map_err(|error| {
                if error.kind() == std::io::ErrorKind::Interrupted {
                    FetchError::Cancelled
                } else {
                    FetchError::Response
                }
            })?;
            downloaded = downloaded.saturating_add(count as u64);
            progress(downloaded);
        }
        Ok(())
    }
}

fn secure_http_config() -> ureq::config::Config {
    ureq::Agent::config_builder()
        .https_only(true)
        .max_redirects(3)
        .max_redirects_will_error(true)
        .timeout_connect(Some(Duration::from_secs(15)))
        .timeout_recv_response(Some(Duration::from_secs(30)))
        // This bounds cancellation latency while a peer stalls between body
        // chunks. The loop checks the cancellation flag after every read.
        .timeout_recv_body(Some(Duration::from_secs(5)))
        .build()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum AuthorizationError {
    #[error("the action is not in the current recomputed plan")]
    NotInCurrentPlan,
    #[error("the action dependencies have not all succeeded")]
    DependenciesIncomplete,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ConsentError {
    #[error("the granted consent must exactly match the authorized action")]
    ExactSetRequired,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ShutdownOutcome {
    Joined,
    DetachedAfterTimeout,
}

#[derive(Debug, thiserror::Error)]
pub enum SetupExecutorError {
    #[error(transparent)]
    ProbeFact(#[from] ProbeFactError),
    #[error(transparent)]
    Plan(#[from] phorminx_setup::PlanError),
    #[error(transparent)]
    Authorization(#[from] AuthorizationError),
    #[error(transparent)]
    Coordinator(#[from] phorminx_setup::CoordinatorError),
    #[error(transparent)]
    Consent(#[from] ConsentError),
    #[error(transparent)]
    ManagedRoot(#[from] ManagedRootError),
    #[error("another setup action is still executing")]
    Busy,
    #[error("the setup action worker could not start: {0}")]
    Spawn(std::io::Error),
    #[error("the setup action worker panicked")]
    WorkerPanicked,
    #[error("the setup coordinator lock is poisoned")]
    CoordinatorPoisoned,
    #[error("the coalesced setup event state is poisoned")]
    EventStatePoisoned,
    #[error("the setup event channel closed")]
    EventChannelClosed,
}

#[cfg(test)]
mod tests {
    use super::*;
    use phorminx_setup::{FormattingChoice, RecognitionChoice};
    use std::sync::atomic::{AtomicU64, AtomicUsize};

    static EXECUTOR_TEST_LOCK: Mutex<()> = Mutex::new(());

    #[derive(Default)]
    struct FakeAdapters {
        selected: AtomicUsize,
        launch_applied: AtomicUsize,
        rollbacks: AtomicUsize,
        delay_ms: AtomicU64,
    }

    impl ActionAdapters for FakeAdapters {
        fn probe(
            &self,
            _capability: &CapabilityId,
            _cancel: &AtomicBool,
        ) -> Result<CapabilityRecord, HostActionError> {
            Err(consistency_error())
        }
        fn import_verified_assets(
            &self,
            _artifacts: &BTreeSet<phorminx_setup::ArtifactDescriptor>,
            _cancel: &AtomicBool,
        ) -> Result<(), HostActionError> {
            Ok(())
        }
        fn validate(
            &self,
            _capability: &CapabilityId,
            _cancel: &AtomicBool,
        ) -> Result<(), HostActionError> {
            Ok(())
        }
        fn activate_recognition(
            &self,
            _engine: EngineKind,
            _language: Language,
            _cancel: &AtomicBool,
        ) -> Result<(), HostActionError> {
            Ok(())
        }
        fn grant_microphone_access(&self, _cancel: &AtomicBool) -> Result<(), HostActionError> {
            Ok(())
        }
        fn select_microphone(&self, _cancel: &AtomicBool) -> Result<(), HostActionError> {
            self.selected.fetch_add(1, Ordering::AcqRel);
            thread::sleep(Duration::from_millis(self.delay_ms.load(Ordering::Acquire)));
            Ok(())
        }
        fn guided_ollama_install(&self, _cancel: &AtomicBool) -> Result<(), HostActionError> {
            Ok(())
        }
        fn start_ollama(&self, _cancel: &AtomicBool) -> Result<(), HostActionError> {
            Ok(())
        }
        fn pull_ollama_model(
            &self,
            _digest: &Sha256Digest,
            _cancel: &AtomicBool,
        ) -> Result<(), HostActionError> {
            Ok(())
        }
        fn apply_launch_at_login(
            &self,
            _enabled: bool,
            _cancel: &AtomicBool,
        ) -> Result<(), HostActionError> {
            self.launch_applied.fetch_add(1, Ordering::AcqRel);
            thread::sleep(Duration::from_millis(self.delay_ms.load(Ordering::Acquire)));
            Ok(())
        }
        fn run_benchmark(
            &self,
            _protocol: &phorminx_setup::ContentFreeId,
            _cancel: &AtomicBool,
        ) -> Result<(), HostActionError> {
            Ok(())
        }
        fn rollback(
            &self,
            _action: &ActionKey,
            _cancel: &AtomicBool,
        ) -> Result<(), HostActionError> {
            self.rollbacks.fetch_add(1, Ordering::AcqRel);
            Ok(())
        }
        fn reconcile_external(
            &self,
            _action: &ActionKey,
            _cancel: &AtomicBool,
        ) -> Result<(), HostActionError> {
            Ok(())
        }
    }

    struct EmptyFetcher;
    impl ArtifactFetcher for EmptyFetcher {
        fn fetch(
            &self,
            _url: &str,
            _output: &mut dyn Write,
            _cancel: &AtomicBool,
            _progress: &mut dyn FnMut(u64),
        ) -> Result<(), FetchError> {
            Err(FetchError::Network)
        }
    }

    fn desired(formatting: FormattingChoice) -> DesiredConfiguration {
        DesiredConfiguration {
            language: Language::English,
            recognition: RecognitionChoice::Accurate,
            formatting,
            launch_at_login: false,
            benchmark_protocol: None,
        }
    }

    fn ready_baseline() -> Vec<NormalizedProbeFact> {
        vec![
            NormalizedProbeFact::Microphone {
                selected_is_available: true,
                permission_denied: false,
            },
            NormalizedProbeFact::Recognition {
                engine: EngineKind::Accurate,
                language: Language::English,
                model_digest: Some(Sha256Digest::new("a".repeat(64)).unwrap()),
                state: super::super::RecognitionProbeState::Ready { resident: true },
            },
            NormalizedProbeFact::LaunchAtLogin {
                enabled: false,
                exact_command: true,
            },
        ]
    }

    #[test]
    fn action_authority_comes_only_from_a_fresh_plan_and_exposes_exact_consent() {
        let authority = SetupAuthority::phorminx().unwrap();
        let digest = Sha256Digest::new("b".repeat(64)).unwrap();
        let mut facts = ready_baseline();
        facts.extend([
            NormalizedProbeFact::OllamaDaemon {
                version: None,
                reachable: false,
                installed: false,
            },
            NormalizedProbeFact::OllamaModel {
                expected_digest: digest.clone(),
                present_digest: Some(digest),
            },
        ]);
        let plan = authority
            .plan(
                &desired(FormattingChoice::Ollama {
                    model_digest: Sha256Digest::new("b".repeat(64)).unwrap(),
                }),
                facts,
            )
            .unwrap();
        let install = plan
            .actions()
            .into_iter()
            .find(|action| matches!(action.key, ActionKey::GuidedExternalInstall { .. }))
            .unwrap();
        assert_eq!(install.required_consent.len(), 3);
        assert!(plan.authorize(&install.id).is_ok());
        assert!(matches!(
            plan.authorize(&ActionId::for_key(&ActionKey::SelectMicrophone)),
            Err(AuthorizationError::NotInCurrentPlan)
        ));
        let start = plan
            .actions()
            .into_iter()
            .find(|action| matches!(action.key, ActionKey::StartExternalTool { .. }))
            .unwrap();
        let start = plan.authorize(&start.id).unwrap();
        let temporary = tempfile::tempdir().unwrap();
        let root = ManagedRoot::for_test(
            &temporary.path().join("managed"),
            super::super::AcquisitionLimits::default(),
        )
        .unwrap();
        let mut executor = SetupExecutor::new(
            root,
            Arc::new(FakeAdapters::default()),
            Arc::new(EmptyFetcher),
        );
        assert!(matches!(
            executor.start(
                start,
                [ConsentCategory::StartBackgroundProcess]
                    .into_iter()
                    .collect()
            ),
            Err(SetupExecutorError::Authorization(
                AuthorizationError::DependenciesIncomplete
            ))
        ));
    }

    #[test]
    fn executor_rejects_partial_or_extra_consent_before_spawning() {
        let authority = SetupAuthority::phorminx().unwrap();
        let mut facts = ready_baseline();
        facts[0] = NormalizedProbeFact::Microphone {
            selected_is_available: false,
            permission_denied: false,
        };
        let plan = authority
            .plan(&desired(FormattingChoice::Deterministic), facts)
            .unwrap();
        let presented = plan.actions().into_iter().next().unwrap();
        let authorized = plan.authorize(&presented.id).unwrap();
        let temporary = tempfile::tempdir().unwrap();
        let root = ManagedRoot::for_test(
            &temporary.path().join("managed"),
            super::super::AcquisitionLimits::default(),
        )
        .unwrap();
        let mut executor = SetupExecutor::new(
            root,
            Arc::new(FakeAdapters::default()),
            Arc::new(EmptyFetcher),
        );
        assert!(matches!(
            executor.start(
                authorized,
                [ConsentCategory::NetworkDownload].into_iter().collect()
            ),
            Err(SetupExecutorError::Consent(ConsentError::ExactSetRequired))
        ));
    }

    #[test]
    fn successful_action_immediately_unlocks_its_dependent_action() {
        let _serial = EXECUTOR_TEST_LOCK.lock().unwrap();
        let authority = SetupAuthority::phorminx().unwrap();
        let digest = Sha256Digest::new("b".repeat(64)).unwrap();
        let mut facts = ready_baseline();
        facts.extend([
            NormalizedProbeFact::OllamaDaemon {
                version: None,
                reachable: false,
                installed: false,
            },
            NormalizedProbeFact::OllamaModel {
                expected_digest: digest.clone(),
                present_digest: Some(digest.clone()),
            },
        ]);
        let plan = authority
            .plan(
                &desired(FormattingChoice::Ollama {
                    model_digest: digest,
                }),
                facts,
            )
            .unwrap();
        let install = plan
            .actions()
            .into_iter()
            .find(|action| matches!(action.key, ActionKey::GuidedExternalInstall { .. }))
            .unwrap();
        let start = plan
            .actions()
            .into_iter()
            .find(|action| matches!(action.key, ActionKey::StartExternalTool { .. }))
            .unwrap();
        let install_consent = install.required_consent.clone();
        let start_consent = start.required_consent.clone();
        let install = plan.authorize(&install.id).unwrap();
        let start = plan.authorize(&start.id).unwrap();
        let temporary = tempfile::tempdir().unwrap();
        let root = ManagedRoot::for_test(
            &temporary.path().join("managed"),
            super::super::AcquisitionLimits::default(),
        )
        .unwrap();
        let mut executor = SetupExecutor::new(
            root,
            Arc::new(FakeAdapters::default()),
            Arc::new(EmptyFetcher),
        );

        executor.start(install, install_consent).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            assert!(std::time::Instant::now() < deadline);
            if executor
                .try_event()
                .unwrap()
                .is_some_and(|event| event.state == ActionState::Succeeded)
            {
                break;
            }
            thread::sleep(Duration::from_millis(5));
        }

        executor.start(start, start_consent).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            assert!(std::time::Instant::now() < deadline);
            if executor
                .try_event()
                .unwrap()
                .is_some_and(|event| event.state == ActionState::Succeeded)
            {
                break;
            }
            thread::sleep(Duration::from_millis(5));
        }
        executor.shutdown().unwrap();
    }

    #[test]
    fn duplicate_operation_is_rejected_and_success_is_reported() {
        let _serial = EXECUTOR_TEST_LOCK.lock().unwrap();
        let authority = SetupAuthority::phorminx().unwrap();
        let mut facts = ready_baseline();
        facts[0] = NormalizedProbeFact::Microphone {
            selected_is_available: false,
            permission_denied: false,
        };
        let plan = authority
            .plan(&desired(FormattingChoice::Deterministic), facts)
            .unwrap();
        let presented = plan.actions().into_iter().next().unwrap();
        let first = plan.authorize(&presented.id).unwrap();
        let second = first.clone();
        let temporary = tempfile::tempdir().unwrap();
        let adapters = Arc::new(FakeAdapters {
            delay_ms: AtomicU64::new(100),
            ..FakeAdapters::default()
        });
        let root = ManagedRoot::for_test(
            &temporary.path().join("managed"),
            super::super::AcquisitionLimits::default(),
        )
        .unwrap();
        let mut executor = SetupExecutor::new(root, adapters.clone(), Arc::new(EmptyFetcher));
        executor.start(first, BTreeSet::new()).unwrap();
        assert!(matches!(
            executor.start(second, BTreeSet::new()),
            Err(SetupExecutorError::Busy)
        ));
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let mut succeeded = false;
        while std::time::Instant::now() < deadline {
            if let Some(event) = executor.try_event().unwrap()
                && event.state == ActionState::Succeeded
            {
                succeeded = true;
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        assert!(succeeded);
        assert_eq!(adapters.selected.load(Ordering::Acquire), 1);
        executor.shutdown().unwrap();
    }

    #[test]
    fn cancellation_of_a_compensating_action_finishes_rollback() {
        let _serial = EXECUTOR_TEST_LOCK.lock().unwrap();
        let authority = SetupAuthority::phorminx().unwrap();
        let mut desired = desired(FormattingChoice::Deterministic);
        desired.launch_at_login = true;
        let plan = authority.plan(&desired, ready_baseline()).unwrap();
        let presented = plan
            .actions()
            .into_iter()
            .find(|action| matches!(action.key, ActionKey::ApplyLaunchAtLogin { .. }))
            .unwrap();
        let consent = presented.required_consent.clone();
        let action = plan.authorize(&presented.id).unwrap();
        let temporary = tempfile::tempdir().unwrap();
        let root = ManagedRoot::for_test(
            &temporary.path().join("managed"),
            super::super::AcquisitionLimits::default(),
        )
        .unwrap();
        let adapters = Arc::new(FakeAdapters {
            delay_ms: AtomicU64::new(100),
            ..FakeAdapters::default()
        });
        let mut executor = SetupExecutor::new(root, adapters.clone(), Arc::new(EmptyFetcher));
        executor.start(action, consent).unwrap();
        let entered_deadline = std::time::Instant::now() + Duration::from_secs(1);
        while adapters.launch_applied.load(Ordering::Acquire) == 0
            && std::time::Instant::now() < entered_deadline
        {
            thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(adapters.launch_applied.load(Ordering::Acquire), 1);
        executor.cancel().unwrap();

        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            assert!(std::time::Instant::now() < deadline);
            if executor.try_event().unwrap().is_some_and(|event| {
                matches!(
                    event.state,
                    ActionState::Cancelled {
                        outcome: phorminx_setup::CancellationOutcome::RollbackCompleted
                    }
                )
            }) {
                break;
            }
            thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(adapters.rollbacks.load(Ordering::Acquire), 1);
        executor.shutdown().unwrap();
    }

    #[test]
    fn shutdown_is_deadlined_even_when_an_adapter_is_temporarily_stalled() {
        let _serial = EXECUTOR_TEST_LOCK.lock().unwrap();
        let authority = SetupAuthority::phorminx().unwrap();
        let mut facts = ready_baseline();
        facts[0] = NormalizedProbeFact::Microphone {
            selected_is_available: false,
            permission_denied: false,
        };
        let plan = authority
            .plan(&desired(FormattingChoice::Deterministic), facts)
            .unwrap();
        let presented = plan.actions().into_iter().next().unwrap();
        let action = plan.authorize(&presented.id).unwrap();
        let temporary = tempfile::tempdir().unwrap();
        let root = ManagedRoot::for_test(
            &temporary.path().join("managed"),
            super::super::AcquisitionLimits::default(),
        )
        .unwrap();
        let adapters = Arc::new(FakeAdapters {
            delay_ms: AtomicU64::new(150),
            ..FakeAdapters::default()
        });
        let mut executor = SetupExecutor::new(root, adapters.clone(), Arc::new(EmptyFetcher));
        executor.start(action, BTreeSet::new()).unwrap();
        let wait_until = std::time::Instant::now() + Duration::from_secs(1);
        while adapters.selected.load(Ordering::Acquire) == 0
            && std::time::Instant::now() < wait_until
        {
            thread::sleep(Duration::from_millis(1));
        }
        let started = std::time::Instant::now();
        assert_eq!(
            executor
                .shutdown_with_timeout(Duration::from_millis(10))
                .unwrap(),
            ShutdownOutcome::DetachedAfterTimeout
        );
        assert!(started.elapsed() < Duration::from_millis(100));
        // Let the detached test adapter release the process-global setup lock
        // before another executor test runs.
        thread::sleep(Duration::from_millis(175));
    }

    #[test]
    fn https_policy_rejects_downgrades_and_bounds_redirects_and_stalls() {
        let config = secure_http_config();
        assert!(config.https_only());
        assert_eq!(config.max_redirects(), 3);
        assert!(config.max_redirects_will_error());
        assert_eq!(config.timeouts().recv_body, Some(Duration::from_secs(5)));
        let mut output = Vec::new();
        assert!(matches!(
            HttpsFetcher.fetch(
                "http://example.invalid/file",
                &mut output,
                &AtomicBool::new(false),
                &mut |_| {}
            ),
            Err(FetchError::Network)
        ));
    }
}
