use std::time::Duration;

use phorminx_core::{
    AudioClip, DictationId, RuntimeState, RuntimeStateMachine, StateError, Transcript,
    normalize_transcript, recommended_audio_context,
};

use crate::settings::RuntimeFormatting;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UiStatus {
    Ready,
    Listening,
    Transcribing,
    Cleaning,
    Inserted,
    ClipboardReady,
    NoSpeech,
    Error,
}

#[derive(Clone, Debug)]
pub struct FinishedAudio {
    pub clip: AudioClip,
    pub backend_warning_count: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InsertDisposition<R> {
    Pasted,
    ClipboardOnly(R),
}

pub trait AppIo {
    type Target;
    type Recording;
    type ClipboardReason;

    fn start_recording(&mut self) -> Result<Self::Recording, String>;
    fn finish_recording(&mut self, recording: Self::Recording) -> Result<FinishedAudio, String>;
    fn submit_transcription(
        &mut self,
        id: DictationId,
        clip: AudioClip,
        language: &str,
        audio_context: u32,
    ) -> Result<(), String>;
    fn insert(
        &mut self,
        target: Option<Self::Target>,
        text: &str,
    ) -> Result<InsertDisposition<Self::ClipboardReason>, String>;
    fn show_status(&mut self, status: UiStatus) -> Result<(), String>;
}

#[derive(Debug)]
pub enum RuntimeNotice<R> {
    BusyRejected {
        id: Option<DictationId>,
        state: RuntimeState,
    },
    RecordingStarted {
        id: DictationId,
    },
    RecordingStopped {
        id: DictationId,
    },
    AudioBackendWarning {
        id: DictationId,
        state: RuntimeState,
        count: u64,
    },
    NoSpeech {
        id: DictationId,
        event: &'static str,
    },
    TranscriptionStarted {
        id: DictationId,
    },
    CleanupStarted {
        id: DictationId,
    },
    RecoveredAfterResume {
        cancelled_id: Option<DictationId>,
    },
    StaleTranscription {
        id: DictationId,
        state: RuntimeState,
    },
    Inserted {
        id: DictationId,
        inference_time: Duration,
    },
    ClipboardReady {
        id: DictationId,
        reason: R,
    },
    Failure {
        id: Option<DictationId>,
        event: &'static str,
        message: String,
    },
    StatusUpdateFailed {
        message: String,
    },
}

pub struct AppRuntime<Target, Recording> {
    machine: RuntimeStateMachine,
    recording: Option<Recording>,
    target: Option<Target>,
    pending_id: Option<DictationId>,
    minimum_rms: f32,
    language: String,
    formatting: RuntimeFormatting,
}

impl<Target, Recording> AppRuntime<Target, Recording> {
    pub fn new(minimum_rms: f32, language: String) -> Result<Self, StateError> {
        Self::new_with_formatting(minimum_rms, language, RuntimeFormatting::Light)
    }

    pub fn new_with_formatting(
        minimum_rms: f32,
        language: String,
        formatting: RuntimeFormatting,
    ) -> Result<Self, StateError> {
        let mut machine = RuntimeStateMachine::default();
        machine.mark_ready()?;
        Ok(Self {
            machine,
            recording: None,
            target: None,
            pending_id: None,
            minimum_rms,
            language,
            formatting,
        })
    }

    pub fn state(&self) -> RuntimeState {
        self.machine.state()
    }

    pub fn active_id(&self) -> Option<DictationId> {
        self.machine.active_id()
    }

    pub fn pending_id(&self) -> Option<DictationId> {
        self.pending_id
    }

    /// Read-only access used by the event-loop incremental scheduler. The
    /// recording remains owned by the runtime and can still be finalized or
    /// dropped through the existing state-machine paths.
    pub fn active_recording(&self) -> Option<&Recording> {
        self.recording.as_ref()
    }

    pub fn active_recording_mut(&mut self) -> Option<&mut Recording> {
        self.recording.as_mut()
    }

