//! Background meeting coordinator. UI paint, audio ingest and network jobs are independent.
use crate::{
    meeting_audio::{MeetingAudioEvent, MeetingAudioService, MeetingAudioSource},
    performance_runtime::{
        RuntimeActivityKind, RuntimeActivityLease, production_workload_coordinator,
    },
    settings::{HistoryRetention, SettingsStore},
};
use phorminx_assistant::{
    ActionConfig, ActionMethod, AssistantConfig, CancellationToken, ChatMessage, ChatRole,
    ConfigStore, PreparedAction, ProtectedSecret, ProviderConfig, ProviderKind,
    execute_prepared_action, prepare_action, stream_chat,
};
use phorminx_persistence::{MeetingMessageStatus, Persistence};
use phorminx_session::{AiRunTracker, MeetingCutoffs, MeetingSegment};
use phorminx_ui::workspace::*;
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex, mpsc},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const VISIBLE_SEGMENTS: usize = 200;
const VISIBLE_MESSAGES: usize = 80;
const CONTEXT_BYTES: usize = 96 * 1024;
const SHUTDOWN_STALL_TIMEOUT: Duration = Duration::from_secs(5);
#[cfg(test)]
#[path = "action_integration_qa.rs"]
mod action_integration_qa;
#[cfg(test)]
#[path = "meeting_workspace_tests.rs"]
mod integration_tests;
#[cfg(test)]
#[path = "meeting_refinement_tests.rs"]
mod refinement_tests;
#[cfg(test)]
#[path = "meeting_transport_tests.rs"]
mod transport_tests;
enum Command {
    Event(Box<WorkspaceEvent>),
    Action(Box<ActionConfig>),
    Shutdown,
}
enum JobEvent {
    Delta(u64, String),
    End(u64, Result<(), String>),
    Delivered(Result<(), String>),
}
pub struct MeetingWorkspace {
    commands: mpsc::SyncSender<Command>,
    snapshot: Arc<Mutex<WorkspaceSnapshot>>,
    thread: Option<thread::JoinHandle<()>>,
}
impl MeetingWorkspace {
    pub fn start(store: SettingsStore) -> Result<Self, String> {
        let (commands, rx) = mpsc::sync_channel(32);
        let snapshot = Arc::new(Mutex::new(WorkspaceSnapshot::default()));
        let shared = Arc::clone(&snapshot);
        let thread = thread::Builder::new()
            .name("phorminx-meeting-workspace".into())
            .spawn(move || match Host::new(store) {
                Ok(mut host) => host.run(rx, shared),
                Err(message) => {
                    if let Ok(mut snapshot) = shared.lock() {
                        snapshot.notice = message;
                        snapshot.revision = 1;
                    }
                }
            })
            .map_err(|_| "The meeting workspace could not start.".to_owned())?;
        Ok(Self {
            commands,
            snapshot,
            thread: Some(thread),
        })
    }
    pub fn send(&self, event: WorkspaceEvent) -> Result<(), String> {
        self.commands
            .try_send(Command::Event(Box::new(event)))
            .map_err(|_| "The workspace is busy. Please try again.".into())
    }
    pub fn trigger_saved_action(&self, action: ActionConfig) -> Result<(), String> {
        self.commands
            .try_send(Command::Action(Box::new(action)))
            .map_err(|_| "The workspace is busy. Please try again.".into())
    }
    pub fn request_shutdown(&self) -> Result<(), String> {
        if self
            .thread
            .as_ref()
            .is_none_or(|thread| thread.is_finished())
        {
            return Ok(());
        }
        self.commands
            .try_send(Command::Shutdown)
            .map_err(|_| "Waiting for the meeting workspace to accept shutdown.".into())
    }
    pub fn is_finished(&self) -> bool {
        self.thread
            .as_ref()
            .is_none_or(|thread| thread.is_finished())
    }
    pub fn snapshot_after(&self, revision: u64) -> Option<WorkspaceSnapshot> {
        self.snapshot
            .try_lock()
            .ok()
            .filter(|s| s.revision != revision)
            .map(|s| s.clone())
    }
}
impl Drop for MeetingWorkspace {
    fn drop(&mut self) {
        // Product window close only hides it. Actual application shutdown drains the
        // explicitly stopped audio tail before allowing the process to exit.
        let _ = self.commands.send(Command::Shutdown);
        if let Some(worker) = self.thread.take() {
            let _ = worker.join();
        }
    }
}
struct PendingWrite {
    session: i64,
    kind: WriteKind,
}
enum WriteKind {
    Segment {
        sequence: u64,
        segment: MeetingSegment,
    },
    User {
        cutoff: Option<u64>,
        text: String,
        context: String,
        context_start: Option<u64>,
    },
    Start(u64),
    Answer {
        generation: u64,
        text: String,
        status: MeetingMessageStatus,
    },
    Finalize,
}
struct ActiveJob {
    generation: u64,
    cancel: CancellationToken,
    session: Option<i64>,
}
struct PendingChat {
    generation: u64,
    provider: ProviderConfig,
    messages: Vec<ChatMessage>,
    session: Option<i64>,
}
struct Host {
    store: SettingsStore,
    config_store: ConfigStore,
    config: AssistantConfig,
    db: Persistence,
    view: WorkspaceSnapshot,
    audio: Option<MeetingAudioService>,
    capture_lease: Option<RuntimeActivityLease>,
    boundaries: MeetingCutoffs,
    cutoff_id: u64,
    sequence: u64,
    runs: AiRunTracker,
    active: Option<ActiveJob>,
    pending: Option<PendingChat>,
    job_tx: mpsc::SyncSender<JobEvent>,
    job_rx: mpsc::Receiver<JobEvent>,
    delivery_cancel: Option<CancellationToken>,
    note_capture: bool,
    loaded_session: bool,
    search: String,
    deferred_cutoff: Option<(u64, u64)>,
    cutoff_question: Option<String>,
    cutoff_request: Option<u64>,
    action_capture: Option<ActionConfig>,
    memory: Option<crate::meeting_memory::MeetingMemoryWorker>,
    search_generation: u64,
    last_memory_poll: Instant,
    pending_segments: VecDeque<PendingWrite>,
    last_persist: Instant,
    last_delivery: Option<DeliveryAttempt>,
}
struct DeliveryAttempt {
    action: ActionConfig,
    text: String,
    id: String,
    prepared: Arc<Mutex<Option<PreparedAction>>>,
}
impl DeliveryAttempt {
    fn matches(&self, action: &ActionConfig, text: &str) -> bool {
        self.action == *action && self.text == text
    }
}
impl Host {
    fn new(store: SettingsStore) -> Result<Self, String> {
        let directory = store
            .path()
            .parent()
            .ok_or("Settings directory unavailable.")?;
        let config_store = ConfigStore::new(directory.join("assistant.json"));
        let config = config_store.load().map_err(|e| e.to_string())?;
        let db = Persistence::open_with_busy_timeout(
            directory.join("phorminx.db"),
            Duration::from_millis(50),
        )
        .map_err(|_| "Meeting storage could not open.")?;
        let (job_tx, job_rx) = mpsc::sync_channel(64);
        let mut host = Self {
            store,
            config_store,
            config,
            db,
            view: WorkspaceSnapshot::default(),
            audio: None,
            capture_lease: None,
            boundaries: MeetingCutoffs::new(),
            cutoff_id: 0,
            sequence: 0,
            runs: AiRunTracker::new(),
            active: None,
            pending: None,
            job_tx,
            job_rx,
            delivery_cancel: None,
            note_capture: false,
            loaded_session: false,
            search: String::new(),
            deferred_cutoff: None,
            cutoff_question: None,
            cutoff_request: None,
            action_capture: None,
            memory: None,
            search_generation: 0,
            last_memory_poll: Instant::now() - Duration::from_secs(2),
            pending_segments: VecDeque::new(),
            last_persist: Instant::now(),
            last_delivery: None,
        };
        host.refresh_config();
        host.refresh_sessions()?;
        Ok(host)
    }
    fn run(&mut self, rx: mpsc::Receiver<Command>, shared: Arc<Mutex<WorkspaceSnapshot>>) {
        if let Some(directory) = self.store.path().parent() {
            self.memory =
                crate::meeting_memory::MeetingMemoryWorker::spawn(directory.join("phorminx.db"))
                    .ok();
        }
        self.view.input_devices = phorminx_audio::input_devices()
            .map(|devices| devices.into_iter().map(|device| device.name).collect())
            .unwrap_or_default();
        self.view.output_devices = phorminx_audio::output_devices()
            .map(|devices| devices.into_iter().map(|device| device.name).collect())
            .unwrap_or_default();
        let mut published = Instant::now() - Duration::from_secs(1);
        let mut privacy_check = Instant::now() - Duration::from_secs(2);
        let mut shutting_down = false;
        let mut shutdown_stalled_since = Instant::now();
        let mut shutdown_pending = usize::MAX;
        loop {
            if !shutting_down {
                match rx.recv_timeout(Duration::from_millis(20)) {
                    Ok(Command::Event(event)) => {
                        if let Err(message) = self.handle(*event) {
                            self.view.notice = message;
                        }
                    }
                    Ok(Command::Action(action)) => {
                        if let Err(message) = self.trigger_config(*action) {
                            self.view.notice = message;
                        }
                    }
                    Ok(Command::Shutdown) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                        shutting_down = true;
                        self.action_capture = None;
                        self.view.capture_action = None;
                        if let Some(memory) = &self.memory {
                            memory.set_paused(true);
                        }
                        self.cancel_answer();
                        if let Some(cancel) = &self.delivery_cancel {
                            cancel.cancel();
                        }
                        if let Some(audio) = &self.audio {
                            let _ = audio.stop();
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                }
            } else {
                thread::sleep(Duration::from_millis(20));
            }
            while let Some(event) = self.audio.as_ref().and_then(MeetingAudioService::try_recv) {
                if let Err(message) = self.audio_event(event) {
                    self.view.notice = message;
                }
            }
            while let Ok(event) = self.job_rx.try_recv() {
                self.job_event(event);
            }
            if !shutting_down {
                self.start_pending();
                self.poll_memory();
            } else {
                self.cancel_answer();
            }
            if self.last_persist.elapsed() >= Duration::from_millis(500) {
                self.flush_persistence();
                self.last_persist = Instant::now();
            }
            if privacy_check.elapsed() >= Duration::from_secs(1) {
                if let Ok(settings) = self.store.load() {
                    self.view.history_enabled =
                        settings.privacy.history_retention != HistoryRetention::Disabled;
                    if !self.view.history_enabled {
                        if self.loaded_session {
                            self.cancel_answer();
                            self.view.transcript.clear();
                            self.view.messages.clear();
                            self.view.provisional_text.clear();
                            self.view.title.clear();
                            self.loaded_session = false;
                            self.view.saved_session = false;
                            self.view.total_segments = 0;
                            self.view.transcript_offset = 0;
                        }
                        self.view.session_id = None;
                        self.view.sessions.clear();
                        self.pending_segments.clear();
                    }
                    if let Some(id) = self.view.session_id
                        && matches!(self.db.meetings().get(id), Ok(None))
                    {
                        self.view.session_id = None;
                        self.pending_segments.retain(|p| p.session != id);
                    }
                }
                privacy_check = Instant::now();
            }
            if published.elapsed() >= Duration::from_millis(100) {
                self.publish_snapshot(&shared);
                published = Instant::now();
            }
            if shutting_down && self.audio.is_none() {
                let ready = self.drain_shutdown(&mut shutdown_pending, &mut shutdown_stalled_since);
                self.publish_snapshot(&shared);
                if ready {
                    break;
                }
            }
        }
        self.cancel_answer();
        if let Some(cancel) = &self.delivery_cancel {
            cancel.cancel();
        }
        if let Some(audio) = &self.audio {
            let _ = audio.stop();
        }
    }
    fn drain_shutdown(&mut self, pending: &mut usize, stalled_since: &mut Instant) -> bool {
        self.flush_persistence();
        if self.pending_segments.is_empty() {
            self.view.shutdown_ready = true;
            self.view.shutdown_error.clear();
            return true;
        }
        if self.pending_segments.len() < *pending {
            *stalled_since = Instant::now();
        }
        *pending = self.pending_segments.len();
        if stalled_since.elapsed() >= SHUTDOWN_STALL_TIMEOUT {
            self.view.shutdown_error="Phorminx cannot finish quitting while local storage is unavailable. Your pending text remains in memory; saving will retry automatically. Keep this window open.".into();
            self.view.notice = self.view.shutdown_error.clone();
        }
        false
    }
    fn publish_snapshot(&mut self, shared: &Arc<Mutex<WorkspaceSnapshot>>) {
        if let Ok(mut current) = shared.lock() {
            self.view.revision = current.revision;
            if *current != self.view {
                self.view.revision = self.view.revision.saturating_add(1);
                *current = self.view.clone();
            }
        }
    }
    fn handle(&mut self, event: WorkspaceEvent) -> Result<(), String> {
        match event {
            WorkspaceEvent::DeleteSelected {
                dictations,
                meeting_transcripts,
                chats,
            } => {
                let result = self.delete_selected(phorminx_persistence::DeleteSelection {
                    dictations,
                    meeting_transcripts,
                    chats,
                });
                self.view.deletion_revision = self.view.deletion_revision.saturating_add(1);
                self.view.deletion_failed = result.is_err();
                self.view.deletion_notice = result
                    .as_ref()
                    .err()
                    .cloned()
                    .unwrap_or_else(|| self.view.notice.clone());
                result
            }
            WorkspaceEvent::Refresh => self.refresh_sessions(),
            WorkspaceEvent::Search(query) => {
                self.search = query;
                self.refresh_sessions()?;
                self.search_generation = self.search_generation.saturating_add(1);
                if let Some(memory) = &self.memory {
                    memory.search(self.search_generation, self.search.clone());
                }
                Ok(())
            }
            WorkspaceEvent::New => {
                self.new_session()?;
                Ok(())
            }
            WorkspaceEvent::Select(id) => self.open_session(id),
            WorkspaceEvent::Delete(id) => {
                if self.audio.is_some() {
                    return Err("Stop recording before deleting a session.".into());
                }
                if self.view.session_id == Some(id) {
                    self.new_session()?;
                }
                self.invalidate_meeting_memory();
                self.db
                    .meetings()
                    .delete(id)
                    .map_err(|_| "The session could not be deleted.")?;
                self.refresh_sessions()
            }
            WorkspaceEvent::Start {
                source,
                device,
                title,
                note,
            } => self.start_capture(source, device, title, note),
            WorkspaceEvent::Stop => {
                if let Some(audio) = &self.audio {
                    audio.stop()?;
                    self.view.capture = CaptureState::Stopping;
                }
                Ok(())
            }
            WorkspaceEvent::SendCutoff => self.send_cutoff(),
            WorkspaceEvent::SendChat(text) => self.send_question(text),
            WorkspaceEvent::SendChatRequest { id, text } => {
                if self.view.send_pending {
                    self.view.chat_error_id = id;
                    return Err(
                        "Finishing the previous question's context. Your draft is retained.".into(),
                    );
                }
                let revision = self.view.chat_revision;
                self.cutoff_request = Some(id);
                match self.send_question(text) {
                    Ok(()) => {
                        if self.view.chat_revision != revision {
                            self.view.chat_ack_id = id;
                            self.cutoff_request = None;
                        }
                        Ok(())
                    }
                    Err(error) => {
                        self.view.chat_error_id = id;
                        self.cutoff_request = None;
                        Err(error)
                    }
                }
            }
            WorkspaceEvent::StartMeeting => {
                if self.audio.is_some() {
                    return Ok(());
                }
                self.start_capture(
                    if self.config.meeting_microphone {
                        CaptureSource::Microphone
                    } else {
                        CaptureSource::SystemAudio
                    },
                    self.config.meeting_device.clone(),
                    String::new(),
                    false,
                )
            }
            WorkspaceEvent::SaveCapturePreferences { source, device } => {
                if self.audio.is_some() {
                    return Err("Stop recording before changing the audio source.".into());
                }
                let mut config = self.config.clone();
                config.meeting_microphone = source == CaptureSource::Microphone;
                config.meeting_device = device;
                self.config_store.save(&config).map_err(|e| e.to_string())?;
                self.config = config;
                self.refresh_config();
                Ok(())
            }
            WorkspaceEvent::OpenCompanion | WorkspaceEvent::HideCompanion => Ok(()),
            WorkspaceEvent::TriggerAction(id) => self.trigger_action(&id),
            WorkspaceEvent::TriggerActionSlot(slot) => {
                let id = self
                    .config
                    .actions
                    .iter()
                    .find(|a| a.launcher_slot == Some(slot))
                    .map(|a| a.id.clone())
                    .ok_or("No action is assigned to this launcher number.")?;
                self.trigger_action(&id)
            }
            WorkspaceEvent::CancelActionCapture => {
                self.action_capture = None;
                self.view.notice = "Action cancelled. Nothing will be sent.".into();
                if self.note_capture
                    && let Some(audio) = &self.audio
                {
                    audio.stop()?;
                    self.view.capture = CaptureState::Stopping;
                }
                Ok(())
            }
            WorkspaceEvent::RetryAnswer => self.retry_answer(),
            WorkspaceEvent::TranscriptPage(offset) => self.transcript_page(offset),
            WorkspaceEvent::SendTranscriptPage => self.send_transcript_page(),
            WorkspaceEvent::CancelAnswer => {
                self.cancel_answer();
                Ok(())
            }
            WorkspaceEvent::SaveProvider(draft) => self.save_provider(draft),
            WorkspaceEvent::SaveAction(draft) => self.save_action(draft),
            WorkspaceEvent::DeleteAction(id) => {
                let mut config = self.config.clone();
                config.actions.retain(|a| a.id != id);
                self.config_store.save(&config).map_err(|e| e.to_string())?;
                self.config = config;
                self.refresh_config();
                Ok(())
            }
            WorkspaceEvent::SendAction { id, text } => self.deliver(id, text),
        }
    }
    fn delete_selected(
        &mut self,
        selection: phorminx_persistence::DeleteSelection,
    ) -> Result<(), String> {
        if selection == phorminx_persistence::DeleteSelection::default() {
            return Err("Select what you want to delete. Nothing was removed.".into());
        }
        if selection.meeting_transcripts || selection.chats {
            if self.audio.is_some() || self.active.is_some() || self.pending.is_some() {
                return Err("Stop the meeting recording and any AI answer before deleting meeting text or chats. Nothing was removed.".into());
            }
            self.flush_persistence();
            if !self.pending_segments.is_empty() {
                return Err("Meeting text is still being saved. Please retry deletion shortly; nothing was removed.".into());
            }
        }
        self.invalidate_meeting_memory();
        self.db.delete_selected(selection).map_err(
            |_| "Selected data could not be deleted. No categories were partially deleted.",
        )?;
        if selection.meeting_transcripts {
            self.view.transcript.clear();
            self.view.provisional_text.clear();
            self.view.total_segments = 0;
            self.view.transcript_offset = 0;
            self.view.captured_samples = 0;
            self.view.committed_samples = 0;
            self.boundaries = MeetingCutoffs::new();
            self.deferred_cutoff = None;
            self.cutoff_question = None;
            self.cutoff_id = 0;
            self.sequence = 0;
            self.view.send_pending = false;
        }
        if selection.chats {
            self.view.messages.clear();
            self.runs.cancel();
        }
        if let Some(id) = self.view.session_id
            && self
                .db
                .meetings()
                .get(id)
                .map_err(|_| "Deletion completed, but the session list could not refresh.")?
                .is_none()
        {
            self.view.session_id = None;
            self.view.title.clear();
            self.loaded_session = false;
            self.view.saved_session = false;
        }
        self.refresh_sessions()?;
        self.view.notice="Selected saved data deleted. Unselected categories, models, settings and credentials were kept.".into();
        Ok(())
    }
    fn new_session(&mut self) -> Result<(), String> {
        if self.audio.is_some() {
            return Err("Stop recording before opening another session.".into());
        }
        self.cancel_answer();
        self.view.session_id = None;
        self.view.title.clear();
        self.view.transcript.clear();
        self.view.messages.clear();
        self.view.captured_samples = 0;
        self.view.committed_samples = 0;
        self.view.send_pending = false;
        self.view.notice.clear();
        self.boundaries = MeetingCutoffs::new();
        self.cutoff_id = 0;
        self.sequence = 0;
        self.loaded_session = false;
        self.view.saved_session = false;
        self.note_capture = false;
        self.view.provisional_text.clear();
        self.deferred_cutoff = None;
        self.cutoff_question = None;
        self.cutoff_request = None;
        self.action_capture = None;
        self.view.capture_action = None;
        self.view.transcript_offset = 0;
        self.view.total_segments = 0;
        Ok(())
    }
    fn start_capture(
        &mut self,
        source: CaptureSource,
        device: Option<String>,
        title: String,
        note: bool,
    ) -> Result<(), String> {
        if let Some(memory) = &self.memory {
            memory.set_paused(true);
        }
        if !note {
            let mut config = self.config.clone();
            config.meeting_microphone = source == CaptureSource::Microphone;
            config.meeting_device = device.clone();
            self.config_store.save(&config).map_err(|e| e.to_string())?;
            self.config = config;
        }
        self.new_session()?;
        let lease = production_workload_coordinator()
            .try_begin(RuntimeActivityKind::Dictation)
            .map_err(
                |_| "Finish the current dictation or calibration before starting a meeting.",
            )?;
        let settings = self
            .store
            .load()
            .map_err(|_| "Recording settings could not be loaded.")?;
        let mut recognition = settings.recognition;
        recognition.model_path = self.store.resolve_model_path(&recognition.model_path);
        recognition.instant_model_path = self
            .store
            .resolve_asset_path(&recognition.instant_model_path);
        recognition.instant_runtime_path = self
            .store
            .resolve_asset_path(&recognition.instant_runtime_path);
        let directory = self
            .store
            .path()
            .parent()
            .ok_or("Settings directory unavailable.")?;
        let service = MeetingAudioService::start(
            recognition,
            match source {
                CaptureSource::SystemAudio => MeetingAudioSource::SystemAudio,
                CaptureSource::Microphone => MeetingAudioSource::Microphone,
            },
            device,
            directory.join("meeting-scratch"),
        )?;
        self.view.title = if title.trim().is_empty() {
            "Untitled session".into()
        } else {
            title.chars().take(250).collect()
        };
        self.note_capture = note;
        if note {
            self.view.note_text.clear();
            self.view.note_generation = self.view.note_generation.saturating_add(1);
        }
        self.view.history_enabled =
            settings.privacy.history_retention != HistoryRetention::Disabled;
        if !note && self.view.history_enabled {
            match self.db.meetings().create(
                &self.view.title,
                if source == CaptureSource::SystemAudio {
                    "system"
                } else {
                    "mic"
                },
                now_ms(),
            ) {
                Ok(id) => self.view.session_id = id,
                Err(_) => {
                    self.view.notice =
                        "Recording continues, but this session could not be saved.".into()
                }
            }
        }
        self.capture_lease = Some(lease);
        self.audio = Some(service);
        self.view.capture = CaptureState::Starting;
        Ok(())
    }
    fn audio_event(&mut self, event: MeetingAudioEvent) -> Result<(), String> {
        match event {
            MeetingAudioEvent::Partial { start_sample, text } => {
                if start_sample >= self.view.committed_samples {
                    self.view.provisional_text = text;
                }
            }
            MeetingAudioEvent::Ready => self.view.capture = CaptureState::Listening,
            MeetingAudioEvent::Warning { message } => self.view.notice = message,
            MeetingAudioEvent::Progress {
                captured_samples,
                committed_samples,
            } => {
                self.view.captured_samples = captured_samples;
                self.view.committed_samples = committed_samples;
            }
            MeetingAudioEvent::Segment {
                start_sample,
                end_sample,
                text,
            } => {
                let fresh = self
                    .boundaries
                    .commit(MeetingSegment {
                        start_sample,
                        end_sample,
                        text: text.clone(),
                    })
                    .map_err(|e| e.to_string())?;
                if !fresh {
                    return Ok(());
                }
                if let Some(id) = self.view.session_id {
                    self.pending_segments.push_back(PendingWrite {
                        session: id,
                        kind: WriteKind::Segment {
                            sequence: self.sequence,
                            segment: MeetingSegment {
                                start_sample,
                                end_sample,
                                text: text.clone(),
                            },
                        },
                    });
                }
                self.sequence += 1;
                self.view.committed_samples = end_sample;
                self.view.provisional_text.clear();
                self.view.total_segments = self.sequence;
                if self.note_capture && !text.trim().is_empty() {
                    if !self.view.note_text.is_empty() {
                        self.view.note_text.push(' ');
                    }
                    self.view.note_text.push_str(text.trim());
                }
                self.view.transcript.push(MeetingLine {
                    start_sample,
                    end_sample,
                    text,
                });
                if self.view.transcript.len() > VISIBLE_SEGMENTS {
                    self.view.transcript.remove(0);
                }
            }
            MeetingAudioEvent::CutoffReached { id, sample } => {
                self.view.send_pending = false;
                self.deferred_cutoff = Some((id, sample));
                if let Err(error) = self.accept_deferred_cutoff() {
                    if let Some(request) = self.cutoff_request.take() {
                        self.view.chat_error_id = request;
                    }
                    return Err(error);
                }
            }
            MeetingAudioEvent::Stopped { sample } => {
                self.view.captured_samples = sample;
                self.view.capture = CaptureState::Idle;
                self.audio = None;
                self.capture_lease = None;
                self.view.provisional_text.clear();
                if let Some(id) = self.view.session_id {
                    self.pending_segments.push_back(PendingWrite {
                        session: id,
                        kind: WriteKind::Finalize,
                    });
                }
                if !self.note_capture {
                    let _ = self.refresh_sessions();
                }
                if let Some(action) = self.action_capture.take() {
                    let text = self.view.note_text.clone();
                    if text.trim().is_empty() {
                        self.view.notice =
                            "No speech was captured. The action was not sent.".into();
                    } else {
                        self.deliver_config(action, text)?;
                    }
                }
            }
            MeetingAudioEvent::Error { message } => {
                self.action_capture = None;
                if let Some(request) = self.cutoff_request.take() {
                    self.view.chat_error_id = request;
                }
                self.audio = None;
                self.capture_lease = None;
                self.view.capture = CaptureState::Idle;
                self.view.send_pending = false;
                self.view.provisional_text.clear();
                if let Some(id) = self.view.session_id {
                    self.pending_segments.push_back(PendingWrite {
                        session: id,
                        kind: WriteKind::Finalize,
                    });
                }
                self.view.notice = message;
            }
        }
        Ok(())
    }
    fn trigger_action(&mut self, id: &str) -> Result<(), String> {
        let action = self
            .config
            .actions
            .iter()
            .find(|a| a.id == id)
            .cloned()
            .ok_or("This action no longer exists.")?;
        self.trigger_config(action)
    }
    fn trigger_config(&mut self, action: ActionConfig) -> Result<(), String> {
        let id = action.id.as_str();
        if self
            .action_capture
            .as_ref()
            .is_some_and(|action| action.id == id)
        {
            if let Some(audio) = &self.audio {
                audio.stop()?;
                self.view.capture = CaptureState::Stopping;
            }
            return Ok(());
        }
        if self.audio.is_some() || self.view.delivery_busy {
            return Err("Finish the current recording or action before starting another.".into());
        }
        action.validate().map_err(|e| e.to_string())?;
        self.start_capture(CaptureSource::Microphone, None, action.name.clone(), true)?;
        self.last_delivery = None;
        self.view.capture_action = Some(action.name.clone());
        self.view.capture_action_endpoint = action.endpoint.clone();
        self.action_capture = Some(action);
        self.view.notice =
            "Recording your action. Stop to send, or Cancel to discard this attempt.".into();
        Ok(())
    }
    fn invalidate_meeting_memory(&mut self) {
        self.search_generation = self.search_generation.saturating_add(1);
        if let Some(memory) = &self.memory {
            memory.refresh();
        }
    }
    fn poll_memory(&mut self) {
        let Some(memory) = &self.memory else {
            return;
        };
        memory.set_paused(self.audio.is_some() || self.view.ai_busy || self.view.delivery_busy);
        if self.last_memory_poll.elapsed() < Duration::from_secs(1) {
            return;
        }
        self.last_memory_poll = Instant::now();
        if let Ok(settings) = self.store.load() {
            memory.configure(crate::meeting_memory::MeetingMemoryConfig {
                title_model: settings.formatting.ollama_model.or_else(|| {
                    (self.config.provider.kind == ProviderKind::Ollama
                        && !self.config.provider.model.is_empty())
                    .then(|| self.config.provider.model.clone())
                }),
                embedding_model: settings.search.embedding_model,
                embedding_identity: None,
                history_enabled: settings.privacy.history_retention != HistoryRetention::Disabled,
            });
        } else {
            memory.set_paused(true);
            return;
        }
        if let Some(update) = memory.try_recv() {
            self.view.search_notice = update.status;
            if update.generation == self.search_generation
                && update.generation != 0
                && self.view.history_enabled
            {
                self.view.sessions = update
                    .hits
                    .into_iter()
                    .map(|s| SavedMeeting {
                        id: s.id,
                        title: s.title,
                        source: s.source,
                    })
                    .collect();
            }
            if update.titles_changed {
                if let Some(id) = self.view.session_id
                    && let Ok(Some(record)) = self.db.meetings().get(id)
                {
                    self.view.title = record.title;
                }
                let _ = self.refresh_sessions();
            }
        }
    }
    fn send_question(&mut self, text: String) -> Result<(), String> {
        if text.trim().is_empty() {
            return Ok(());
        }
        self.preflight_message(&text)?;
        if self.view.send_pending {
            return Err(
                "Finishing the previous question's transcript context. Your draft is retained."
                    .into(),
            );
        }
        if !self.loaded_session
            && !self.note_capture
            && (self.audio.is_some()
                || self.boundaries.committed_sample() > self.boundaries.submitted_sample()
                || self.deferred_cutoff.is_some())
        {
            self.cutoff_question = Some(text);
            self.send_cutoff()
        } else {
            self.submit(text, None)?;
            self.view.chat_revision = self.view.chat_revision.saturating_add(1);
            Ok(())
        }
    }
    fn send_cutoff(&mut self) -> Result<(), String> {
        if self.deferred_cutoff.is_some() {
            return self.accept_deferred_cutoff();
        }
        if self.view.send_pending {
            return Err("Your previous cutoff is still being transcribed.".into());
        }
        if self.loaded_session {
            return Err("For a saved session, ask a question in chat. Start a new session to capture more audio.".into());
        }
        if self.config.provider.model.trim().is_empty() {
            return Err(
                "Choose a chat model in Assistant before sending a transcript portion.".into(),
            );
        }
        self.cancel_answer();
        self.cutoff_id = self
            .cutoff_id
            .checked_add(1)
            .ok_or("Session identifier exhausted.")?;
        let sample = if let Some(audio) = &self.audio {
            audio.request_cutoff(self.cutoff_id)?
        } else {
            self.boundaries.committed_sample()
        };
        self.boundaries
            .request_cutoff(self.cutoff_id, sample)
            .map_err(|e| e.to_string())?;
        self.view.send_pending = true;
        if self.audio.is_none() {
            self.audio_event(MeetingAudioEvent::CutoffReached {
                id: self.cutoff_id,
                sample,
            })?;
        }
        Ok(())
    }
    fn preflight_message(&self, text: &str) -> Result<(), String> {
        if self.loaded_session
            && self
                .store
                .load()
                .map_err(|_| "Saved-session privacy settings could not be verified.")?
                .privacy
                .history_retention
                == HistoryRetention::Disabled
        {
            return Err("Saved meeting access is disabled while history is off.".into());
        }
        self.config.provider.validate().map_err(|e| e.to_string())?;
        if self.config.provider.model.trim().is_empty() {
            return Err(
                "Choose a chat model in Assistant first. The transcript portion remains unsent."
                    .into(),
            );
        }
        if self.config.provider.kind != ProviderKind::Ollama
            && self.config.provider.credential.is_none()
        {
            return Err("Add an API key for the selected provider. Nothing has been sent.".into());
        }
        if text.len() + self.config.preset.len() > CONTEXT_BYTES {
            return Err("This portion exceeds the chat context budget and remains unsent. Ask a shorter question or send a saved transcript page; recording continues.".into());
        }
        Ok(())
    }
    fn accept_deferred_cutoff(&mut self) -> Result<(), String> {
        let Some((id, sample)) = self.deferred_cutoff else {
            return Ok(());
        };
        let preview = self
            .boundaries
            .preview_boundary(id, sample)
            .map_err(|e| e.to_string())?;
        if let Some(question) = self.cutoff_question.clone() {
            let attached = !preview.text.trim().is_empty();
            self.submit_question(
                question,
                attached.then_some(sample),
                preview.text,
                attached.then_some(preview.start_sample),
            )?;
            self.cutoff_question = None;
            self.view.chat_revision = self.view.chat_revision.saturating_add(1);
            if let Some(request) = self.cutoff_request.take() {
                self.view.chat_ack_id = request;
            }
        } else if !preview.text.trim().is_empty() {
            self.submit(preview.text, Some(sample))?;
        } else {
            self.view.notice = "No new speech in that portion. Recording continues.".into();
        }
        self.boundaries
            .boundary_reached(id, sample)
            .map_err(|e| e.to_string())?;
        self.deferred_cutoff = None;
        Ok(())
    }
    fn retry_answer(&mut self) -> Result<(), String> {
        let latest = self
            .view
            .messages
            .iter()
            .rfind(|line| line.role == "You" && matches!(line.status.as_str(), "sent" | "complete"))
            .ok_or("There is no submitted message to retry.")?;
        self.preflight_message(&latest.text)?;
        self.cancel_answer();
        let (messages, omitted) = build_context(&self.config.preset, &self.view.messages);
        let generation = self.runs.begin().ok_or("Answer generation exhausted.")?;
        self.view.messages.push(ChatLine {
            role: "Assistant".into(),
            text: String::new(),
            status: "waiting".into(),
            ..Default::default()
        });
        if self.view.messages.len() > VISIBLE_MESSAGES {
            self.view
                .messages
                .drain(..self.view.messages.len() - VISIBLE_MESSAGES);
        }
        if let Some(id) = self.view.session_id {
            self.pending_segments.push_back(PendingWrite {
                session: id,
                kind: WriteKind::Start(generation),
            });
        }
        self.pending = Some(PendingChat {
            generation,
            provider: self.config.provider.clone(),
            messages,
            session: self.view.session_id,
        });
        self.view.ai_busy = true;
        if omitted {
            self.view.notice =
                "Earlier messages are omitted from this answer's context budget.".into();
        }
        Ok(())
    }
    fn flush_persistence(&mut self) {
        match self.store.load() {
            Ok(settings) if settings.privacy.history_retention == HistoryRetention::Disabled => {
                self.pending_segments.clear();
                self.view.session_id = None;
                self.view.history_enabled = false;
                self.view.sessions.clear();
                return;
            }
            Ok(_) => {}
            Err(_) => {
                self.view.notice =
                    "Saving meeting text is paused because privacy settings could not be verified."
                        .into();
                return;
            }
        }
        let started = Instant::now();
        while let Some(pending) = self.pending_segments.front() {
            let repo = self.db.meetings();
            let result = match &pending.kind {
                WriteKind::Segment { sequence, segment } => repo.append_segment(
                    pending.session,
                    *sequence,
                    segment.start_sample,
                    segment.end_sample,
                    &segment.text,
                    now_ms(),
                ),
                WriteKind::User {
                    cutoff: Some(sample),
                    text,
                    context_start: None,
                    ..
                } => repo
                    .append_user_message(pending.session, *sample, text, now_ms())
                    .map(|id| id.is_some()),
                WriteKind::User {
                    cutoff,
                    text,
                    context,
                    context_start,
                } => repo
                    .append_user_question(
                        pending.session,
                        *cutoff,
                        text,
                        context,
                        *context_start,
                        now_ms(),
                    )
                    .map(|id| id.is_some()),
                WriteKind::Start(generation) => repo
                    .start_assistant(pending.session, *generation, now_ms())
                    .map(|id| id.is_some()),
                WriteKind::Answer {
                    generation,
                    text,
                    status,
                } => repo.update_assistant(pending.session, *generation, text, *status, now_ms()),
                WriteKind::Finalize => repo.finalize(pending.session, now_ms()),
            };
            match result {
                Ok(_) => {
                    self.pending_segments.pop_front();
                }
                Err(_) => {
                    self.view.notice="Recording continues. Saving transcript text is delayed; Phorminx will retry in order.".into();
                    break;
                }
            }
            if started.elapsed() > Duration::from_millis(40) {
                break;
            }
        }
        if let Some(active) = &self.active
            && self.runs.accepts(active.generation)
            && let Some(id) = active.session
            && let Some(line) = self.view.messages.last()
            && line.status == "answering"
        {
            self.queue_answer(
                id,
                active.generation,
                line.text.clone(),
                MeetingMessageStatus::Streaming,
            );
        }
    }
    fn queue_answer(
        &mut self,
        session: i64,
        generation: u64,
        text: String,
        status: MeetingMessageStatus,
    ) {
        if let Some(pending) = self.pending_segments.iter_mut().rev().find(|p| {
            p.session == session
                && matches!(p.kind,WriteKind::Answer{generation:old,..} if old==generation)
        }) {
            pending.kind = WriteKind::Answer {
                generation,
                text,
                status,
            };
        } else {
            self.pending_segments.push_back(PendingWrite {
                session,
                kind: WriteKind::Answer {
                    generation,
                    text,
                    status,
                },
            });
        }
    }
    fn submit(&mut self, text: String, cutoff: Option<u64>) -> Result<(), String> {
        self.submit_question(text, cutoff, String::new(), None)
    }
    fn submit_question(
        &mut self,
        text: String,
        cutoff: Option<u64>,
        context: String,
        context_start: Option<u64>,
    ) -> Result<(), String> {
        if let Some(memory) = &self.memory {
            memory.set_paused(true);
        }
        let context_start = context_start.filter(|_| cutoff.is_some());
        if text.trim().is_empty() {
            return Ok(());
        }
        self.preflight_message(&question_content(&text, &context))?;
        self.cancel_answer();
        if self.view.session_id.is_none()
            && self.view.history_enabled
            && !self.note_capture
            && self.audio.is_none()
            && !self.loaded_session
        {
            self.view.session_id = self
                .db
                .meetings()
                .create("Conversation", "chat", now_ms())
                .map_err(
                    |_| "This conversation could not be saved. Your message has not been sent.",
                )?;
        }
        if let Some(id) = self.view.session_id {
            self.pending_segments.push_back(PendingWrite {
                session: id,
                kind: WriteKind::User {
                    cutoff,
                    text: text.clone(),
                    context: context.clone(),
                    context_start,
                },
            });
        }
        self.view.messages.push(ChatLine {
            role: "You".into(),
            text,
            status: "sent".into(),
            context_text: context,
            context_samples: context_start.zip(cutoff),
        });
        let (messages, omitted) = build_context(&self.config.preset, &self.view.messages);
        let generation = self.runs.begin().ok_or("Answer generation exhausted.")?;
        self.view.messages.push(ChatLine {
            role: "Assistant".into(),
            text: String::new(),
            status: "waiting".into(),
            ..Default::default()
        });
        if self.view.messages.len() > VISIBLE_MESSAGES {
            self.view
                .messages
                .drain(..self.view.messages.len() - VISIBLE_MESSAGES);
        }
        if let Some(id) = self.view.session_id {
            self.pending_segments.push_back(PendingWrite {
                session: id,
                kind: WriteKind::Start(generation),
            });
        }
        self.pending = Some(PendingChat {
            generation,
            provider: self.config.provider.clone(),
            messages,
            session: self.view.session_id,
        });
        self.view.ai_busy = true;
        if omitted {
            self.view.notice =
                "Earlier messages are omitted from this answer's context budget.".into();
        }
        Ok(())
    }
    fn cancel_answer(&mut self) {
        let pending = self.pending.as_ref().map(|p| (p.generation, p.session));
        if let Some((generation, Some(id))) = pending {
            self.queue_answer(
                id,
                generation,
                String::new(),
                MeetingMessageStatus::Cancelled,
            );
        }
        if let Some(active) = &self.active {
            active.cancel.cancel();
        }
        self.pending = None;
        self.runs.cancel();
        self.view.ai_busy = false;
        let mut cancelled = None;
        if let Some(line) = self.view.messages.last_mut()
            && line.role == "Assistant"
            && matches!(line.status.as_str(), "waiting" | "answering")
        {
            line.status = "stopped".into();
            if pending.is_none()
                && let Some(active) = &self.active
                && let Some(id) = active.session
            {
                cancelled = Some((id, active.generation, line.text.clone()));
            }
        }
        if let Some((id, generation, text)) = cancelled {
            self.queue_answer(id, generation, text, MeetingMessageStatus::Cancelled);
        }
    }
    fn start_pending(&mut self) {
        if self.active.is_some() {
            return;
        }
        let Some(waiting) = self.pending.as_ref() else {
            return;
        };
        let coordinator = production_workload_coordinator();
        let compute_lease = match acquire_chat_compute(waiting.provider.kind, &coordinator) {
            Ok(lease) => lease,
            Err(()) => {
                self.view.notice="The local assistant is waiting for other model work to finish. Recording continues.".into();
                return;
            }
        };
        if self.view.notice
            == "The local assistant is waiting for other model work to finish. Recording continues."
        {
            self.view.notice.clear();
        }
        let Some(pending) = self.pending.take() else {
            return;
        };
        let cancel = CancellationToken::default();
        let token = cancel.clone();
        let tx = self.job_tx.clone();
        let generation = pending.generation;
        let session = pending.session;
        let spawned = thread::Builder::new()
            .name("phorminx-meeting-answer".into())
            .spawn(move || {
                // Own the lease until the actual network worker exits, including cancellation.
                let _compute_lease = compute_lease;
                let result = stream_chat(&pending.provider, &pending.messages, &token, |delta| {
                    let _ = tx.send(JobEvent::Delta(generation, delta.into()));
                })
                .map_err(|e| e.to_string());
                let _ = tx.send(JobEvent::End(generation, result));
            });
        self.active = Some(ActiveJob {
            generation,
            cancel,
            session,
        });
        if spawned.is_err() {
            self.job_event(JobEvent::End(
                generation,
                Err("The answer worker could not start.".into()),
            ));
        }
    }
    fn job_event(&mut self, event: JobEvent) {
        match event {
            JobEvent::Delta(generation, text) => {
                if self.runs.accepts(generation)
                    && let Some(line) = self.view.messages.last_mut()
                {
                    line.text.push_str(&text);
                    line.status = "answering".into();
                }
            }
            JobEvent::End(generation, result) => {
                let session = self
                    .active
                    .as_ref()
                    .filter(|a| a.generation == generation)
                    .and_then(|a| a.session);
                if self
                    .active
                    .as_ref()
                    .is_some_and(|a| a.generation == generation)
                {
                    self.active = None;
                }
                if self.runs.finish(generation) {
                    self.view.ai_busy = false;
                    let mut completed = None;
                    if let Some(line) = self.view.messages.last_mut() {
                        line.status = if result.is_ok() { "complete" } else { "failed" }.into();
                        if let Some(id) = session {
                            completed = Some((
                                id,
                                line.text.clone(),
                                if result.is_ok() {
                                    MeetingMessageStatus::Complete
                                } else {
                                    MeetingMessageStatus::Failed
                                },
                            ));
                        }
                    }
                    if let Some((id, text, status)) = completed {
                        self.queue_answer(id, generation, text, status);
                    }
                    if let Err(message) = result {
                        self.view.notice = message;
                    }
                }
            }
            JobEvent::Delivered(result) => {
                self.delivery_cancel = None;
                self.view.delivery_busy = false;
                self.view.notice = match result {
                    Ok(()) => {
                        self.last_delivery = None;
                        "Note delivered.".into()
                    }
                    Err(message) => message,
                };
            }
        }
    }
    fn refresh_sessions(&mut self) -> Result<(), String> {
        let settings = self
            .store
            .load()
            .map_err(|_| "Privacy settings could not be loaded.")?;
        self.view.history_enabled =
            settings.privacy.history_retention != HistoryRetention::Disabled;
        self.view.sessions = if self.view.history_enabled {
            self.db
                .meetings()
                .list(&self.search, None, 100)
                .map_err(|_| "Saved sessions could not be read.")?
                .into_iter()
                .map(|s| SavedMeeting {
                    id: s.id,
                    title: s.title,
                    source: s.source,
                })
                .collect()
        } else {
            vec![]
        };
        Ok(())
    }
    fn open_session(&mut self, id: i64) -> Result<(), String> {
        let settings = self
            .store
            .load()
            .map_err(|_| "Privacy settings could not be loaded.")?;
        if settings.privacy.history_retention == HistoryRetention::Disabled {
            return Err("Saved meeting access is disabled while history is off.".into());
        }
        self.flush_persistence();
        if self
            .pending_segments
            .iter()
            .any(|pending| pending.session == id)
        {
            return Err("This session is still being saved. Please try again shortly.".into());
        }
        self.new_session()?;
        let record = self
            .db
            .meetings()
            .get(id)
            .map_err(|_| "Session could not be read.")?
            .ok_or("This session no longer exists.")?;
        self.view.session_id = Some(id);
        self.view.title = record.title;
        self.view.committed_samples = record.committed_sample;
        self.view.captured_samples = record.committed_sample;
        self.runs.advance_past(record.assistant_generation);
        self.view.total_segments = record.next_sequence;
        self.loaded_session = true;
        self.view.saved_session = true;
        self.transcript_page(record.next_sequence.saturating_sub(100))?;
        {
            let messages = self
                .db
                .meetings()
                .latest_messages(id, 80)
                .map_err(|_| "Chat could not be read.")?;
            for message in messages {
                let attachment = self
                    .db
                    .meetings()
                    .question_context(id, message.id)
                    .map_err(|_| "Saved transcript context could not be read.")?;
                let mut text = message.text;
                if message.text_truncated {
                    text.push_str(
                        "\n[Preview truncated; this message will not be sent as complete context.]",
                    );
                }
                self.view.messages.push(ChatLine {
                    role: if message.role == "user" {
                        "You"
                    } else {
                        "Assistant"
                    }
                    .into(),
                    text,
                    status: if message.text_truncated {
                        "preview"
                    } else {
                        match message.status {
                            MeetingMessageStatus::Complete => "complete",
                            MeetingMessageStatus::Streaming | MeetingMessageStatus::Cancelled => {
                                "stopped"
                            }
                            MeetingMessageStatus::Failed => "failed",
                        }
                    }
                    .into(),
                    context_text: attachment
                        .as_ref()
                        .map(|c| c.text.clone())
                        .unwrap_or_default(),
                    context_samples: attachment.map(|c| (c.start_sample, c.end_sample)),
                });
                if self.view.messages.len() > VISIBLE_MESSAGES {
                    self.view.messages.remove(0);
                }
            }
        }
        self.view.notice="Showing the latest transcript page and messages. Chat uses previously submitted messages; send a transcript page explicitly to include it. Starting recording creates a new session.".into();
        Ok(())
    }
    fn transcript_page(&mut self, offset: u64) -> Result<(), String> {
        if !self.loaded_session || self.audio.is_some() {
            return Err("Transcript paging is available for saved sessions.".into());
        }
        if self
            .store
            .load()
            .map_err(|_| "Privacy settings unavailable.")?
            .privacy
            .history_retention
            == HistoryRetention::Disabled
        {
            return Err("History is off.".into());
        }
        let id = self.view.session_id.ok_or("Open a saved session first.")?;
        let segments = self
            .db
            .meetings()
            .segments(id, offset.checked_sub(1), 100)
            .map_err(|_| "Transcript page could not be read.")?;
        self.view.transcript_offset = offset;
        self.view.transcript = segments
            .into_iter()
            .map(|segment| {
                let mut text: String = segment.text.chars().take(16384).collect();
                if text.len() < segment.text.len() {
                    text.push_str("\n[Display preview truncated]");
                }
                MeetingLine {
                    start_sample: segment.start_sample,
                    end_sample: segment.end_sample,
                    text,
                }
            })
            .collect();
        Ok(())
    }
    fn send_transcript_page(&mut self) -> Result<(), String> {
        if !self.loaded_session || self.audio.is_some() {
            return Err("Open a saved transcript page first.".into());
        }
        let id = self.view.session_id.ok_or("Open a saved session first.")?;
        if self
            .store
            .load()
            .map_err(|_| "Privacy settings unavailable.")?
            .privacy
            .history_retention
            == HistoryRetention::Disabled
        {
            return Err("History is off.".into());
        }
        let segments = self
            .db
            .meetings()
            .segments(id, self.view.transcript_offset.checked_sub(1), 100)
            .map_err(|_| "Transcript page could not be read.")?;
        let mut text = String::from("Selected meeting transcript page:\n");
        for segment in segments {
            if text.len() + segment.text.len() + 1 > CONTEXT_BYTES {
                return Err("This transcript page exceeds the model context budget. Ask a shorter question instead.".into());
            }
            text.push_str(&segment.text);
            text.push('\n');
        }
        self.submit(text, None)
    }
    fn refresh_config(&mut self) {
        self.view.meeting_source = if self.config.meeting_microphone {
            CaptureSource::Microphone
        } else {
            CaptureSource::SystemAudio
        };
        self.view.meeting_device = self.config.meeting_device.clone();
        let provider = &self.config.provider;
        self.view.provider = ProviderDraft {
            kind: match provider.kind {
                ProviderKind::Ollama => ProviderChoice::Ollama,
                ProviderKind::OpenAi => ProviderChoice::OpenAi,
                ProviderKind::Anthropic => ProviderChoice::Anthropic,
            },
            model: provider.model.clone(),
            endpoint: provider.ollama_endpoint.clone(),
            api_key: String::new(),
            has_credential: provider.credential.is_some(),
            clear_credential: false,
            preset: self.config.preset.clone(),
        };
        self.view.actions = self
            .config
            .actions
            .iter()
            .map(|a| ActionDraft {
                id: a.id.clone(),
                name: a.name.clone(),
                endpoint: a.endpoint.clone(),
                method: format!("{}", a.method),
                payload_template: a.payload_template.clone(),
                authorization: String::new(),
                has_credential: a.authorization.is_some(),
                clear_credential: false,
                headers_json: String::new(),
                clear_headers: false,
                header_names: a.headers.iter().map(|header| header.name.clone()).collect(),
                launcher_slot: a.launcher_slot,
                payload_mode: match a.payload_mode {
                    phorminx_assistant::ActionPayloadMode::Template => ActionPayloadMode::Template,
                    phorminx_assistant::ActionPayloadMode::AiJson => ActionPayloadMode::AiJson,
                },
                payload_model: a.payload_model.clone(),
                payload_prompt: a.payload_prompt.clone(),
            })
            .collect();
    }
    fn save_provider(&mut self, draft: ProviderDraft) -> Result<(), String> {
        let kind = match draft.kind {
            ProviderChoice::Ollama => ProviderKind::Ollama,
            ProviderChoice::OpenAi => ProviderKind::OpenAi,
            ProviderChoice::Anthropic => ProviderKind::Anthropic,
        };
        let credential = if draft.clear_credential {
            None
        } else if !draft.api_key.trim().is_empty() {
            Some(ProtectedSecret::protect(draft.api_key.trim()).map_err(|e| e.to_string())?)
        } else if kind == self.config.provider.kind {
            self.config.provider.credential.clone()
        } else {
            None
        };
        let mut config = self.config.clone();
        config.provider = ProviderConfig {
            kind,
            model: draft.model.trim().into(),
            ollama_endpoint: if draft.endpoint.trim().is_empty() {
                "http://127.0.0.1:11434".into()
            } else {
                draft.endpoint.trim().into()
            },
            credential,
            ..ProviderConfig::default()
        };
        config.preset = draft.preset;
        self.config_store.save(&config).map_err(|e| e.to_string())?;
        self.config = config;
        self.refresh_config();
        self.view.provider_revision += 1;
        self.view.notice = "Assistant settings saved. Recording settings are unchanged.".into();
        Ok(())
    }
    fn save_action(&mut self, draft: ActionDraft) -> Result<(), String> {
        let mut config = self.config.clone();
        let existing = config.actions.iter().find(|a| a.id == draft.id);
        let headers = if draft.clear_headers {
            vec![]
        } else if !draft.headers_json.trim().is_empty() {
            if draft.headers_json.len() > 32768 {
                return Err("Custom headers are too large.".into());
            }
            let values: std::collections::BTreeMap<String, String> = serde_json::from_str(
                &draft.headers_json,
            )
            .map_err(|_| "Custom headers must be a JSON object of names and string values.")?;
            values
                .into_iter()
                .map(|(name, value)| {
                    Ok(phorminx_assistant::ActionHeader {
                        name,
                        value: ProtectedSecret::protect(&value).map_err(|e| e.to_string())?,
                    })
                })
                .collect::<Result<Vec<_>, String>>()?
        } else {
            existing
                .filter(|a| a.endpoint == draft.endpoint)
                .map(|a| a.headers.clone())
                .unwrap_or_default()
        };
        let authorization = if draft.clear_credential {
            None
        } else if !draft.authorization.trim().is_empty() {
            Some(ProtectedSecret::protect(draft.authorization.trim()).map_err(|e| e.to_string())?)
        } else {
            existing
                .filter(|a| a.endpoint == draft.endpoint)
                .and_then(|a| a.authorization.clone())
        };
        let method = match draft.method.as_str() {
            "GET" => ActionMethod::Get,
            "POST" => ActionMethod::Post,
            "PUT" => ActionMethod::Put,
            "PATCH" => ActionMethod::Patch,
            "DELETE" => ActionMethod::Delete,
            _ => return Err("Choose GET, POST, PUT, PATCH or DELETE.".into()),
        };
        let action = ActionConfig {
            id: if draft.id.is_empty() {
                fresh_workspace_id("action")
            } else {
                draft.id
            },
            name: draft.name,
            endpoint: draft.endpoint,
            method,
            headers,
            payload_template: draft.payload_template,
            authorization,
            launcher_slot: draft.launcher_slot,
            payload_mode: match draft.payload_mode {
                ActionPayloadMode::Template => phorminx_assistant::ActionPayloadMode::Template,
                ActionPayloadMode::AiJson => phorminx_assistant::ActionPayloadMode::AiJson,
            },
            payload_model: draft.payload_model,
            payload_prompt: draft.payload_prompt,
        };
        let id = action.id.clone();
        config.actions.retain(|a| a.id != action.id);
        config.actions.push(action);
        self.config_store.save(&config).map_err(|e| e.to_string())?;
        self.config = config;
        self.refresh_config();
        self.view.action_revision += 1;
        self.view.last_saved_action = id;
        self.view.notice = "Action saved. Nothing has been sent.".into();
        Ok(())
    }
    fn deliver(&mut self, id: String, text: String) -> Result<(), String> {
        if self.delivery_cancel.is_some() {
            return Err("A delivery is already in progress.".into());
        }
        let action = self
            .config
            .actions
            .iter()
            .find(|a| a.id == id)
            .cloned()
            .ok_or("Select a saved action.")?;
        self.deliver_config(action, text)
    }
    fn deliver_config(&mut self, action: ActionConfig, text: String) -> Result<(), String> {
        if self.delivery_cancel.is_some() {
            return Err("A delivery is already in progress.".into());
        }
        action.validate().map_err(|e| e.to_string())?;
        if text.trim().is_empty() {
            return Err("Write or dictate a note before sending.".into());
        }
        if text.len() > 512 * 1024 {
            return Err("This note is too large for one delivery. Send a smaller portion.".into());
        }
        let cancel = CancellationToken::default();
        let token = cancel.clone();
        let tx = self.job_tx.clone();
        let delivery_id = delivery_identity(self.last_delivery.as_ref(), &action, &text);
        let prepared = self
            .last_delivery
            .as_ref()
            .filter(|attempt| attempt.matches(&action, &text))
            .map(|attempt| Arc::clone(&attempt.prepared))
            .unwrap_or_default();
        let attempt = DeliveryAttempt {
            action: action.clone(),
            text: text.clone(),
            id: delivery_id.clone(),
            prepared: Arc::clone(&prepared),
        };
        let endpoint = self.config.provider.ollama_endpoint.clone();
        thread::Builder::new()
            .name("phorminx-note-delivery".into())
            .spawn(move || {
                let result = (|| -> Result<(), String> {
                    let cached = prepared.lock().map_err(|_| "Prepared action unavailable.")?.clone();
                    let ready = if let Some(ready) = cached { ready } else {
                        let _lease = if action.payload_mode == phorminx_assistant::ActionPayloadMode::AiJson {
                            acquire_chat_compute(ProviderKind::Ollama, &production_workload_coordinator())
                                .map_err(|_| "The local model is busy. Your note is retained; retry when it is idle.")?
                        } else { None };
                        let ready = prepare_action(&action, &text, &delivery_id, &endpoint, &token).map_err(|e| e.to_string())?;
                        *prepared.lock().map_err(|_| "Prepared action unavailable.")? = Some(ready.clone());
                        ready
                    };
                    execute_prepared_action(&ready, &token).map(|_| ()).map_err(|e| e.to_string())
                })();
                let _ = tx.send(JobEvent::Delivered(result));
            })
            .map_err(|_| "Delivery worker could not start.")?;
        self.last_delivery = Some(attempt);
        self.delivery_cancel = Some(cancel);
        self.view.delivery_busy = true;
        Ok(())
    }
}
fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}
fn fresh_workspace_id(prefix: &str) -> String {
    static NEXT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let sequence = NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{prefix}-{timestamp}-{}-{sequence}", std::process::id())
}
fn delivery_identity(
    previous: Option<&DeliveryAttempt>,
    action: &ActionConfig,
    text: &str,
) -> String {
    previous
        .filter(|attempt| attempt.matches(action, text))
        .map(|attempt| attempt.id.clone())
        .unwrap_or_else(|| fresh_workspace_id("phorminx"))
}
fn acquire_chat_compute(
    kind: ProviderKind,
    coordinator: &crate::performance_runtime::WorkloadCoordinator,
) -> Result<Option<RuntimeActivityLease>, ()> {
    if kind == ProviderKind::Ollama {
        coordinator
            .try_begin(RuntimeActivityKind::Ollama)
            .map(Some)
            .map_err(|_| ())
    } else {
        Ok(None)
    }
}
fn build_context(preset: &str, lines: &[ChatLine]) -> (Vec<ChatMessage>, bool) {
    let mut used = preset.len();
    let mut messages = Vec::new();
    let mut omitted = false;
    for line in lines.iter().rev() {
        if !matches!(line.status.as_str(), "sent" | "complete") || line.text.is_empty() {
            omitted |= line.status == "preview";
            continue;
        }
        let content = question_content(&line.text, &line.context_text);
        if used + content.len() > CONTEXT_BYTES {
            omitted = true;
            break;
        }
        used += content.len();
        messages.push(ChatMessage {
            role: if line.role == "You" {
                ChatRole::User
            } else {
                ChatRole::Assistant
            },
            content,
        });
    }
    messages.push(ChatMessage {
        role: ChatRole::System,
        content: preset.to_owned(),
    });
    messages.reverse();
    (messages, omitted)
}
fn question_content(question: &str, context: &str) -> String {
    if context.is_empty() {
        question.to_owned()
    } else {
        serde_json::json!({"meeting_transcript": context, "question": question}).to_string()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn context_excludes_cancelled_failed_and_truncated_answers() {
        let lines = vec![
            ChatLine {
                role: "You".into(),
                text: "question".into(),
                status: "sent".into(),
                ..Default::default()
            },
            ChatLine {
                role: "Assistant".into(),
                text: "partial".into(),
                status: "stopped".into(),
                ..Default::default()
            },
            ChatLine {
                role: "You".into(),
                text: "preview".into(),
                status: "preview".into(),
                ..Default::default()
            },
        ];
        let (messages, omitted) = build_context("instructions", &lines);
        assert!(omitted);
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[1].content, "question");
    }
    #[test]
    fn context_omits_whole_old_messages_not_partial_text() {
        let lines = vec![
            ChatLine {
                role: "You".into(),
                text: "a".repeat(CONTEXT_BYTES),
                status: "sent".into(),
                ..Default::default()
            },
            ChatLine {
                role: "You".into(),
                text: "new".into(),
                status: "sent".into(),
                ..Default::default()
            },
        ];
        let (messages, omitted) = build_context("", &lines);
        assert!(omitted);
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[1].content, "new");
    }
}
