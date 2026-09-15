//! Meeting recognition owns a separate capture pump, never an AI request.
//! Cutoffs are sample positions, not string offsets or guesses about words.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::Duration;

use phorminx_audio::{
    CaptureObserver, ExtendedCaptureConfig, ExtendedCaptureFactory, ExtendedCapturedAudio,
    ExtendedRecording,
};
use phorminx_core::{AudioClip, TranscriptionOptions};
use phorminx_session::{AudioSpan, SampleRange};
use phorminx_vosk::{VoskModel, VoskSession};
use phorminx_whisper::{WhisperBackendPreference, WhisperRecognizer};

use crate::settings::{AccurateBackendPreference, RecognitionMode, RecognitionSettings};

const RATE: u64 = 16_000;
const MIN_ACCURATE: u64 = 2 * RATE;
const MAX_ACCURATE: u64 = 12 * RATE;
const LOOKAHEAD: u64 = RATE / 2;
const INSTANT_BATCH: u64 = RATE / 4;
const INSTANT_ROLLOVER: u64 = 20 * RATE;
const MAX_PENDING_CUTOFFS: usize = 32;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MeetingAudioSource {
    Microphone,
    SystemAudio,
}

#[derive(Clone, Debug, PartialEq)]
pub enum MeetingAudioEvent {
    Ready,
    Warning {
        message: String,
    },
    /// Ephemeral preview, never sample ownership. Replaces the previous preview.
    Partial {
        start_sample: u64,
        text: String,
    },
    Progress {
        captured_samples: u64,
        committed_samples: u64,
    },
    /// Contiguous absolute sample ownership. Empty text legitimately owns silence.
    Segment {
        start_sample: u64,
        end_sample: u64,
        text: String,
    },
    CutoffReached {
        id: u64,
        sample: u64,
    },
    Stopped {
        sample: u64,
    },
    Error {
        message: String,
    },
}

#[derive(Default)]
struct Control {
    observer: Option<CaptureObserver>,
    cutoffs: VecDeque<(u64, u64)>,
    stop_at: Option<u64>,
    closed: bool,
    partial: Option<(u64, String)>,
}

/// Dropping requests shutdown without blocking the UI on native inference.
pub struct MeetingAudioService {
    control: Arc<Mutex<Control>>,
    events: mpsc::Receiver<MeetingAudioEvent>,
}

impl MeetingAudioService {
    /// Paths in `settings` must have been resolved against the settings store.
    /// `scratch_directory` is an empty ownership directory, not an audio archive;
    /// encrypted-spool fallback is explicitly disabled for meeting capture.
    pub fn start(
        settings: RecognitionSettings,
        source: MeetingAudioSource,
        device: Option<String>,
        scratch_directory: PathBuf,
    ) -> Result<Self, String> {
        let control = Arc::new(Mutex::new(Control::default()));
        let worker_control = Arc::clone(&control);
        let (sender, events) = mpsc::channel();
        thread::Builder::new()
            .name("phorminx-meeting-audio".into())
            .spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    run(settings, source, device, scratch_directory, &worker_control, &sender)
                })).unwrap_or_else(|_| Err("The meeting recognition worker failed unexpectedly. Captured text is retained.".into()));
                if let Ok(mut state) = worker_control.lock() {
                    if let Some(observer) = &state.observer {
                        observer.request_stop();
                    }
                    state.closed = true;
                }
                if let Err(message) = result {
                    let _ = sender.send(MeetingAudioEvent::Error { message });
                }
            })
            .map_err(|_| "Could not start the meeting audio worker.".to_owned())?;
        Ok(Self { control, events })
    }

    /// The gate also covers decode-plan selection, preventing a racing decoder
    /// from committing past a cutoff before that cutoff enters its command queue.
    pub fn request_cutoff(&self, id: u64) -> Result<u64, String> {
        let mut state = self
            .control
            .lock()
            .map_err(|_| "Meeting control is unavailable.")?;
        if state.closed || state.stop_at.is_some() {
            return Err("The meeting is stopping or stopped.".into());
        }
        if state.cutoffs.len() >= MAX_PENDING_CUTOFFS {
            return Err("Please wait for pending transcript portions to finish.".into());
        }
        if state.cutoffs.iter().any(|(pending, _)| *pending == id) {
            return Err("This transcript portion is already pending.".into());
        }
        let observer = state
            .observer
            .as_ref()
            .ok_or("Meeting audio is still starting.")?;
        let sample = observer.progress().canonical_samples;
        state.cutoffs.push_back((id, sample));
        Ok(sample)
    }

    pub fn stop(&self) -> Result<(), String> {
        let mut state = self
            .control
            .lock()
            .map_err(|_| "Meeting control is unavailable.")?;
        if state.closed || state.stop_at.is_some() {
            return Ok(());
        }
        state.stop_at = Some(
            state
                .observer
                .as_ref()
                .map_or(0, |observer| observer.progress().canonical_samples),
        );
        if let Some(observer) = &state.observer {
            observer.request_stop();
        }
        Ok(())
    }

    pub fn try_recv(&self) -> Option<MeetingAudioEvent> {
        self.events.try_recv().ok().or_else(|| {
            self.control
                .lock()
                .ok()?
                .partial
                .take()
                .map(|(start_sample, text)| MeetingAudioEvent::Partial { start_sample, text })
        })
    }
}