    pub fn is_clean_idle(&self) -> bool {
        self.machine.state() == RuntimeState::Idle
            && self.machine.active_id().is_none()
            && self.recording.is_none()
            && self.target.is_none()
            && self.pending_id.is_none()
    }

    pub fn configure_next_dictation(
        &mut self,
        language: String,
        formatting: RuntimeFormatting,
    ) -> Result<(), StateError> {
        if self.machine.state() != RuntimeState::Idle {
            return Err(StateError::Busy(self.machine.state()));
        }
        self.language = language;
        self.formatting = formatting;
        Ok(())
    }

    pub fn announce_ready<I>(&mut self, io: &mut I) -> Vec<RuntimeNotice<I::ClipboardReason>>
    where
        I: AppIo<Target = Target, Recording = Recording>,
    {
        let mut notices = Vec::new();
        Self::show_status(io, UiStatus::Ready, &mut notices);
        notices
    }

    pub fn hold_started<I>(
        &mut self,
        target: Option<Target>,
        io: &mut I,
    ) -> Result<Vec<RuntimeNotice<I::ClipboardReason>>, StateError>
    where
        I: AppIo<Target = Target, Recording = Recording>,
    {
        let mut notices = Vec::new();
        if self.machine.state() != RuntimeState::Idle {
            notices.push(RuntimeNotice::BusyRejected {
                id: self.machine.active_id(),
                state: self.machine.state(),
            });
            return Ok(notices);
        }

        let id = self.machine.begin_dictation()?;
        match io.start_recording() {
            Ok(recording) => {
                self.recording = Some(recording);
                self.target = target;
                Self::show_status(io, UiStatus::Listening, &mut notices);
                notices.push(RuntimeNotice::RecordingStarted { id });
            }
            Err(message) => {
                self.reset_after_fault()?;
                Self::show_status(io, UiStatus::Error, &mut notices);
                notices.push(RuntimeNotice::Failure {
                    id: Some(id),
                    event: "audio_start_failed",
                    message,
                });
            }
        }
        self.debug_assert_invariants();
        Ok(notices)
    }

    pub fn hold_ended<I>(
        &mut self,
        io: &mut I,
    ) -> Result<Vec<RuntimeNotice<I::ClipboardReason>>, StateError>
    where
        I: AppIo<Target = Target, Recording = Recording>,
    {
        let mut notices = Vec::new();
        if self.machine.state() != RuntimeState::Listening {
            return Ok(notices);
        }

        let id = self
            .machine
            .active_id()
            .expect("listening state must have a dictation id");
        self.machine.transition(RuntimeState::FinalizingAudio)?;
        notices.push(RuntimeNotice::RecordingStopped { id });

        let Some(recording) = self.recording.take() else {
            self.reset_after_fault()?;
            Self::show_status(io, UiStatus::Error, &mut notices);
            notices.push(RuntimeNotice::Failure {
                id: Some(id),
                event: "audio_finish_failed",
                message: "active recording was missing".to_owned(),
            });
            return Ok(notices);
        };

        let captured = match io.finish_recording(recording) {
            Ok(captured) => captured,
            Err(message) => {
                self.reset_after_fault()?;
                Self::show_status(io, UiStatus::Error, &mut notices);
                notices.push(RuntimeNotice::Failure {
                    id: Some(id),
                    event: "audio_finish_failed",
                    message,
                });
                self.debug_assert_invariants();
                return Ok(notices);
            }
        };

        if captured.backend_warning_count != 0 {
            notices.push(RuntimeNotice::AudioBackendWarning {
                id,
                state: self.machine.state(),
                count: captured.backend_warning_count,
            });
        }

        if captured.clip.duration() < Duration::from_millis(200)
            || captured.clip.rms() < self.minimum_rms
        {
            self.machine.cancel()?;
            notices.push(RuntimeNotice::NoSpeech {
                id,
                event: "silence_rejected",
            });
            self.clear_owned_state();
            self.machine.transition(RuntimeState::Idle)?;
            Self::show_status(io, UiStatus::NoSpeech, &mut notices);
            self.debug_assert_invariants();
            return Ok(notices);
        }

        let audio_context = recommended_audio_context(captured.clip.duration());
        self.machine.transition(RuntimeState::Transcribing)?;
        Self::show_status(io, UiStatus::Transcribing, &mut notices);
        match io.submit_transcription(id, captured.clip, &self.language, audio_context) {
            Ok(()) => {
                self.pending_id = Some(id);
                notices.push(RuntimeNotice::TranscriptionStarted { id });
            }
            Err(message) => {
                self.reset_after_fault()?;
                Self::show_status(io, UiStatus::Error, &mut notices);
                notices.push(RuntimeNotice::Failure {
                    id: Some(id),
                    event: "transcription_submit_failed",
                    message,
                });
            }
        }
        self.debug_assert_invariants();
        Ok(notices)
    }

