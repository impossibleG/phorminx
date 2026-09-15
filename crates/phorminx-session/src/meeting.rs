//! Text-only meeting boundaries. Recognition and AI lifetimes are independent.
use std::collections::VecDeque;

use thiserror::Error;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MeetingSegment {
    pub start_sample: u64,
    pub end_sample: u64,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MeetingSubmission {
    pub cutoff_id: u64,
    pub start_sample: u64,
    pub end_sample: u64,
    pub text: String,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum MeetingBoundaryError {
    #[error("transcript segments must cover consecutive, nonempty sample ranges")]
    NonContiguous,
    #[error("cutoff identifiers must increase and sample markers cannot move backwards")]
    InvalidCutoff,
    #[error("recognition must seal a transcript segment at the requested cutoff")]
    CrossingCutoff,
    #[error("cutoff acknowledgement does not match the next requested, committed boundary")]
    InvalidAcknowledgement,
}

/// Holds only text not yet submitted. Empty text segments still advance audio coverage.
/// The capture adapter must seal recognition at each requested sample marker; text is
/// never proportionally divided, since doing so silently loses word ownership.
#[derive(Default, Debug)]
pub struct MeetingCutoffs {
    committed_sample: u64,
    submitted_sample: u64,
    last_requested: Option<(u64, u64)>,
    last_segment: Option<MeetingSegment>,
    pending: VecDeque<(u64, u64)>,
    segments: VecDeque<MeetingSegment>,
}

impl MeetingCutoffs {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn committed_sample(&self) -> u64 {
        self.committed_sample
    }

    pub fn submitted_sample(&self) -> u64 {
        self.submitted_sample
    }

    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }

    pub fn request_cutoff(&mut self, id: u64, sample: u64) -> Result<(), MeetingBoundaryError> {
        if sample < self.committed_sample
            || self
                .last_requested
                .is_some_and(|(last_id, last_sample)| id <= last_id || sample < last_sample)
        {
            return Err(MeetingBoundaryError::InvalidCutoff);
        }
        self.pending.push_back((id, sample));
        self.last_requested = Some((id, sample));
        Ok(())
    }

    /// Returns false only for an exact replay of the last committed segment.
    pub fn commit(&mut self, segment: MeetingSegment) -> Result<bool, MeetingBoundaryError> {
        if self.last_segment.as_ref() == Some(&segment) {
            return Ok(false);
        }
        if segment.start_sample != self.committed_sample
            || segment.end_sample <= segment.start_sample
        {
            return Err(MeetingBoundaryError::NonContiguous);
        }
        if self
            .pending
            .iter()
            .any(|(_, marker)| segment.start_sample < *marker && *marker < segment.end_sample)
        {
            return Err(MeetingBoundaryError::CrossingCutoff);
        }
        self.committed_sample = segment.end_sample;
        self.last_segment = Some(segment.clone());
        self.segments.push_back(segment);
        Ok(true)
    }

    /// Consumes a boundary only after the audio worker confirms exact ownership.
    /// An empty submission is intentional (silence or two sends at the same marker);
    /// the host can omit an AI request without losing subsequent speech.
    pub fn boundary_reached(
        &mut self,
        id: u64,
        sample: u64,
    ) -> Result<MeetingSubmission, MeetingBoundaryError> {
        let submission = self.preview_boundary(id, sample)?;
        while self
            .segments
            .front()
            .is_some_and(|segment| segment.end_sample <= sample)
        {
            self.segments.pop_front();
        }
        self.pending.pop_front();
        self.submitted_sample = sample;
        Ok(submission)
    }

    /// Preview a ready cutoff without consuming text. The host can validate the
    /// provider/context budget first and retry later after a failed preflight.
    pub fn preview_boundary(
        &self,
        id: u64,
        sample: u64,
    ) -> Result<MeetingSubmission, MeetingBoundaryError> {
        if self.pending.front() != Some(&(id, sample))
            || sample > self.committed_sample
            || self
                .segments
                .iter()
                .any(|s| s.start_sample < sample && sample < s.end_sample)
        {
            return Err(MeetingBoundaryError::InvalidAcknowledgement);
        }
        let start_sample = self.submitted_sample;
        let mut text = String::new();
        for segment in self
            .segments
            .iter()
            .take_while(|segment| segment.end_sample <= sample)
        {
            let part = segment.text.trim();
            if !part.is_empty() {
                if !text.is_empty() {
                    text.push(' ');
                }
                text.push_str(part);
            }
        }
        Ok(MeetingSubmission {
            cutoff_id: id,
            start_sample,
            end_sample: sample,
            text,
        })
    }
}

/// A cancelled/finished generation cannot publish output, even when its transport
/// delivers buffered tokens after cancellation. Never used to stop audio capture.
#[derive(Default, Debug)]
pub struct AiRunTracker {
    generation: u64,
    active: bool,
}

impl AiRunTracker {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn begin(&mut self) -> Option<u64> {
        self.generation = self.generation.checked_add(1)?;
        self.active = true;
        Some(self.generation)
    }
    /// Adopt a persisted generation floor before starting chat in a saved session.
    /// Never changes an active run, and never lowers the process-wide counter.
    pub fn advance_past(&mut self, generation: u64) -> bool {
        if self.active {
            return false;
        }
        self.generation = self.generation.max(generation);
        true
    }
    pub fn accepts(&self, generation: u64) -> bool {
        self.active && generation == self.generation
    }
    pub fn finish(&mut self, generation: u64) -> bool {
        if !self.accepts(generation) {
            return false;
        }
        self.active = false;
        true
    }
    pub fn cancel(&mut self) {
        self.active = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn segment(start: u64, end: u64, text: &str) -> MeetingSegment {
        MeetingSegment {
            start_sample: start,
            end_sample: end,
            text: text.into(),
        }
    }
    #[test]
    fn cutoffs_never_repeat_or_lose_text_while_capture_continues() {
        let mut state = MeetingCutoffs::new();
        state.commit(segment(0, 10, "before")).unwrap();
        state.request_cutoff(1, 20).unwrap();
        state.request_cutoff(2, 40).unwrap();
        state.commit(segment(10, 20, "boundary")).unwrap();
        state.commit(segment(20, 30, "after")).unwrap();
        assert_eq!(
            state.boundary_reached(1, 20).unwrap().text,
            "before boundary"
        );
        state.commit(segment(30, 40, "next")).unwrap();
        let second = state.boundary_reached(2, 40).unwrap();
        assert_eq!(
            (second.start_sample, second.end_sample, second.text.as_str()),
            (20, 40, "after next")
        );
        assert_eq!(state.pending_count(), 0);
    }
    #[test]
    fn boundary_errors_do_not_mutate_committed_text() {
        let mut state = MeetingCutoffs::new();
        state.request_cutoff(1, 10).unwrap();
        assert_eq!(
            state.commit(segment(0, 20, "crosses")),
            Err(MeetingBoundaryError::CrossingCutoff)
        );
        assert_eq!(
            state.boundary_reached(1, 10),
            Err(MeetingBoundaryError::InvalidAcknowledgement)
        );
        assert_eq!(
            state.commit(segment(1, 10, "gap")),
            Err(MeetingBoundaryError::NonContiguous)
        );
        assert_eq!(state.committed_sample(), 0);
        state.commit(segment(0, 10, "safe")).unwrap();
        assert!(!state.commit(segment(0, 10, "safe")).unwrap());
        assert!(state.commit(segment(0, 10, "changed")).is_err());
        assert!(state.boundary_reached(2, 10).is_err());
        assert_eq!(state.boundary_reached(1, 10).unwrap().text, "safe");
        assert!(state.boundary_reached(1, 10).is_err());
    }
    #[test]
    fn silence_and_repeated_sample_cutoffs_are_explicit_empty_submissions() {
        let mut state = MeetingCutoffs::new();
        state.request_cutoff(1, 0).unwrap();
        assert!(state.boundary_reached(1, 0).unwrap().text.is_empty());
        state.request_cutoff(2, 20).unwrap();
        state.request_cutoff(3, 20).unwrap();
        state.commit(segment(0, 20, " ")).unwrap();
        assert!(state.boundary_reached(2, 20).unwrap().text.is_empty());
        assert!(state.boundary_reached(3, 20).unwrap().text.is_empty());
        assert!(state.request_cutoff(3, 30).is_err());
        assert!(state.request_cutoff(4, 19).is_err());
    }
    #[test]
    fn cancellation_and_restart_reject_all_late_output() {
        let mut runs = AiRunTracker::new();
        let first = runs.begin().unwrap();
        let second = runs.begin().unwrap();
        assert!(!runs.accepts(first));
        assert!(!runs.finish(first));
        assert!(runs.accepts(second));
        runs.cancel();
        assert!(!runs.accepts(second));
        let third = runs.begin().unwrap();
        assert!(runs.finish(third));
        assert!(!runs.accepts(third));
    }

    #[test]
    fn saved_generations_advance_only_when_inactive_and_never_wrap() {
        let mut runs = AiRunTracker::new();
        assert!(runs.advance_past(40));
        let current = runs.begin().unwrap();
        assert_eq!(current, 41);
        assert!(!runs.advance_past(100));
        assert!(runs.accepts(current));
        runs.cancel();
        assert!(runs.advance_past(2));
        assert_eq!(runs.begin(), Some(42));
        runs.cancel();
        assert!(runs.advance_past(u64::MAX));
        assert_eq!(runs.begin(), None);
        assert!(!runs.accepts(u64::MAX));
    }

    #[test]
    fn failed_preflight_can_retry_same_cutoff_while_recording_continues() {
        let mut state = MeetingCutoffs::new();
        state.request_cutoff(1, 10).unwrap();
        state.commit(segment(0, 10, "retry me")).unwrap();
        let preview = state.preview_boundary(1, 10).unwrap();
        assert_eq!(state.submitted_sample(), 0);
        assert_eq!(state.pending_count(), 1);
        state.commit(segment(10, 20, "next portion")).unwrap();
        assert_eq!(state.preview_boundary(1, 10).unwrap(), preview);
        assert_eq!(state.boundary_reached(1, 10).unwrap(), preview);
        assert!(state.preview_boundary(1, 10).is_err());
        state.request_cutoff(2, 20).unwrap();
        assert_eq!(state.boundary_reached(2, 20).unwrap().text, "next portion");
    }
    #[test]
    fn many_cutoffs_preserve_unicode_without_a_duration_limit() {
        let mut state = MeetingCutoffs::new();
        for n in 1..=10_000u64 {
            let marker = n * 16_000 * 60;
            state.request_cutoff(n, marker).unwrap();
            state
                .commit(segment((n - 1) * 16_000 * 60, marker, "Olá 世界"))
                .unwrap();
            assert_eq!(state.boundary_reached(n, marker).unwrap().text, "Olá 世界");
        }
        assert!(state.segments.is_empty());
    }
}