impl Drop for MeetingAudioService {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

enum Capture {
    Active(ExtendedRecording),
    Final(ExtendedCapturedAudio),
}
impl Capture {
    fn snapshot(&mut self, start: u64, end: u64) -> Result<AudioSpan, String> {
        let range = SampleRange::new(start, end).map_err(|_| "Invalid meeting audio range.")?;
        match self {
            Self::Active(recording) => recording.snapshot(range),
            Self::Final(recording) => recording.snapshot(range),
        }.map_err(|_| "Meeting audio could not be read. The input device or recognition backlog needs attention.".into())
    }
    fn acknowledge(&self, sample: u64) {
        if let Self::Active(recording) = self {
            recording.ownership_acknowledger().acknowledge(sample);
        }
    }
}

enum Decoder {
    Instant {
        model: VoskModel,
        session: Option<VoskSession>,
        origin: u64,
        accepted: u64,
    },
    Accurate {
        recognizer: WhisperRecognizer,
        language: String,
        attempted_through: u64,
        provisional: (u64, String),
    },
}

impl Decoder {
    fn load(settings: &RecognitionSettings) -> Result<Self, String> {
        match settings.mode {
            RecognitionMode::Instant => {
                let model = VoskModel::load(&settings.instant_runtime_path, &settings.instant_model_path, &settings.language)
                    .map_err(|_| "Instant recognition could not load. Check its runtime, model, and language in Models.".to_owned())?;
                let session = model
                    .session(RATE as u32)
                    .map_err(|_| "Instant recognition could not create a session.")?;
                Ok(Self::Instant {
                    model,
                    session: Some(session),
                    origin: 0,
                    accepted: 0,
                })
            }
            RecognitionMode::Accurate => {
                let backend = match settings.accurate_backend {
                    AccurateBackendPreference::Auto => WhisperBackendPreference::Auto,
                    AccurateBackendPreference::Cpu => WhisperBackendPreference::Cpu,
                    AccurateBackendPreference::Vulkan => WhisperBackendPreference::Vulkan,
                };
                let recognizer = WhisperRecognizer::load_with_backend(&settings.model_path, backend)
                    .map_err(|_| "Accurate recognition could not load. Check its model and compute backend in Models.".to_owned())?;
                // Whisper expects its primary language code, unlike the Vosk model selector.
                let language = settings
                    .language
                    .split('-')
                    .next()
                    .unwrap_or("en")
                    .to_owned();
                Ok(Self::Accurate {
                    recognizer,
                    language,
                    attempted_through: 0,
                    provisional: (0, String::new()),
                })
            }
        }
    }

    fn next_start(&self, committed: u64) -> u64 {
        match self {
            Self::Instant { accepted, .. } => *accepted,
            Self::Accurate { .. } => committed,
        }
    }