    pub fn transcription_completed<I>(
        &mut self,
        id: DictationId,
        result: Result<Transcript, String>,
        io: &mut I,
    ) -> Result<Vec<RuntimeNotice<I::ClipboardReason>>, StateError>
    where
        I: AppIo<Target = Target, Recording = Recording>,
    {
        let mut notices = Vec::new();
        if self.pending_id != Some(id)
            || !matches!(
                self.machine.state(),
                RuntimeState::Transcribing | RuntimeState::Cleaning
            )
        {
            notices.push(RuntimeNotice::StaleTranscription {
                id,
                state: self.machine.state(),
            });
            return Ok(notices);
        }
        self.pending_id = None;

        let transcript = match result {
            Ok(transcript) => transcript,
            Err(message) => {
                self.reset_after_fault()?;
                Self::show_status(io, UiStatus::Error, &mut notices);
                notices.push(RuntimeNotice::Failure {
                    id: Some(id),
                    event: "transcription_failed",
                    message,
                });
                self.debug_assert_invariants();
                return Ok(notices);
            }
        };

        if self.machine.state() == RuntimeState::Transcribing {
            self.machine.transition(RuntimeState::Normalizing)?;
        }
        let normalized = match self.formatting {
            RuntimeFormatting::Raw => transcript.text,
            RuntimeFormatting::Light => normalize_transcript(&transcript.text),
            RuntimeFormatting::Balanced | RuntimeFormatting::Strong | RuntimeFormatting::Custom => {
                if self.machine.state() == RuntimeState::Normalizing {
                    self.machine.transition(RuntimeState::Cleaning)?;
                    Self::show_status(io, UiStatus::Cleaning, &mut notices);
                }
                normalize_transcript(&transcript.text)
            }
        };
        if normalized.is_empty() {
            self.machine.cancel()?;
            notices.push(RuntimeNotice::NoSpeech {
                id,
                event: "empty_transcript_rejected",
            });
            self.clear_owned_state();
            self.machine.transition(RuntimeState::Idle)?;
            Self::show_status(io, UiStatus::NoSpeech, &mut notices);
            self.debug_assert_invariants();
            return Ok(notices);
        }

        self.machine.transition(RuntimeState::ReadyToInsert)?;
        self.machine.transition(RuntimeState::Inserting)?;
        match io.insert(self.target.take(), &normalized) {
            Ok(InsertDisposition::Pasted) => {
                notices.push(RuntimeNotice::Inserted {
                    id,
                    inference_time: transcript.inference_time,
                });
                Self::show_status(io, UiStatus::Inserted, &mut notices);
                self.machine.transition(RuntimeState::Idle)?;
            }
            Ok(InsertDisposition::ClipboardOnly(reason)) => {
                notices.push(RuntimeNotice::ClipboardReady { id, reason });
                Self::show_status(io, UiStatus::ClipboardReady, &mut notices);
                self.machine.transition(RuntimeState::Idle)?;
            }
            Err(message) => {
                self.reset_after_fault()?;
                Self::show_status(io, UiStatus::Error, &mut notices);
                notices.push(RuntimeNotice::Failure {
                    id: Some(id),
                    event: "insertion_failed",
                    message,
                });
            }
        }
        self.debug_assert_invariants();
        Ok(notices)
    }

