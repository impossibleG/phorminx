//! Dual capture has two independent sample ledgers. Never mix waveforms or infer speakers.
//! A global segment is a delivery batch, with original source timestamps in its text.
//! Wall-clock offsets correct device-open skew, not hardware clock-rate drift.
use super::*;
use std::time::Instant;

pub struct DualAudioService {
    inputs: [SingleAudioService; 2],
    epoch: Instant,
    offsets: [u64; 2],
    ready: [bool; 2],
    ended: [bool; 2],
    captured: [u64; 2],
    committed: [u64; 2],
    ledger: MergeLedger,
    pending: VecDeque<Barrier>,
    output: VecDeque<MeetingAudioEvent>,
    partials: [String; 2],
    stopping: bool,
    terminal: bool,
}
struct Barrier {
    id: u64,
    sample: u64,
    reached: [bool; 2],
    source_ends: [u64; 2],
}
#[derive(Default)]
struct MergeLedger {
    through: u64,
    text: Vec<SourceText>,
}
struct SourceText {
    source: usize,
    start: u64,
    end: u64,
    text: String,
}
fn label(source: usize) -> &'static str {
    if source == 0 {
        "Computer audio"
    } else {
        "You / microphone"
    }
}
fn stamp(sample: u64) -> String {
    let ms = sample.saturating_mul(1000) / RATE;
    format!(
        "{:02}:{:02}.{:03}",
        ms / 60_000,
        (ms / 1000) % 60,
        ms % 1000
    )
}
impl MergeLedger {
    fn drain(&mut self, through: u64) -> Option<MeetingAudioEvent> {
        self.drain_bounded(through, [through; 2])
    }
    fn drain_bounded(&mut self, through: u64, source_ends: [u64; 2]) -> Option<MeetingAudioEvent> {
        if through <= self.through {
            return None;
        }
        self.text.sort_by_key(|s| (s.start, s.source, s.end));
        let mut text = String::new();
        self.text.retain(|s| {
            if s.end > source_ends[s.source] {
                return true;
            }
            if !s.text.trim().is_empty() {
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(&format!(
                    "[{} · {}] {}",
                    stamp(s.start),
                    label(s.source),
                    s.text.trim()
                ));
            }
            false
        });
        let start_sample = self.through;
        self.through = through;
        Some(MeetingAudioEvent::Segment {
            start_sample,
            end_sample: through,
            text,
        })
    }
}
impl DualAudioService {
    pub fn set_mute(&mut self, microphone: bool, computer_audio: bool) -> Result<(), String> {
        if self.terminal || self.stopping {
            return Err("The meeting is stopping or stopped.".into());
        }
        // Validate both sources before changing either. Otherwise a source
        // stopping between UI frames could leave one source muted while the
        // workspace reports that the whole control failed.
        let mut computer = self.inputs[0]
            .control
            .lock()
            .map_err(|_| "Meeting control is unavailable.")?;
        let mut mic = self.inputs[1]
            .control
            .lock()
            .map_err(|_| "Meeting control is unavailable.")?;
        let prepare = |state: &Control, ended: bool, muted: bool| {
            if ended {
                return Ok(None);
            }
            if state.closed || state.stop_at.is_some() {
                return Err("The meeting is stopping or stopped.".to_owned());
            }
            let sample = state
                .observer
                .as_ref()
                .map_or(0, |observer| observer.progress().canonical_samples);
            let mut timeline = state.mute.clone();
            timeline.set(sample, muted)?;
            Ok(Some(timeline))
        };
        let computer_next = prepare(&computer, self.ended[0], computer_audio)?;
        let mic_next = prepare(&mic, self.ended[1], microphone)?;
        if let Some(next) = computer_next {
            computer.mute = next;
            computer.partial = None;
        }
        if let Some(next) = mic_next {
            mic.mute = next;
            mic.partial = None;
        }
        Ok(())
    }
    pub fn start(
        settings: RecognitionSettings,
        output_device: Option<String>,
        microphone_device: Option<String>,
        scratch: PathBuf,
    ) -> Result<Self, String> {
        let epoch = Instant::now();
        // Each source resolves its own saved selection (or Windows default).
        let computer = SingleAudioService::start(
            settings.clone(),
            MeetingAudioSource::SystemAudio,
            output_device,
            scratch.join("computer"),
        )?;
        let microphone = SingleAudioService::start(
            settings,
            MeetingAudioSource::Microphone,
            microphone_device,
            scratch.join("microphone"),
        )?;
        Ok(Self {
            inputs: [computer, microphone],
            epoch,
            offsets: [0; 2],
            ready: [false; 2],
            ended: [false; 2],
            captured: [0; 2],
            committed: [0; 2],
            ledger: MergeLedger::default(),
            pending: VecDeque::new(),
            output: VecDeque::new(),
            partials: Default::default(),
            stopping: false,
            terminal: false,
        })
    }
    pub fn request_cutoff(&mut self, id: u64) -> Result<u64, String> {
        if self.stopping || self.terminal {
            return Err("The meeting is stopping or stopped.".into());
        }
        if !self
            .ready
            .iter()
            .zip(self.ended)
            .all(|(ready, ended)| *ready || ended)
        {
            return Err("Both meeting inputs are still starting.".into());
        }
        if self.pending.len() >= MAX_PENDING_CUTOFFS
            || self.pending.back().is_some_and(|b| b.id >= id)
        {
            return Err("Please wait for pending transcript portions to finish.".into());
        }
        // Lock both controls before inserting either command: no half-applied cutoff.
        let mut controls = [
            self.inputs[0]
                .control
                .lock()
                .map_err(|_| "Meeting control unavailable.")?,
            self.inputs[1]
                .control
                .lock()
                .map_err(|_| "Meeting control unavailable.")?,
        ];
        let mut markers = [0; 2];
        for i in 0..2 {
            if self.ended[i] {
                markers[i] = self.committed[i];
                continue;
            }
            let c = &controls[i];
            if c.closed || c.stop_at.is_some() || c.cutoffs.len() >= MAX_PENDING_CUTOFFS {
                return Err("A meeting input is changing state. Please retry.".into());
            }
            markers[i] = c
                .observer
                .as_ref()
                .ok_or("Meeting input is still starting.")?
                .progress()
                .canonical_samples;
        }
        // This is a delivery ledger, not either device's clock. A later cutoff
        // can contain new source text even when clock skew leaves its maximum
        // timestamp unchanged. Reserve a distinct scalar boundary in that case.
        let next_delivery = self
            .pending
            .back()
            .map_or(self.ledger.through, |b| b.sample)
            .checked_add(1)
            .ok_or("Meeting delivery counter is exhausted.")?;
        let sample = (0..2)
            .map(|i| markers[i].saturating_add(self.offsets[i]))
            .max()
            .unwrap_or(0)
            .max(next_delivery);
        for i in 0..2 {
            if !self.ended[i] {
                controls[i].cutoffs.push_back((id, markers[i]));
            }
        }
        self.pending.push_back(Barrier {
            id,
            sample,
            reached: self.ended,
            source_ends: [
                markers[0].saturating_add(self.offsets[0]),
                markers[1].saturating_add(self.offsets[1]),
            ],
        });
        Ok(sample)
    }
    pub fn stop(&mut self) -> Result<(), String> {
        self.stopping = true;
        let first = self.inputs[0].stop();
        let second = self.inputs[1].stop();
        first.and(second)
    }
    pub fn try_recv(&mut self) -> Option<MeetingAudioEvent> {
        if let Some(event) = self.output.pop_front() {
            return Some(event);
        }
        if self.terminal {
            return None;
        }
        // Bound each poll so neither a fast input nor UI traffic starves the other.
        let mut any_changed = false;
        for _ in 0..32 {
            let mut changed = false;
            for source in 0..2 {
                if let Some(event) = self.inputs[source].try_recv() {
                    changed = true;
                    any_changed = true;
                    self.accept(source, event);
                }
            }
            if !changed {
                break;
            }
        }
        if any_changed {
            self.flush();
        }
        self.output.pop_front()
    }
    fn accept(&mut self, source: usize, event: MeetingAudioEvent) {
        match event {
            MeetingAudioEvent::SourceSegment { .. } => {
                unreachable!("single-source worker cannot emit merged metadata")
            }
            MeetingAudioEvent::Ready => {
                self.ready[source] = true;
                self.offsets[source] = self.inputs[source]
                    .control
                    .lock()
                    .ok()
                    .and_then(|c| c.started_at)
                    .map(|start| {
                        start.saturating_duration_since(self.epoch).as_secs_f64() * RATE as f64
                    })
                    .unwrap_or(0.0) as u64;
                if self
                    .ready
                    .iter()
                    .zip(self.ended)
                    .all(|(ready, ended)| *ready || ended)
                {
                    self.output.push_back(MeetingAudioEvent::Ready);
                }
            }
            MeetingAudioEvent::Segment {
                start_sample,
                end_sample,
                text,
            } => {
                self.output.push_back(MeetingAudioEvent::SourceSegment {
                    source: if source == 0 {
                        MeetingAudioSource::SystemAudio
                    } else {
                        MeetingAudioSource::Microphone
                    },
                    start_sample,
                    end_sample,
                    offset_sample: self.offsets[source],
                    text: text.clone(),
                });
                self.committed[source] = end_sample;
                self.partials[source].clear();
                self.ledger.text.push(SourceText {
                    source,
                    start: start_sample.saturating_add(self.offsets[source]),
                    end: end_sample.saturating_add(self.offsets[source]),
                    text,
                });
            }
            MeetingAudioEvent::Progress {
                captured_samples, ..
            } => self.captured[source] = captured_samples,
            MeetingAudioEvent::Partial { text, .. } => self.partials[source] = text,
            MeetingAudioEvent::Warning { message } => {
                self.output.push_back(MeetingAudioEvent::Warning {
                    message: format!("{}: {message}", label(source)),
                })
            }
            MeetingAudioEvent::CutoffReached { id, .. } => {
                if let Some(barrier) = self.pending.iter_mut().find(|barrier| barrier.id == id) {
                    barrier.reached[source] = true;
                }
            }
            MeetingAudioEvent::Stopped { sample } => {
                self.ended[source] = true;
                self.captured[source] = sample;
                for barrier in &mut self.pending {
                    barrier.reached[source] = true;
                }
            }
            MeetingAudioEvent::Error { message } => {
                self.ended[source] = true;
                self.partials[source].clear();
                for barrier in &mut self.pending {
                    barrier.reached[source] = true;
                }
                self.output.push_back(MeetingAudioEvent::Warning { message: format!("{} is unavailable: {message} The other input continues if available; missing audio is not reconstructed.", label(source)) });
                if self.ready[1 - source] {
                    self.output.push_back(MeetingAudioEvent::Ready);
                }
            }
        }
    }
    fn flush(&mut self) {
        // While a barrier is pending, don't let the faster source's next portion leak into it.
        while self
            .pending
            .front()
            .is_some_and(|b| b.reached == [true, true])
        {
            let barrier = self.pending.pop_front().unwrap();
            if let Some(event) = self
                .ledger
                .drain_bounded(barrier.sample, barrier.source_ends)
            {
                self.output.push_back(event);
            }
            self.output.push_back(MeetingAudioEvent::CutoffReached {
                id: barrier.id,
                sample: barrier.sample,
            });
        }
        let end = (0..2)
            .map(|i| self.committed[i].saturating_add(self.offsets[i]))
            .max()
            .unwrap_or(0);
        let watermark = (0..2)
            .filter(|i| !self.ended[*i])
            .map(|i| self.committed[i].saturating_add(self.offsets[i]))
            .min()
            .unwrap_or(end);
        if self.pending.is_empty()
            && let Some(event) = self.ledger.drain(watermark)
        {
            self.output.push_back(event);
        }
        if self.ended == [true, true] {
            // A faster source's post-cutoff portion may have timestamps behind
            // the global barrier. It still owns unsent words. Give that final
            // delivery its own interval instead of dropping it at equal time.
            let delivery_end = if self.ledger.text.is_empty() {
                end
            } else {
                end.max(self.ledger.through.saturating_add(1))
            };
            if let Some(event) = self.ledger.drain_bounded(delivery_end, [end; 2]) {
                self.output.push_back(event);
            }
            self.output.push_back(MeetingAudioEvent::Stopped {
                sample: self.ledger.through,
            });
            self.terminal = true;
        } else {
            let mut preview = self
                .ledger
                .text
                .iter()
                .filter(|part| !part.text.trim().is_empty())
                .map(|part| {
                    format!(
                        "[{} · {}] {}",
                        stamp(part.start),
                        label(part.source),
                        part.text
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            for i in 0..2 {
                if !self.partials[i].trim().is_empty() {
                    preview.push_str(&format!("\n{}: {}", label(i), self.partials[i]));
                }
            }
            let preview = preview.chars().take(16_384).collect::<String>();
            if !preview.is_empty() {
                self.output.push_back(MeetingAudioEvent::Partial {
                    start_sample: self.ledger.through,
                    text: preview,
                });
            }
            let captured_samples = (0..2)
                .map(|i| self.captured[i].saturating_add(self.offsets[i]))
                .max()
                .unwrap_or(0);
            // Avoid a perpetual event stream when nothing changed; output only with real progress.
            self.output.push_back(MeetingAudioEvent::Progress {
                captured_samples,
                committed_samples: self.ledger.through,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> DualAudioService {
        let input = |source| {
            let (_, events) = mpsc::channel();
            SingleAudioService {
                source,
                control: Arc::new(Mutex::new(Control::default())),
                events,
            }
        };
        DualAudioService {
            inputs: [
                input(MeetingAudioSource::SystemAudio),
                input(MeetingAudioSource::Microphone),
            ],
            epoch: Instant::now(),
            offsets: [0; 2],
            ready: [true; 2],
            ended: [false; 2],
            captured: [0; 2],
            committed: [0; 2],
            ledger: MergeLedger::default(),
            pending: VecDeque::new(),
            output: VecDeque::new(),
            partials: Default::default(),
            stopping: false,
            terminal: false,
        }
    }
    #[test]
    fn source_mutes_are_independent_and_do_not_stop_the_meeting() {
        let mut service = fixture();
        service.set_mute(true, false).unwrap();
        for (source, expected) in [(0, 0.5), (1, 0.0)] {
            let mute = &service.inputs[source].control.lock().unwrap().mute;
            let masked = mute.mask(AudioSpan::new(0, vec![0.5; 8]).unwrap()).unwrap();
            assert_eq!(masked.samples(), &[expected; 8]);
        }
        service.set_mute(false, true).unwrap();
        for (source, expected) in [(0, 0.0), (1, 0.5)] {
            let mute = &service.inputs[source].control.lock().unwrap().mute;
            let masked = mute.mask(AudioSpan::new(0, vec![0.5; 8]).unwrap()).unwrap();
            assert_eq!(masked.samples(), &[expected; 8]);
        }
        assert!(!service.terminal && !service.stopping);
    }
    #[test]
    fn failed_source_mute_does_not_change_the_other_source() {
        let mut service = fixture();
        service.inputs[1].control.lock().unwrap().stop_at = Some(0);
        assert!(service.set_mute(false, true).is_err());
        let computer = service.inputs[0].control.lock().unwrap();
        assert!(computer.mute.changes.is_empty());
    }
    #[test]
    fn cutoff_waits_for_both_sources_and_excludes_faster_sources_next_portion() {
        let mut service = fixture();
        service.pending.push_back(Barrier {
            id: 1,
            sample: 200,
            reached: [false; 2],
            source_ends: [100, 200],
        });
        service.accept(
            0,
            MeetingAudioEvent::Segment {
                start_sample: 0,
                end_sample: 100,
                text: "first".into(),
            },
        );
        service.accept(0, MeetingAudioEvent::CutoffReached { id: 1, sample: 100 });
        service.accept(
            0,
            MeetingAudioEvent::Segment {
                start_sample: 100,
                end_sample: 150,
                text: "next portion".into(),
            },
        );
        service.flush();
        assert!(
            !service
                .output
                .iter()
                .any(|e| matches!(e, MeetingAudioEvent::CutoffReached { .. }))
        );
        service.output.clear();
        service.accept(
            1,
            MeetingAudioEvent::Segment {
                start_sample: 0,
                end_sample: 200,
                text: "second".into(),
            },
        );
        service.accept(1, MeetingAudioEvent::CutoffReached { id: 1, sample: 200 });
        service.flush();
        let text = service
            .output
            .iter()
            .find_map(|e| {
                if let MeetingAudioEvent::Segment { text, .. } = e {
                    Some(text)
                } else {
                    None
                }
            })
            .unwrap();
        assert!(text.contains("first") && text.contains("second"));
        assert!(!text.contains("next portion"));
        assert_eq!(service.ledger.text.len(), 1);
        assert!(
            service
                .output
                .iter()
                .any(|e| matches!(e, MeetingAudioEvent::CutoffReached { id: 1, sample: 200 }))
        );
    }
    #[test]
    fn failed_input_is_explicit_and_other_input_continues() {
        let mut service = fixture();
        service.accept(
            0,
            MeetingAudioEvent::Error {
                message: "disconnected".into(),
            },
        );
        service.accept(
            1,
            MeetingAudioEvent::Segment {
                start_sample: 0,
                end_sample: 100,
                text: "still listening".into(),
            },
        );
        service.flush();
        assert!(
            service
                .output
                .iter()
                .any(|e| matches!(e, MeetingAudioEvent::Warning { .. }))
        );
        assert!(
            !service
                .output
                .iter()
                .any(|e| matches!(e, MeetingAudioEvent::Stopped { .. }))
        );
        assert!(service.output.iter().any(|e| matches!(e, MeetingAudioEvent::Segment {text,..} if text.contains("still listening"))));
        service.accept(1, MeetingAudioEvent::Stopped { sample: 100 });
        service.flush();
        assert!(service.terminal);
    }
    #[test]
    fn qa_stop_after_skewed_cutoff_must_preserve_the_unsent_source_tail() {
        let mut service = fixture();
        service.pending.push_back(Barrier {
            id: 1,
            sample: 200,
            reached: [false; 2],
            source_ends: [100, 200],
        });
        service.accept(
            0,
            MeetingAudioEvent::Segment {
                start_sample: 0,
                end_sample: 100,
                text: "first".into(),
            },
        );
        service.accept(0, MeetingAudioEvent::CutoffReached { id: 1, sample: 100 });
        service.accept(
            0,
            MeetingAudioEvent::Segment {
                start_sample: 100,
                end_sample: 150,
                text: "unsent tail".into(),
            },
        );
        service.accept(
            1,
            MeetingAudioEvent::Segment {
                start_sample: 0,
                end_sample: 200,
                text: "second".into(),
            },
        );
        service.accept(1, MeetingAudioEvent::CutoffReached { id: 1, sample: 200 });
        service.flush();
        service.output.clear();
        service.accept(0, MeetingAudioEvent::Stopped { sample: 150 });
        service.accept(1, MeetingAudioEvent::Stopped { sample: 200 });
        service.flush();
        assert!(service.output.iter().any(|event| matches!(event, MeetingAudioEvent::Segment {text,..} if text.contains("unsent tail"))), "Already recognized post-cutoff text must reach the main transcript before Stopped");
        assert!(service.ledger.text.is_empty());
    }
    #[test]
    fn successive_cutoff_with_same_maximum_source_clock_delivers_new_source_text() {
        let mut service = fixture();
        service.pending.push_back(Barrier {
            id: 1,
            sample: 200,
            reached: [true; 2],
            source_ends: [100, 200],
        });
        for (source, start, end, text) in [
            (0, 0, 100, "before"),
            (0, 100, 150, "after"),
            (1, 0, 200, "other"),
        ] {
            service.accept(
                source,
                MeetingAudioEvent::Segment {
                    start_sample: start,
                    end_sample: end,
                    text: text.into(),
                },
            );
        }
        service.flush();
        service.output.clear();
        // request_cutoff reserves a new delivery boundary even though max(150,200)
        // is still 200. These source cutoffs seal different audio than the first.
        service.pending.push_back(Barrier {
            id: 2,
            sample: 201,
            reached: [true; 2],
            source_ends: [150, 200],
        });
        service.flush();
        let text = service
            .output
            .iter()
            .find_map(|e| match e {
                MeetingAudioEvent::Segment { text, .. } => Some(text),
                _ => None,
            })
            .unwrap();
        assert!(text.contains("after"));
        assert!(!text.contains("before") && !text.contains("other"));
        assert!(service.ledger.text.is_empty());
    }
    #[test]
    fn unchanged_partial_cannot_make_polling_an_infinite_event_stream() {
        let mut service = fixture();
        service.accept(
            0,
            MeetingAudioEvent::Partial {
                start_sample: 0,
                text: "preview".into(),
            },
        );
        service.flush();
        for _ in 0..8 {
            let _ = service.try_recv();
        }
        assert!(service.try_recv().is_none());
    }
    #[test]
    fn interleaved_source_text_keeps_original_times_and_never_splits_words() {
        let mut ledger = MergeLedger::default();
        ledger.text.push(SourceText {
            source: 1,
            start: 16_800,
            end: 64_000,
            text: "microphone phrase".into(),
        });
        ledger.text.push(SourceText {
            source: 0,
            start: 16_000,
            end: 32_000,
            text: "computer phrase".into(),
        });
        let Some(MeetingAudioEvent::Segment { text, .. }) = ledger.drain(32_000) else {
            panic!()
        };
        assert_eq!(text, "[00:01.000 · Computer audio] computer phrase");
        let Some(MeetingAudioEvent::Segment {
            start_sample, text, ..
        }) = ledger.drain(64_000)
        else {
            panic!()
        };
        assert_eq!(start_sample, 32_000);
        assert_eq!(text, "[00:01.050 · You / microphone] microphone phrase");
        assert!(ledger.drain(64_000).is_none());
        assert!(ledger.text.is_empty());
    }
    #[test]
    fn simultaneous_sources_are_both_retained_and_silence_advances() {
        let mut ledger = MergeLedger::default();
        for source in [1, 0] {
            ledger.text.push(SourceText {
                source,
                start: 0,
                end: RATE,
                text: "yes yes".into(),
            });
        }
        let Some(MeetingAudioEvent::Segment { text, .. }) = ledger.drain(RATE) else {
            panic!()
        };
        assert_eq!(text.matches("yes yes").count(), 2);
        assert!(text.starts_with("[00:00.000 · Computer audio]"));
        assert!(
            matches!(ledger.drain(2 * RATE), Some(MeetingAudioEvent::Segment { text, .. }) if text.is_empty())
        );
    }
}