    fn plan(
        &self,
        committed: u64,
        available: u64,
        boundary: Option<u64>,
    ) -> Option<(u64, u64, bool)> {
        let plan = plan_range(
            self.next_start(committed),
            available,
            boundary,
            matches!(self, Self::Instant { .. }),
        )?;
        if let Self::Accurate {
            attempted_through, ..
        } = self
            && !should_attempt_accurate(plan.1, plan.2, *attempted_through)
        {
            return None;
        }
        Some(plan)
    }

    fn transcribe(
        &mut self,
        span: &AudioSpan,
        force: bool,
    ) -> Result<Option<(u64, String)>, String> {
        match self {
            Self::Instant {
                model,
                session,
                origin,
                accepted,
            } => {
                let outcome = session
                    .as_mut()
                    .ok_or("Instant session is unavailable.")?
                    .accept_f32(span.samples())
                    .map_err(|_| {
                        "Instant recognition could not process meeting audio.".to_owned()
                    })?;
                *accepted = span.range().end();
                let mut result = outcome
                    .endpoint
                    .map(|segment| (*origin + segment.end_sample, segment.text));
                if force || *accepted - *origin >= INSTANT_ROLLOVER {
                    let tail = session
                        .take()
                        .ok_or("Instant session is unavailable.")?
                        .finish()
                        .map_err(|_| {
                            "Instant recognition could not finish the transcript portion."
                                .to_owned()
                        })?;
                    let text = match result.take() {
                        Some((_, first)) => join_text(&first, &tail.text),
                        None => tail.text,
                    };
                    result = Some((*accepted, text));
                    *origin = *accepted;
                    *session = Some(model.session(RATE as u32).map_err(|_| {
                        "Instant recognition could not continue the meeting.".to_owned()
                    })?);
                }
                Ok(result)
            }
            Self::Accurate {
                recognizer,
                language,
                attempted_through,
                provisional,
            } => {
                *attempted_through = span.range().end();
                *provisional = (span.range().end(), String::new());
                if span.samples().iter().all(|sample| sample.abs() < 0.000_001) {
                    return Ok(Some((span.range().end(), String::new())));
                }
                let clip = AudioClip::new(span.samples().to_vec(), RATE as u32)
                    .map_err(|_| "Invalid meeting audio.")?;
                let detailed = recognizer
                    .transcribe_detailed(
                        &clip,
                        &TranscriptionOptions {
                            language: Some(language),
                            ..Default::default()
                        },
                        None,
                        None,
                    )
                    .map_err(|_| {
                        "Accurate recognition could not process meeting audio.".to_owned()
                    })?;
                if force {
                    return Ok(Some((
                        span.range().end(),
                        crate::incremental::strip_known_non_speech_annotations(
                            &detailed.transcript.text,
                        ),
                    )));
                }
                let stable_duration = span.range().len().saturating_sub(LOOKAHEAD);
                let mut text = String::new();
                let mut end = 0;
                let mut pending = String::new();
                let mut deferred = false;
                for segment in detailed.segments {
                    let segment_end = (segment.end.as_secs_f64() * RATE as f64).round() as u64;
                    // A crossing segment remains audio-owned and is decoded again
                    // with subsequent context. Never split its text proportionally.
                    if deferred || segment_end > stable_duration {
                        deferred = true;
                        pending = join_text(
                            &pending,
                            &crate::incremental::strip_known_non_speech_annotations(&segment.text),
                        );
                        continue;
                    }
                    if segment_end <= end {
                        continue;
                    }
                    text = join_text(
                        &text,
                        &crate::incremental::strip_known_non_speech_annotations(&segment.text),
                    );
                    end = segment_end;
                }
                *provisional = (span.range().start() + end, pending);
                Ok((end > 0).then_some((span.range().start() + end, text)))
            }
        }
    }