    pub fn cleanup_started<I>(
        &mut self,
        id: DictationId,
        io: &mut I,
    ) -> Result<Vec<RuntimeNotice<I::ClipboardReason>>, StateError>
    where
        I: AppIo<Target = Target, Recording = Recording>,
    {
        let mut notices = Vec::new();
        if self.pending_id != Some(id) || self.machine.state() != RuntimeState::Transcribing {
            notices.push(RuntimeNotice::StaleTranscription {
                id,
                state: self.machine.state(),
            });
            return Ok(notices);
        }
        self.machine.transition(RuntimeState::Normalizing)?;
        self.machine.transition(RuntimeState::Cleaning)?;
        Self::show_status(io, UiStatus::Cleaning, &mut notices);
        notices.push(RuntimeNotice::CleanupStarted { id });
        Ok(notices)
    }

    pub fn worker_disconnected<I>(
        &mut self,
        io: &mut I,
    ) -> Result<Vec<RuntimeNotice<I::ClipboardReason>>, StateError>
    where
        I: AppIo<Target = Target, Recording = Recording>,
    {
        let mut notices = Vec::new();
        let id = self.machine.active_id();
        if self.machine.state() != RuntimeState::Idle {
            self.reset_after_fault()?;
        } else {
            self.clear_owned_state();
        }
        Self::show_status(io, UiStatus::Error, &mut notices);
        notices.push(RuntimeNotice::Failure {
            id,
            event: "transcription_worker_disconnected",
            message: "the transcription worker stopped unexpectedly".to_owned(),
        });
        self.debug_assert_invariants();
        Ok(notices)
    }

    pub fn recover_after_system_resume<I>(
        &mut self,
        io: &mut I,
    ) -> Result<Vec<RuntimeNotice<I::ClipboardReason>>, StateError>
    where
        I: AppIo<Target = Target, Recording = Recording>,
    {
        let cancelled_id = self.machine.active_id();
        if self.machine.state() != RuntimeState::Idle {
            self.machine.cancel()?;
            self.clear_owned_state();
            self.machine.transition(RuntimeState::Idle)?;
        } else {
            self.clear_owned_state();
        }
        let mut notices = vec![RuntimeNotice::RecoveredAfterResume { cancelled_id }];
        Self::show_status(io, UiStatus::Ready, &mut notices);
        self.debug_assert_invariants();
        Ok(notices)
    }

    fn reset_after_fault(&mut self) -> Result<(), StateError> {
        self.clear_owned_state();
        self.machine.fault()?;
        self.machine.transition(RuntimeState::Idle)
    }

    fn clear_owned_state(&mut self) {
        self.recording = None;
        self.target = None;
        self.pending_id = None;
    }

    fn show_status<I>(
        io: &mut I,
        status: UiStatus,
        notices: &mut Vec<RuntimeNotice<I::ClipboardReason>>,
    ) where
        I: AppIo<Target = Target, Recording = Recording>,
    {
        if let Err(message) = io.show_status(status) {
            notices.push(RuntimeNotice::StatusUpdateFailed { message });
        }
    }

    fn debug_assert_invariants(&self) {
        debug_assert!(match self.machine.state() {
            RuntimeState::Idle =>
                self.recording.is_none()
                    && self.target.is_none()
                    && self.pending_id.is_none()
                    && self.machine.active_id().is_none(),
            RuntimeState::Listening => {
                self.recording.is_some()
                    && self.pending_id.is_none()
                    && self.machine.active_id().is_some()
            }
            RuntimeState::Transcribing => {
                self.recording.is_none()
                    && self.pending_id == self.machine.active_id()
                    && self.machine.active_id().is_some()
            }
            _ => true,
        });
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum FakeReason {
        TargetUnavailable,
        TargetChanged,
    }

    #[derive(Clone, Copy)]
    enum FinishPlan {
        Speech,
        Short,
        Quiet,
        Failure,
    }

    struct FakeRecording {
        token: u64,
        drops: Arc<AtomicUsize>,
    }

    impl Drop for FakeRecording {
        fn drop(&mut self) {
            self.drops.fetch_add(1, Ordering::Relaxed);
        }
    }

    struct FakeIo {
        next_token: u64,
        start_fails: bool,
        finish_plan: FinishPlan,
        submit_fails: bool,
        insertion_fails: bool,
        status_fails: bool,
        ambient_target: Option<u64>,
        starts: usize,
        insertions: usize,
        pasted_targets: Vec<u64>,
        inserted_texts: Vec<String>,
        submitted: VecDeque<DictationId>,
        max_outstanding: usize,
        statuses: Vec<UiStatus>,
        recording_drops: Arc<AtomicUsize>,
    }

    impl Default for FakeIo {
        fn default() -> Self {
            Self {
                next_token: 0,
                start_fails: false,
                finish_plan: FinishPlan::Speech,
                submit_fails: false,
                insertion_fails: false,
                status_fails: false,
                ambient_target: None,
                starts: 0,
                insertions: 0,
                pasted_targets: Vec::new(),
                inserted_texts: Vec::new(),
                submitted: VecDeque::new(),
                max_outstanding: 0,
                statuses: Vec::new(),
                recording_drops: Arc::new(AtomicUsize::new(0)),
            }
        }
    }

    impl FakeIo {
        fn reset_plan(&mut self, cycle: u64) {
            self.next_token = cycle;
            self.start_fails = false;
            self.finish_plan = FinishPlan::Speech;
            self.submit_fails = false;
            self.insertion_fails = false;
            self.status_fails = false;
            self.ambient_target = Some(cycle);
        }

        fn complete_submission(&mut self, expected: DictationId) {
            assert_eq!(self.submitted.pop_front(), Some(expected));
        }
    }

    impl AppIo for FakeIo {
        type Target = u64;
        type Recording = FakeRecording;
        type ClipboardReason = FakeReason;

        fn start_recording(&mut self) -> Result<Self::Recording, String> {
            self.starts += 1;
            if self.start_fails {
                return Err("start failed".to_owned());
            }
            Ok(FakeRecording {
                token: self.next_token,
                drops: Arc::clone(&self.recording_drops),
            })
        }

        fn finish_recording(
            &mut self,
            recording: Self::Recording,
        ) -> Result<FinishedAudio, String> {
            assert_eq!(recording.token, self.next_token);
            match self.finish_plan {
                FinishPlan::Speech => Ok(FinishedAudio {
                    clip: clip(3_200, 0.1),
                    backend_warning_count: 0,
                }),
                FinishPlan::Short => Ok(FinishedAudio {
                    clip: clip(1_600, 0.1),
                    backend_warning_count: 0,
                }),
                FinishPlan::Quiet => Ok(FinishedAudio {
                    clip: clip(3_200, 0.000_1),
                    backend_warning_count: 0,
                }),
                FinishPlan::Failure => Err("finish failed".to_owned()),
            }
        }

        fn submit_transcription(
            &mut self,
            id: DictationId,
            _clip: AudioClip,
            _language: &str,
            _audio_context: u32,
        ) -> Result<(), String> {
            if self.submit_fails {
                return Err("submit failed".to_owned());
            }
            self.submitted.push_back(id);
            self.max_outstanding = self.max_outstanding.max(self.submitted.len());
            Ok(())
        }

        fn insert(
            &mut self,
            target: Option<Self::Target>,
            text: &str,
        ) -> Result<InsertDisposition<Self::ClipboardReason>, String> {
            self.insertions += 1;
            self.inserted_texts.push(text.to_owned());
            if self.insertion_fails {
                return Err("insert failed".to_owned());
            }
            let Some(target) = target else {
                return Ok(InsertDisposition::ClipboardOnly(
                    FakeReason::TargetUnavailable,
                ));
            };
            if self.ambient_target != Some(target) {
                return Ok(InsertDisposition::ClipboardOnly(FakeReason::TargetChanged));
            }
            self.pasted_targets.push(target);
            Ok(InsertDisposition::Pasted)
        }

        fn show_status(&mut self, status: UiStatus) -> Result<(), String> {
            self.statuses.push(status);
            if self.status_fails {
                Err("status failed".to_owned())
            } else {
                Ok(())
            }
        }
    }

    fn clip(len: usize, amplitude: f32) -> AudioClip {
        AudioClip::new(vec![amplitude; len], 16_000).unwrap()
    }

    fn transcript(text: &str) -> Transcript {
        Transcript {
            text: text.to_owned(),
            backend: "fake",
            model_load_time: Duration::ZERO,
            inference_time: Duration::from_millis(10),
            audio_duration: Duration::from_millis(200),
        }
    }

    fn notice_id<R>(notices: &[RuntimeNotice<R>]) -> DictationId {
        notices
            .iter()
            .find_map(|notice| match notice {
                RuntimeNotice::RecordingStarted { id }
                | RuntimeNotice::Failure { id: Some(id), .. } => Some(*id),
                _ => None,
            })
            .expect("activation notice should carry an id")
    }

    fn advance_to_transcribing(
        runtime: &mut AppRuntime<u64, FakeRecording>,
        io: &mut FakeIo,
        target: u64,
    ) -> DictationId {
        io.reset_plan(target);
        let notices = runtime.hold_started(Some(target), io).unwrap();
        let id = notice_id(&notices);
        runtime.hold_ended(io).unwrap();
        assert_eq!(runtime.state(), RuntimeState::Transcribing);
        io.complete_submission(id);
        id
    }

    #[test]
    fn soaks_500_mixed_cycles_without_stale_state_or_wrong_target_insertion() {
        let mut runtime = AppRuntime::<u64, FakeRecording>::new(0.003, "en".to_owned()).unwrap();
        let mut io = FakeIo::default();

        for cycle in 0_u64..500 {
            io.reset_plan(cycle);
            let mode = cycle % 10;
            match mode {
                3 => io.finish_plan = FinishPlan::Short,
                4 => io.finish_plan = FinishPlan::Quiet,
                7 => io.insertion_fails = true,
                8 => io.finish_plan = FinishPlan::Failure,
                9 => io.start_fails = true,
                _ => {}
            }
            let target = (mode != 2).then_some(cycle);
            let notices = runtime.hold_started(target, &mut io).unwrap();
            let id = notice_id(&notices);
            assert_eq!(id, DictationId(cycle + 1));

            if mode == 9 {
                assert!(runtime.is_clean_idle());
                continue;
            }

            let starts_before_busy = io.starts;
            runtime.hold_started(Some(99_999), &mut io).unwrap();
            assert_eq!(io.starts, starts_before_busy);
            assert_eq!(runtime.active_id(), Some(id));

            runtime.hold_ended(&mut io).unwrap();
            if matches!(mode, 3 | 4 | 8) {
                assert!(runtime.is_clean_idle());
                continue;
            }

            assert_eq!(runtime.state(), RuntimeState::Transcribing);
            let starts_before_busy = io.starts;
            runtime.hold_started(Some(88_888), &mut io).unwrap();
            assert_eq!(io.starts, starts_before_busy);
            runtime
                .transcription_completed(
                    DictationId(10_000 + cycle),
                    Ok(transcript("stale")),
                    &mut io,
                )
                .unwrap();
            assert_eq!(runtime.pending_id(), Some(id));
            io.complete_submission(id);

            if mode == 1 {
                io.ambient_target = Some(1_000 + cycle);
            }
            let result = match mode {
                5 => Err("recognition failed".to_owned()),
                6 => Ok(transcript("  \t\n ")),
                _ => Ok(transcript("hello world")),
            };
            runtime
                .transcription_completed(id, result, &mut io)
                .unwrap();
            assert!(runtime.is_clean_idle(), "cycle {cycle}, mode {mode}");
        }

        assert_eq!(io.starts, 500);
        assert_eq!(io.insertions, 200);
        assert_eq!(io.pasted_targets.len(), 50);
        assert!(io.pasted_targets.iter().all(|target| target % 10 == 0));
        assert!(io.submitted.is_empty());
        assert_eq!(io.max_outstanding, 1);

        io.reset_plan(500);
        let notices = runtime.hold_started(Some(500), &mut io).unwrap();
        let id = notice_id(&notices);
        assert_eq!(id, DictationId(501));
        runtime.hold_ended(&mut io).unwrap();
        io.complete_submission(id);
        runtime
            .transcription_completed(id, Ok(transcript("recovered")), &mut io)
            .unwrap();
        assert!(runtime.is_clean_idle());
        assert_eq!(io.pasted_targets.last(), Some(&500));
    }

    #[test]
    fn submit_failure_and_worker_disconnect_reset_and_recover() {
        let mut runtime = AppRuntime::<u64, FakeRecording>::new(0.003, "en".to_owned()).unwrap();
        let mut io = FakeIo::default();
        io.reset_plan(1);
        io.submit_fails = true;
        runtime.hold_started(Some(1), &mut io).unwrap();
        runtime.hold_ended(&mut io).unwrap();
        assert!(runtime.is_clean_idle());

        io.reset_plan(2);
        runtime.hold_started(Some(2), &mut io).unwrap();
        runtime.hold_ended(&mut io).unwrap();
        assert_eq!(runtime.state(), RuntimeState::Transcribing);
        runtime.worker_disconnected(&mut io).unwrap();
        io.submitted.clear();
        assert!(runtime.is_clean_idle());

        io.reset_plan(3);
        let notices = runtime.hold_started(Some(3), &mut io).unwrap();
        let id = notice_id(&notices);
        runtime.hold_ended(&mut io).unwrap();
        io.complete_submission(id);
        runtime
            .transcription_completed(id, Ok(transcript("recovered")), &mut io)
            .unwrap();
        assert!(runtime.is_clean_idle());
    }

    #[test]
    fn system_resume_cancels_owned_work_and_discards_late_completion() {
        let drops = Arc::new(AtomicUsize::new(0));
        let mut io = FakeIo {
            recording_drops: Arc::clone(&drops),
            ..FakeIo::default()
        };
        let mut runtime = AppRuntime::<u64, FakeRecording>::new(0.003, "en".to_owned()).unwrap();

        io.reset_plan(1);
        let notices = runtime.hold_started(Some(1), &mut io).unwrap();
        let listening_id = notice_id(&notices);
        let notices = runtime.recover_after_system_resume(&mut io).unwrap();
        assert!(runtime.is_clean_idle());
        assert_eq!(drops.load(Ordering::Relaxed), 1);
        assert!(notices.iter().any(|notice| matches!(
            notice,
            RuntimeNotice::RecoveredAfterResume {
                cancelled_id: Some(id)
            } if *id == listening_id
        )));

        let transcribing_id = advance_to_transcribing(&mut runtime, &mut io, 2);
        runtime.recover_after_system_resume(&mut io).unwrap();
        let notices = runtime
            .transcription_completed(
                transcribing_id,
                Ok(transcript("must not be inserted")),
                &mut io,
            )
            .unwrap();
        assert!(runtime.is_clean_idle());
        assert!(io.inserted_texts.is_empty());
        assert!(notices.iter().any(|notice| matches!(
            notice,
            RuntimeNotice::StaleTranscription { id, state: RuntimeState::Idle }
                if *id == transcribing_id
        )));
    }

    #[test]
    fn boundary_failures_clear_owned_state_and_allow_the_next_activation() {
        let mut runtime = AppRuntime::<u64, FakeRecording>::new(0.003, "en".to_owned()).unwrap();
        let mut io = FakeIo::default();

        io.reset_plan(1);
        io.start_fails = true;
        runtime.hold_started(Some(1), &mut io).unwrap();
        assert!(runtime.is_clean_idle());

        io.reset_plan(2);
        io.finish_plan = FinishPlan::Failure;
        runtime.hold_started(Some(2), &mut io).unwrap();
        runtime.hold_ended(&mut io).unwrap();
        assert!(runtime.is_clean_idle());

        let id = advance_to_transcribing(&mut runtime, &mut io, 3);
        runtime
            .transcription_completed(id, Err("recognition failed".to_owned()), &mut io)
            .unwrap();
        assert!(runtime.is_clean_idle());

        let id = advance_to_transcribing(&mut runtime, &mut io, 4);
        io.insertion_fails = true;
        runtime
            .transcription_completed(id, Ok(transcript("usable text")), &mut io)
            .unwrap();
        assert!(runtime.is_clean_idle());

        let id = advance_to_transcribing(&mut runtime, &mut io, 5);
        runtime
            .transcription_completed(id, Ok(transcript("recovered")), &mut io)
            .unwrap();
        assert!(runtime.is_clean_idle());
        assert_eq!(io.pasted_targets.last(), Some(&5));
    }

    #[test]
    fn status_failure_is_nonfatal_and_active_recording_drops_with_runtime() {
        let drops = Arc::new(AtomicUsize::new(0));
        let mut io = FakeIo {
            status_fails: true,
            recording_drops: Arc::clone(&drops),
            ..FakeIo::default()
        };
        io.reset_plan(1);
        io.status_fails = true;
        let mut runtime = AppRuntime::<u64, FakeRecording>::new(0.003, "en".to_owned()).unwrap();
        let notices = runtime.hold_started(Some(1), &mut io).unwrap();
        assert!(
            notices
                .iter()
                .any(|notice| matches!(notice, RuntimeNotice::StatusUpdateFailed { .. }))
        );
        assert_eq!(runtime.state(), RuntimeState::Listening);
        drop(runtime);
        assert_eq!(drops.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn raw_preserves_transcript_and_light_normalizes_spacing() {
        for (formatting, expected) in [
            (RuntimeFormatting::Raw, "  Hello   world ,  "),
            (RuntimeFormatting::Light, "Hello world,"),
        ] {
            let mut runtime = AppRuntime::<u64, FakeRecording>::new_with_formatting(
                0.003,
                "en".to_owned(),
                formatting,
            )
            .unwrap();
            let mut io = FakeIo::default();
            let id = advance_to_transcribing(&mut runtime, &mut io, 1);
            runtime
                .transcription_completed(id, Ok(transcript("  Hello   world ,  ")), &mut io)
                .unwrap();
            assert_eq!(io.inserted_texts, [expected]);
        }
    }

    #[test]
    fn cleanup_has_an_explicit_sticky_state_before_insertion() {
        let mut runtime = AppRuntime::<u64, FakeRecording>::new_with_formatting(
            0.003,
            "en".to_owned(),
            RuntimeFormatting::Balanced,
        )
        .unwrap();
        let mut io = FakeIo::default();
        let id = advance_to_transcribing(&mut runtime, &mut io, 1);

        let notices = runtime.cleanup_started(id, &mut io).unwrap();
        assert_eq!(runtime.state(), RuntimeState::Cleaning);
        assert!(notices.iter().any(
            |notice| matches!(notice, RuntimeNotice::CleanupStarted { id: found } if *found == id)
        ));

        runtime
            .transcription_completed(id, Ok(transcript("Cleaned output.")), &mut io)
            .unwrap();
        assert_eq!(io.inserted_texts, ["Cleaned output."]);
        assert!(runtime.is_clean_idle());
    }

    #[test]
    fn duplicate_final_completion_inserts_exactly_once() {
        let mut runtime = AppRuntime::<u64, FakeRecording>::new(0.003, "en".to_owned()).unwrap();
        let mut io = FakeIo::default();
        let id = advance_to_transcribing(&mut runtime, &mut io, 1);
        runtime
            .transcription_completed(id, Ok(transcript("one result")), &mut io)
            .unwrap();
        runtime
            .transcription_completed(id, Ok(transcript("duplicate")), &mut io)
            .unwrap();
        assert_eq!(io.insertions, 1);
        assert_eq!(io.inserted_texts, ["one result"]);
    }
}