    fn flush_instant(&mut self, sample: u64) -> Result<Option<String>, String> {
        if let Self::Instant {
            model,
            session,
            origin,
            accepted,
        } = self
        {
            if *accepted != sample {
                return Err("Transcript cutoff was not fully processed.".into());
            }
            let tail = session
                .take()
                .ok_or("Instant session is unavailable.")?
                .finish()
                .map_err(|_| "Instant recognition could not finish the transcript portion.")?;
            *origin = sample;
            *session = Some(
                model
                    .session(RATE as u32)
                    .map_err(|_| "Instant recognition could not continue the meeting.")?,
            );
            return Ok(Some(tail.text));
        }
        Ok(None)
    }

    fn partial(&self, committed: u64) -> Option<(u64, String)> {
        match self {
            Self::Instant { session, .. } => {
                Some((committed, session.as_ref()?.partial_text().ok()?))
            }
            Self::Accurate { provisional, .. } => Some(provisional.clone()),
        }
    }
}

fn plan_range(
    start: u64,
    available: u64,
    boundary: Option<u64>,
    instant: bool,
) -> Option<(u64, u64, bool)> {
    let limit = boundary.unwrap_or(available).min(available);
    if limit <= start {
        return None;
    }
    let maximum = if instant { INSTANT_BATCH } else { MAX_ACCURATE };
    let end = limit.min(start.saturating_add(maximum));
    let force = boundary == Some(end) || (!instant && end - start == MAX_ACCURATE);
    if !instant && !force && end - start < MIN_ACCURATE {
        return None;
    }
    Some((start, end, force))
}

fn should_attempt_accurate(end: u64, force: bool, attempted_through: u64) -> bool {
    // A provisional timestamp tail should gain meaningful new context before
    // another full encoder pass. Explicit cutoffs always bypass this debounce.
    force || end.saturating_sub(attempted_through) >= MIN_ACCURATE
}

fn join_text(first: &str, second: &str) -> String {
    match (first.trim(), second.trim()) {
        ("", second) => second.to_owned(),
        (first, "") => first.to_owned(),
        (first, second) => format!("{first} {second}"),
    }
}

fn publish(
    sender: &mpsc::Sender<MeetingAudioEvent>,
    event: MeetingAudioEvent,
) -> Result<(), String> {
    sender
        .send(event)
        .map_err(|_| "The meeting view was closed.".into())
}

fn commit(
    sender: &mpsc::Sender<MeetingAudioEvent>,
    capture: &Capture,
    committed: &mut u64,
    end: u64,
    text: String,
) -> Result<(), String> {
    if end <= *committed {
        return if text.trim().is_empty() {
            Ok(())
        } else {
            Err("Recognition produced text outside its audio ownership range.".into())
        };
    }
    publish(
        sender,
        MeetingAudioEvent::Segment {
            start_sample: *committed,
            end_sample: end,
            text,
        },
    )?;
    *committed = end;
    capture.acknowledge(end);
    Ok(())
}

fn run(
    settings: RecognitionSettings,
    source: MeetingAudioSource,
    device: Option<String>,
    scratch: PathBuf,
    control: &Arc<Mutex<Control>>,
    sender: &mpsc::Sender<MeetingAudioEvent>,
) -> Result<(), String> {
    // Load once before opening audio; a loading model never consumes a rolling
    // recording's entire backlog allowance before it can begin decoding.
    let mut decoder = Decoder::load(&settings)?;
    {
        let state = control
            .lock()
            .map_err(|_| "Meeting control is unavailable.")?;
        if state.stop_at.is_some() {
            return publish(sender, MeetingAudioEvent::Stopped { sample: 0 });
        }
    }
    let mut config = ExtendedCaptureConfig::new(scratch);
    // This is bounded outstanding audio, not a duration limit: committed audio
    // is continuously reclaimed, permitting arbitrary meeting duration.
    config.short_memory_limit = Duration::from_secs(120);
    config.spill_uncommitted_to_disk = false;
    let factory = ExtendedCaptureFactory::new(config)
        .map_err(|_| "Meeting audio storage could not be prepared.")?;
    let recording = match source { MeetingAudioSource::Microphone => factory.start_input(device.as_deref()), MeetingAudioSource::SystemAudio => factory.start_output_loopback(device.as_deref()) }
        .map_err(|_| "The selected meeting audio device could not be opened. Check the device and its availability.".to_owned())?;
    let observer = recording.observer();
    {
        let mut state = control
            .lock()
            .map_err(|_| "Meeting control is unavailable.")?;
        state.observer = Some(observer.clone());
        if state.stop_at.is_some() {
            observer.request_stop();
        }
    }
    let mut capture = Some(Capture::Active(recording));
    publish(sender, MeetingAudioEvent::Ready)?;
    let mut committed = 0;
    let mut previous_available = 0;
    let mut previous_notice_count = 0;
    let mut partial_published = std::time::Instant::now();
    loop {
        let (available, boundary, plan, span) = {
            let mut state = control
                .lock()
                .map_err(|_| "Meeting control is unavailable.")?;
            let progress = observer.progress();
            if progress.sticky_fault.is_some() || progress.dropped_native_frames != 0 {
                return Err(format!(
                    "Meeting capture failed: {}{}. Captured text remains available.",
                    progress
                        .sticky_fault
                        .map(|fault| fault.to_string())
                        .unwrap_or_else(|| "capture queue lost frames".into()),
                    progress
                        .backend_failure
                        .map(|kind| format!(" [backend: {kind}]"))
                        .unwrap_or_default()
                ));
            }
            if progress.backend_notice_count != previous_notice_count {
                previous_notice_count = progress.backend_notice_count;
                let message = match progress.backend_notice {
                    Some("Xrun") => {
                        "Computer audio reported a timing discontinuity. Capture continues; some audio around the glitch may be missing."
                    }
                    Some("DeviceChanged") => {
                        "Windows rerouted the audio stream. Capture continues on the backend-selected device."
                    }
                    _ => {
                        "Windows declined real-time scheduling for audio. Capture continues; heavy system load may cause glitches."
                    }
                };
                publish(
                    sender,
                    MeetingAudioEvent::Warning {
                        message: message.into(),
                    },
                )?;
            }
            if state.stop_at.is_some() && matches!(capture, Some(Capture::Active(_))) {
                let Some(Capture::Active(recording)) = capture.take() else {
                    unreachable!()
                };
                let finalized = recording
                    .finalize()
                    .map_err(|_| "Meeting audio could not finalize its last portion.")?;
                // Stop flushes the resampler's pending native frames. These were
                // captured before Stop but not yet visible at a live cutoff.
                state.stop_at = Some(finalized.total_samples());
                capture = Some(Capture::Final(finalized));
            }
            let boundary = state
                .cutoffs
                .front()
                .map(|(_, sample)| *sample)
                .or(state.stop_at);
            let available = state.stop_at.unwrap_or(progress.canonical_samples);
            let plan = decoder.plan(committed, available, boundary);
            // Snapshot ownership is acquired before Stop can terminate the pump.
            // The gate is released before the expensive recognition call.
            let span = if let Some((start, end, _)) = plan {
                Some(
                    capture
                        .as_mut()
                        .ok_or("Meeting audio is unavailable.")?
                        .snapshot(start, end)?,
                )
            } else {
                None
            };
            (available, boundary, plan, span)
        };
        let current = capture.as_mut().ok_or("Meeting audio is unavailable.")?;
        if let (Some((_, _, force)), Some(span)) = (plan, span)
            && let Some((end, text)) = decoder.transcribe(&span, force)?
        {
            commit(sender, current, &mut committed, end, text)?;
        }
        if let Some(boundary) = boundary {
            if decoder.next_start(committed) == boundary
                && committed < boundary
                && let Some(text) = decoder.flush_instant(boundary)?
            {
                commit(sender, current, &mut committed, boundary, text)?;
            }
            if committed == boundary {
                let mut state = control
                    .lock()
                    .map_err(|_| "Meeting control is unavailable.")?;
                while state
                    .cutoffs
                    .front()
                    .is_some_and(|(_, sample)| *sample == committed)
                {
                    let (id, sample) = state.cutoffs.pop_front().expect("front was present");
                    publish(sender, MeetingAudioEvent::CutoffReached { id, sample })?;
                }
                if state.stop_at == Some(committed) && state.cutoffs.is_empty() {
                    return publish(sender, MeetingAudioEvent::Stopped { sample: committed });
                }
            }
        }
        if available != previous_available {
            publish(
                sender,
                MeetingAudioEvent::Progress {
                    captured_samples: available,
                    committed_samples: committed,
                },
            )?;
            previous_available = available;
        }
        if partial_published.elapsed() >= Duration::from_millis(250) {
            // One coalescing slot, not an event FIFO: a minimized/busy UI cannot
            // build an unbounded backlog of provisional versions.
            if let Ok(mut state) = control.lock() {
                state.partial = decoder.partial(committed);
            }
            partial_published = std::time::Instant::now();
        }
        // Avoid repeatedly decoding an unchanged provisional Accurate tail.
        if plan.is_none() || (boundary.is_none() && decoder.next_start(committed) < available) {
            thread::sleep(Duration::from_millis(100));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cutoff_forces_short_accurate_tail_without_reading_future_audio() {
        assert_eq!(
            plan_range(100, 90_000, Some(350), false),
            Some((100, 350, true))
        );
    }
    #[test]
    fn ordinary_accurate_window_waits_for_context_and_bounds_inference() {
        assert_eq!(plan_range(0, MIN_ACCURATE - 1, None, false), None);
        assert_eq!(
            plan_range(0, 900 * RATE, None, false),
            Some((0, MAX_ACCURATE, true))
        );
    }

    #[test]
    fn accurate_tail_does_not_redecode_on_each_ui_poll_but_cutoff_bypasses_wait() {
        assert!(!should_attempt_accurate(5 * RATE, false, 5 * RATE));
        assert!(!should_attempt_accurate(
            5 * RATE + RATE / 4,
            false,
            5 * RATE
        ));
        assert!(should_attempt_accurate(7 * RATE, false, 5 * RATE));
        assert!(should_attempt_accurate(5 * RATE + 1, true, 5 * RATE));
    }
    #[test]
    fn instant_accepts_short_batches_and_seals_only_at_exact_cutoff() {
        assert_eq!(
            plan_range(0, RATE, Some(RATE - 1), true),
            Some((0, INSTANT_BATCH, false))
        );
        assert_eq!(
            plan_range(RATE - 2, RATE, Some(RATE - 1), true),
            Some((RATE - 2, RATE - 1, true))
        );
    }
    #[test]
    fn empty_or_already_covered_cutoff_never_duplicates_audio() {
        assert_eq!(plan_range(20, 50, Some(20), false), None);
        assert_eq!(plan_range(20, 20, None, true), None);
    }
    #[test]
    fn simulated_hours_of_cutoffs_have_exact_contiguous_coverage() {
        for instant in [true, false] {
            let mut frontier = 0;
            for cutoff in (1..=2000).map(|i| i * 37_123) {
                while let Some((start, end, _)) =
                    plan_range(frontier, cutoff + RATE, Some(cutoff), instant)
                {
                    assert_eq!(start, frontier);
                    assert!(end <= cutoff && end > start);
                    frontier = end;
                }
                assert_eq!(frontier, cutoff);
            }
        }
    }
    #[test]
    fn stop_before_ready_is_idempotent_and_rejects_cutoffs() {
        let (_, events) = mpsc::channel();
        let service = MeetingAudioService {
            control: Arc::new(Mutex::new(Control::default())),
            events,
        };
        assert!(service.request_cutoff(1).is_err());
        service.stop().unwrap();
        service.stop().unwrap();
        assert!(service.request_cutoff(2).is_err());
        assert_eq!(service.control.lock().unwrap().stop_at, Some(0));
    }

    #[test]
    fn provisional_updates_coalesce_and_finalized_events_are_delivered_first() {
        let (sender, events) = mpsc::channel();
        let service = MeetingAudioService {
            control: Arc::new(Mutex::new(Control::default())),
            events,
        };
        for i in 0..10_000 {
            service.control.lock().unwrap().partial = Some((80, format!("preview {i}")));
        }
        sender
            .send(MeetingAudioEvent::Segment {
                start_sample: 0,
                end_sample: 80,
                text: "final".into(),
            })
            .unwrap();
        assert!(matches!(
            service.try_recv(),
            Some(MeetingAudioEvent::Segment { .. })
        ));
        assert_eq!(
            service.try_recv(),
            Some(MeetingAudioEvent::Partial {
                start_sample: 80,
                text: "preview 9999".into()
            })
        );
        assert!(service.try_recv().is_none());
    }
    #[test]
    fn joining_text_preserves_real_repetitions_and_handles_silence() {
        assert_eq!(join_text("yes", "yes"), "yes yes");
        assert_eq!(join_text("", " "), "");
        assert_eq!(join_text(" first ", " next "), "first next");
    }

    #[test]
    fn failed_model_start_reports_error_without_opening_capture_or_exposing_paths() {
        let directory = tempfile::tempdir().unwrap();
        let scratch = directory.path().join("must-not-open-audio-storage");
        let settings = RecognitionSettings {
            model_path: directory.path().join("private-meeting-model-missing.bin"),
            mode: RecognitionMode::Accurate,
            ..Default::default()
        };
        let service = MeetingAudioService::start(
            settings,
            MeetingAudioSource::SystemAudio,
            None,
            scratch.clone(),
        )
        .unwrap();
        let event = service.events.recv_timeout(Duration::from_secs(5)).unwrap();
        let MeetingAudioEvent::Error { message } = event else {
            panic!("failed model must not emit Ready");
        };
        assert!(!message.contains("private-meeting"));
        assert!(!scratch.exists());
        assert!(service.request_cutoff(1).is_err());
        service.stop().unwrap();
    }

    /// Real recognizers, but only an explicitly supplied synthesized fixture:
    /// this test never opens an audio device or reads personal recordings.
    fn native_fixture(mode: RecognitionMode) {
        let fixture = std::env::var_os("PHORMINX_SPOKEN_ROLLOVER_WAV")
            .expect("set the synthesized speech fixture path");
        let mut clip = phorminx_audio::read_wav(&PathBuf::from(fixture)).unwrap();
        clip.samples.truncate(35 * RATE as usize);
        assert!(
            clip.samples.len() > 30 * RATE as usize,
            "fixture must contain at least thirty seconds of synthetic speech"
        );
        let settings = RecognitionSettings {
            mode,
            instant_runtime_path: std::env::var_os("PHORMINX_VOSK_RUNTIME")
                .map(PathBuf::from)
                .unwrap_or_default(),
            instant_model_path: std::env::var_os("PHORMINX_VOSK_MODEL")
                .map(PathBuf::from)
                .unwrap_or_default(),
            model_path: std::env::var_os("PHORMINX_WHISPER_MODEL")
                .map(PathBuf::from)
                .unwrap_or_default(),
            accurate_backend: if std::env::var_os("PHORMINX_MEETING_TEST_VULKAN").is_some() {
                AccurateBackendPreference::Vulkan
            } else {
                AccurateBackendPreference::Cpu
            },
            ..Default::default()
        };
        let started = std::time::Instant::now();
        let mut decoder = Decoder::load(&settings).unwrap();
        let total = clip.samples.len() as u64;
        let mut cutoffs = VecDeque::from([50_721, 179_683, 478_551, total]);
        let mut committed = 0;
        let mut available = 0;
        let mut transcript = String::new();
        let mut segments = 0;
        let mut iterations = 0;
        let mut nonempty_previews = 0;
        while let Some(boundary) = cutoffs.front().copied() {
            iterations += 1;
            assert!(iterations < 2000, "recognition stopped making progress");
            available = (available + RATE / 4).min(total);
            if let Some((start, end, force)) = decoder.plan(committed, available, Some(boundary)) {
                let span =
                    AudioSpan::new(start, clip.samples[start as usize..end as usize].to_vec())
                        .unwrap();
                if let Some((end, text)) = decoder.transcribe(&span, force).unwrap() {
                    assert!(end > committed && end <= boundary);
                    committed = end;
                    transcript = join_text(&transcript, &text);
                    segments += 1;
                }
            }
            if decoder.next_start(committed) == boundary && committed < boundary {
                let text = decoder
                    .flush_instant(boundary)
                    .unwrap()
                    .expect("Instant pending endpoint");
                transcript = join_text(&transcript, &text);
                committed = boundary;
            }
            if committed == boundary {
                cutoffs.pop_front();
            }
            if let Some((start, text)) = decoder.partial(committed) {
                assert!(start >= committed);
                if !text.is_empty() {
                    nonempty_previews += 1;
                }
            }
        }
        assert_eq!(committed, total);
        assert!(segments >= 4);
        assert!(
            nonempty_previews > 0,
            "live recognition must provide provisional text before finalization"
        );
        assert!(
            transcript.split_whitespace().count() > 50,
            "synthetic speech did not produce a substantial transcript"
        );
        assert!(
            transcript.to_lowercase().contains("train"),
            "beginning of synthetic fixture is missing"
        );
        eprintln!(
            "meeting mode={mode:?} backend={} processing_ms={} sample_coverage={committed} segments={segments} words={} exact_cutoffs=4",
            if mode == RecognitionMode::Instant {
                "Vosk CPU".to_owned()
            } else {
                format!("{:?}", settings.accurate_backend)
            },
            started.elapsed().as_millis(),
            transcript.split_whitespace().count()
        );
        // A full-audio baseline is not human ground truth, but it catches text
        // dropped or repeated specifically by meeting chunk/cutoff orchestration.
        drop(decoder);
        let mut baseline_decoder = Decoder::load(&settings).unwrap();
        let baseline_span = AudioSpan::new(0, clip.samples).unwrap();
        let (_, baseline) = baseline_decoder
            .transcribe(&baseline_span, true)
            .unwrap()
            .unwrap();
        let reference = normalized_words(&baseline);
        let candidate = normalized_words(&transcript);
        let edits = word_distance(&reference, &candidate);
        assert!(!reference.is_empty());
        let variation = edits as f64 / reference.len() as f64;
        eprintln!(
            "meeting mode={mode:?} baseline_words={} word_edits={edits} baseline_variation={variation:.4}",
            reference.len()
        );
        assert!(
            variation <= 0.15,
            "cutoff transcript differs excessively from full-audio baseline: {variation:.3}"
        );
    }

    fn normalized_words(text: &str) -> Vec<String> {
        text.split_whitespace()
            .map(|word| {
                word.chars()
                    .filter(|c| c.is_alphanumeric())
                    .collect::<String>()
                    .to_lowercase()
            })
            .filter(|word| !word.is_empty())
            .collect()
    }

    fn word_distance(reference: &[String], candidate: &[String]) -> usize {
        let mut previous = (0..=candidate.len()).collect::<Vec<_>>();
        for (row, expected) in reference.iter().enumerate() {
            let mut current = vec![row + 1; candidate.len() + 1];
            for (column, actual) in candidate.iter().enumerate() {
                current[column + 1] = (previous[column] + usize::from(expected != actual))
                    .min(previous[column + 1] + 1)
                    .min(current[column] + 1);
            }
            previous = current;
        }
        previous[candidate.len()]
    }

    #[test]
    fn native_quality_metric_detects_inserted_deleted_and_repeated_words() {
        let reference = normalized_words("One, two three.");
        assert_eq!(
            word_distance(&reference, &normalized_words("one two three")),
            0
        );
        assert_eq!(word_distance(&reference, &normalized_words("one three")), 1);
        assert_eq!(
            word_distance(&reference, &normalized_words("one two two three")),
            1
        );
        assert_eq!(
            word_distance(&reference, &normalized_words("one four three")),
            1
        );
    }

    #[test]
    #[ignore = "requires explicit neutral synthesized WAV and installed Vosk assets"]
    fn native_meeting_instant_cutoffs_and_rollover() {
        native_fixture(RecognitionMode::Instant);
    }

    #[test]
    #[ignore = "requires explicit neutral synthesized WAV and installed Whisper model"]
    fn native_meeting_accurate_cutoffs_and_tail() {
        native_fixture(RecognitionMode::Accurate);
    }
}
