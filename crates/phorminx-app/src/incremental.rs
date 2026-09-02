//! Bounded scheduling and transcript reconciliation for incremental STT.
//!
//! The planner is deliberately independent of microphone and Whisper types so
//! its timing, backpressure, and fail-closed behavior can be tested without
//! hardware or a model.

use std::ops::Range;
use std::time::Duration;

use phorminx_core::DictationId;

pub const MIN_CHUNK_DURATION: Duration = Duration::from_millis(1_500);
pub const MAX_CHUNK_DURATION: Duration = Duration::from_secs(3);
pub const CHUNK_OVERLAP: Duration = Duration::from_millis(500);
pub const SILENCE_PROBE_DURATION: Duration = CHUNK_OVERLAP;
pub const PROBE_INTERVAL: Duration = Duration::from_millis(200);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BoundaryKind {
    Silence,
    Forced,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ChunkPlan {
    pub id: DictationId,
    pub sequence: u32,
    pub range: TimeRange,
    pub stable_end: Duration,
    /// Confidence rule for this chunk's overlap with the preceding chunk.
    /// This comes from the preceding chunk's end boundary, not this one's.
    pub start_overlap: Option<MergeExpectation>,
    pub boundary: BoundaryKind,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TimeRange {
    pub start: Duration,
    pub end: Duration,
}

impl TimeRange {
    pub fn duration(self) -> Duration {
        self.end.saturating_sub(self.start)
    }
}

#[derive(Debug, Default)]
pub struct IncrementalPlanner {
    session: Option<Session>,
}

#[derive(Debug)]
struct Session {
    id: DictationId,
    stable_end: Duration,
    next_sequence: u32,
    in_flight: Option<ChunkPlan>,
    next_probe_at: Duration,
    last_boundary: Option<BoundaryKind>,
    degraded: bool,
}

impl IncrementalPlanner {
    pub fn start(&mut self, id: DictationId) {
        self.session = Some(Session {
            id,
            stable_end: Duration::ZERO,
            next_sequence: 0,
            in_flight: None,
            next_probe_at: MIN_CHUNK_DURATION,
            last_boundary: None,
            degraded: false,
        });
    }

    pub fn active_id(&self) -> Option<DictationId> {
        self.session.as_ref().map(|session| session.id)
    }

    pub fn needs_probe(&self, id: DictationId, captured: Duration) -> bool {
        self.session.as_ref().is_some_and(|session| {
            session.id == id
                && !session.degraded
                && session.in_flight.is_none()
                && captured >= session.next_probe_at
                && captured.saturating_sub(session.stable_end) >= MIN_CHUNK_DURATION
        })
    }

    pub fn observe(
        &mut self,
        id: DictationId,
        captured: Duration,
        silence_observed: bool,
    ) -> Option<ChunkPlan> {
        let session = self.session.as_mut()?;
        if session.id != id
            || session.degraded
            || session.in_flight.is_some()
            || captured < session.next_probe_at
            || captured.saturating_sub(session.stable_end) < MIN_CHUNK_DURATION
        {
            return None;
        }

        let forced_end = session.stable_end.saturating_add(MAX_CHUNK_DURATION);
        let silence_covers_deadline =
            silence_observed && captured <= forced_end.saturating_add(PROBE_INTERVAL);
        let (end, boundary) = if silence_covers_deadline {
            (captured.min(forced_end), BoundaryKind::Silence)
        } else if captured >= forced_end {
            (forced_end, BoundaryKind::Forced)
        } else {
            session.next_probe_at = captured.saturating_add(PROBE_INTERVAL);
            return None;
        };
        let start = session.stable_end.saturating_sub(CHUNK_OVERLAP);
        let plan = ChunkPlan {
            id,
            sequence: session.next_sequence,
            range: TimeRange { start, end },
            stable_end: end,
            start_overlap: session.last_boundary.map(MergeExpectation::from),
            boundary,
        };
        session.next_sequence = session.next_sequence.saturating_add(1);
        session.in_flight = Some(plan);
        Some(plan)
    }

    pub fn partial_completed(&mut self, id: DictationId, sequence: u32, succeeded: bool) {
        let Some(session) = self.session.as_mut() else {
            return;
        };
        let Some(plan) = session.in_flight else {
            return;
        };
        if session.id != id || plan.sequence != sequence {
            return;
        }
        session.in_flight = None;
        if succeeded {
            session.stable_end = plan.stable_end;
            session.last_boundary = Some(plan.boundary);
            session.next_probe_at = plan.stable_end.saturating_add(MIN_CHUNK_DURATION);
        } else {
            session.degraded = true;
        }
    }

    pub fn degrade(&mut self, id: DictationId) {
        if let Some(session) = self.session.as_mut().filter(|session| session.id == id) {
            session.degraded = true;
            session.in_flight = None;
        }
    }

    pub fn finish(&mut self, id: DictationId) {
        if self.active_id() == Some(id) {
            self.session = None;
        }
    }

    pub fn cancel(&mut self) -> Option<DictationId> {
        self.session.take().map(|session| session.id)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MergeExpectation {
    /// The overlap was measured as silence, so no lexical match is required.
    Silence,
    /// The boundary was forced through speech; at least two matching words are
    /// required before any text is removed.
    LexicalOverlap,
}

impl From<BoundaryKind> for MergeExpectation {
    fn from(value: BoundaryKind) -> Self {
        match value {
            BoundaryKind::Silence => Self::Silence,
            BoundaryKind::Forced => Self::LexicalOverlap,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MergeError {
    AmbiguousOverlap,
    MissingOverlap,
}

/// Returns true only when the complete Whisper output consists of known,
/// bracketed non-speech annotations. Arbitrary bracketed dictation is retained.
pub fn is_non_speech_only(input: &str) -> bool {
    !input.trim().is_empty() && strip_known_non_speech_annotations(input).is_empty()
}

/// Removes only whitelisted bracketed Whisper annotations, including when
/// they appear beside real speech. Unknown bracketed content is preserved.
pub fn strip_known_non_speech_annotations(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut cursor = 0;
    let mut removed = false;
    while let Some(relative_open) = input[cursor..].find('[') {
        let open = cursor + relative_open;
        let Some(relative_close) = input[open + 1..].find(']') else {
            break;
        };
        let close = open + 1 + relative_close;
        if is_known_non_speech_label(&input[open + 1..close]) {
            output.push_str(&input[cursor..open]);
            while output.ends_with(char::is_whitespace) {
                output.pop();
            }
            cursor = close + 1;
            while cursor < input.len() {
                let Some(character) = input[cursor..].chars().next() else {
                    break;
                };
                if !character.is_whitespace() {
                    break;
                }
                cursor += character.len_utf8();
            }
            if !output.is_empty()
                && cursor < input.len()
                && !starts_with_known_non_speech_annotation(&input[cursor..])
                && !input[cursor..]
                    .chars()
                    .next()
                    .is_some_and(|character| matches!(character, '.' | ',' | ';' | ':' | '!' | '?'))
            {
                output.push(' ');
            }
            removed = true;
        } else {
            output.push_str(&input[cursor..=close]);
            cursor = close + 1;
        }
    }
    output.push_str(&input[cursor..]);
    if !removed {
        return input.to_owned();
    }
    if output.chars().all(|character| {
        character.is_whitespace() || matches!(character, '.' | ',' | ';' | ':' | '!' | '?' | '-')
    }) {
        String::new()
    } else {
        output
    }
}

fn starts_with_known_non_speech_annotation(input: &str) -> bool {
    let Some(after_open) = input.strip_prefix('[') else {
        return false;
    };
    let Some(close) = after_open.find(']') else {
        return false;
    };
    is_known_non_speech_label(&after_open[..close])
}

fn is_known_non_speech_label(label: &str) -> bool {
    let normalized = label
        .chars()
        .filter(|character| character.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect::<String>();
    matches!(
        normalized.as_str(),
        "blankaudio"
            | "silence"
            | "music"
            | "nospeech"
            | "noaudio"
            | "silêncio"
            | "música"
            | "semfala"
            | "semáudio"
            | "áudioembranco"
            | "áudiovazio"
    )
}

/// Reconciles two overlapping Whisper outputs without guessing.
///
/// A confident match replaces the older overlapping suffix with the newer
/// rendering, preserving punctuation from the transcript with more right-hand
/// context. A silence boundary may be concatenated when there is no match. A
/// one-word match is never removed because it may be legitimate repetition.
pub fn merge_overlapping(
    left: &str,
    right: &str,
    expectation: MergeExpectation,
) -> Result<String, MergeError> {
    let left = left.trim();
    let right = right.trim();
    if left.is_empty() {
        return Ok(right.to_owned());
    }
    if right.is_empty() {
        return Ok(left.to_owned());
    }

    let left_tokens = word_tokens(left);
    let right_tokens = word_tokens(right);
    let maximum = left_tokens.len().min(right_tokens.len()).min(16);
    let overlap = (1..=maximum).rev().find(|&count| {
        left_tokens[left_tokens.len() - count..]
            .iter()
            .map(|token| &token.normalized)
            .eq(right_tokens[..count].iter().map(|token| &token.normalized))
    });

    match overlap {
        Some(count) if count >= 2 => {
            let left_start = left_tokens.len() - count;
            let repeated_before = left_start >= count
                && same_tokens(
                    &left_tokens[left_start - count..left_start],
                    &left_tokens[left_start..],
                );
            let repeated_after = right_tokens.len() >= count * 2
                && same_tokens(&right_tokens[..count], &right_tokens[count..count * 2]);
            if repeated_before || repeated_after {
                return Err(MergeError::AmbiguousOverlap);
            }
            let replace_from = left_tokens[left_start].range.start;
            Ok(join_nonempty(&left[..replace_from], right))
        }
        Some(_) => Err(MergeError::AmbiguousOverlap),
        None if expectation == MergeExpectation::Silence => Ok(join_nonempty(left, right)),
        None => Err(MergeError::MissingOverlap),
    }
}

fn same_tokens(left: &[WordToken], right: &[WordToken]) -> bool {
    left.iter()
        .map(|token| &token.normalized)
        .eq(right.iter().map(|token| &token.normalized))
}

fn join_nonempty(left: &str, right: &str) -> String {
    let left = left.trim_end();
    let right = right.trim_start();
    if left.is_empty() {
        right.to_owned()
    } else if right.is_empty() {
        left.to_owned()
    } else {
        format!("{left} {right}")
    }
}

struct WordToken {
    normalized: String,
    range: Range<usize>,
}

fn word_tokens(input: &str) -> Vec<WordToken> {
    let mut tokens = Vec::new();
    let mut start = None;
    for (index, character) in input.char_indices() {
        if character.is_alphanumeric() || character == '_' {
            start.get_or_insert(index);
        } else if let Some(token_start) = start.take() {
            tokens.push(WordToken {
                normalized: input[token_start..index].to_lowercase(),
                range: token_start..index,
            });
        }
    }
    if let Some(token_start) = start {
        tokens.push(WordToken {
            normalized: input[token_start..].to_lowercase(),
            range: token_start..input.len(),
        });
    }
    tokens
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(value: u64) -> DictationId {
        DictationId(value)
    }

    #[test]
    fn short_recordings_never_schedule_incremental_work() {
        let mut planner = IncrementalPlanner::default();
        planner.start(id(1));

        assert!(!planner.needs_probe(id(1), Duration::from_millis(1_499)));
        assert_eq!(
            planner.observe(id(1), Duration::from_millis(1_499), true),
            None
        );
    }

    #[test]
    fn silence_is_preferred_and_only_one_chunk_can_be_in_flight() {
        let mut planner = IncrementalPlanner::default();
        planner.start(id(1));
        let first = planner
            .observe(id(1), Duration::from_secs(2), true)
            .unwrap();
        assert_eq!(first.boundary, BoundaryKind::Silence);
        assert_eq!(first.range.start, Duration::ZERO);
        assert_eq!(first.range.end, Duration::from_secs(2));
        assert_eq!(planner.observe(id(1), Duration::from_secs(10), true), None);

        planner.partial_completed(id(1), first.sequence, true);
        let second = planner
            .observe(id(1), Duration::from_secs(4), true)
            .unwrap();
        assert_eq!(second.range.start, Duration::from_millis(1_500));
        assert_eq!(second.range.end, Duration::from_secs(4));
    }

    #[test]
    fn continuous_speech_forces_a_bounded_chunk() {
        let mut planner = IncrementalPlanner::default();
        planner.start(id(7));
        assert_eq!(
            planner.observe(id(7), Duration::from_millis(1_500), false),
            None
        );
        let plan = planner
            .observe(id(7), Duration::from_secs(4), false)
            .unwrap();
        assert_eq!(plan.boundary, BoundaryKind::Forced);
        assert_eq!(plan.range.end, Duration::from_secs(3));
        assert!(plan.range.duration() <= MAX_CHUNK_DURATION + CHUNK_OVERLAP);
    }

    #[test]
    fn silence_wins_at_and_just_after_the_forced_deadline() {
        for captured in [Duration::from_secs(3), Duration::from_millis(3_100)] {
            let mut planner = IncrementalPlanner::default();
            planner.start(id(70));
            let plan = planner.observe(id(70), captured, true).unwrap();

            assert_eq!(plan.boundary, BoundaryKind::Silence);
            assert_eq!(plan.range.end, captured.min(Duration::from_secs(3)));
        }
    }

    #[test]
    fn delayed_silence_probe_does_not_reclassify_or_extend_the_hard_boundary() {
        let mut planner = IncrementalPlanner::default();
        planner.start(id(71));
        let plan = planner
            .observe(id(71), Duration::from_secs(5), true)
            .unwrap();

        assert_eq!(plan.boundary, BoundaryKind::Forced);
        assert_eq!(plan.range.end, Duration::from_secs(3));
        assert_eq!(plan.range.duration(), MAX_CHUNK_DURATION);
    }

    #[test]
    fn chunk_start_uses_the_previous_boundary_confidence() {
        let mut planner = IncrementalPlanner::default();
        planner.start(id(8));
        let forced = planner
            .observe(id(8), Duration::from_secs(3), false)
            .unwrap();
        assert_eq!(forced.start_overlap, None);
        planner.partial_completed(id(8), forced.sequence, true);

        let ending_at_silence = planner
            .observe(id(8), Duration::from_secs(5), true)
            .unwrap();
        assert_eq!(
            ending_at_silence.start_overlap,
            Some(MergeExpectation::LexicalOverlap)
        );
        planner.partial_completed(id(8), ending_at_silence.sequence, true);

        let ending_forced = planner
            .observe(id(8), Duration::from_secs(8), false)
            .unwrap();
        assert_eq!(ending_forced.start_overlap, Some(MergeExpectation::Silence));
    }

    #[test]
    fn failure_degrades_session_and_stale_completions_are_ignored() {
        let mut planner = IncrementalPlanner::default();
        planner.start(id(2));
        let plan = planner
            .observe(id(2), Duration::from_secs(3), false)
            .unwrap();
        planner.partial_completed(id(99), plan.sequence, true);
        assert_eq!(planner.active_id(), Some(id(2)));
        planner.partial_completed(id(2), plan.sequence, false);
        assert!(!planner.needs_probe(id(2), Duration::from_secs(20)));
        planner.finish(id(99));
        assert_eq!(planner.active_id(), Some(id(2)));
        planner.finish(id(2));
        assert_eq!(planner.active_id(), None);
    }

    #[test]
    fn cancel_clears_outstanding_work() {
        let mut planner = IncrementalPlanner::default();
        planner.start(id(3));
        planner
            .observe(id(3), Duration::from_secs(3), false)
            .unwrap();
        assert_eq!(planner.cancel(), Some(id(3)));
        assert_eq!(planner.active_id(), None);
    }

    #[test]
    fn two_minute_continuous_recording_stays_bounded_and_ordered() {
        let mut planner = IncrementalPlanner::default();
        planner.start(id(4));
        let mut plans = Vec::new();
        for second in 0..=120 {
            let captured = Duration::from_secs(second);
            if let Some(plan) = planner.observe(id(4), captured, false) {
                assert_eq!(plan.sequence as usize, plans.len());
                assert!(plan.range.duration() <= MAX_CHUNK_DURATION + CHUNK_OVERLAP);
                planner.partial_completed(id(4), plan.sequence, true);
                plans.push(plan);
            }
        }

        assert_eq!(plans.len(), 40);
        assert_eq!(plans.last().unwrap().stable_end, Duration::from_secs(120));
        assert!(plans.windows(2).all(|pair| {
            pair[1].range.start == pair[0].stable_end.saturating_sub(CHUNK_OVERLAP)
        }));
    }

    #[test]
    fn merges_confident_english_and_portuguese_overlap() {
        assert_eq!(
            merge_overlapping(
                "Please send the release notes",
                "the release notes tomorrow morning.",
                MergeExpectation::LexicalOverlap
            ),
            Ok("Please send the release notes tomorrow morning.".to_owned())
        );
        assert_eq!(
            merge_overlapping(
                "Isso funciona muito bem",
                "muito bem, mesmo em português.",
                MergeExpectation::LexicalOverlap
            ),
            Ok("Isso funciona muito bem, mesmo em português.".to_owned())
        );
    }

    #[test]
    fn punctuation_and_case_do_not_prevent_a_confident_merge() {
        assert_eq!(
            merge_overlapping(
                "Ship PHORMINX today!",
                "Phorminx today, after QA.",
                MergeExpectation::LexicalOverlap
            ),
            Ok("Ship Phorminx today, after QA.".to_owned())
        );
    }

    #[test]
    fn one_word_overlap_and_missing_forced_overlap_fail_closed() {
        assert_eq!(
            merge_overlapping("I said go", "go go now", MergeExpectation::LexicalOverlap),
            Err(MergeError::AmbiguousOverlap)
        );
        assert_eq!(
            merge_overlapping(
                "alpha beta",
                "gamma delta",
                MergeExpectation::LexicalOverlap
            ),
            Err(MergeError::MissingOverlap)
        );
    }

    #[test]
    fn silence_boundary_preserves_legitimate_repeated_words() {
        assert_eq!(
            merge_overlapping("I said go", "go now", MergeExpectation::Silence),
            Err(MergeError::AmbiguousOverlap)
        );
        assert_eq!(
            merge_overlapping(
                "First sentence.",
                "Second sentence.",
                MergeExpectation::Silence
            ),
            Ok("First sentence. Second sentence.".to_owned())
        );
    }

    #[test]
    fn repeated_multiword_phrase_is_ambiguous_instead_of_deleted() {
        assert_eq!(
            merge_overlapping(
                "go now go now",
                "go now please",
                MergeExpectation::LexicalOverlap
            ),
            Err(MergeError::AmbiguousOverlap)
        );
        assert_eq!(
            merge_overlapping(
                "we should go now",
                "go now go now please",
                MergeExpectation::LexicalOverlap
            ),
            Err(MergeError::AmbiguousOverlap)
        );
    }

    #[test]
    fn recognizes_only_whitelisted_non_speech_annotations_in_english_and_portuguese() {
        for annotation in [
            "[BLANK_AUDIO]",
            " [ blank audio ] ",
            "[No-Speech]",
            "[silence] [ MUSIC ]",
            "[SILÊNCIO]",
            "[ música ]",
            "[sem fala]",
            "[ÁUDIO_EM_BRANCO]",
        ] {
            assert!(is_non_speech_only(annotation), "{annotation}");
        }

        for dictation in [
            "",
            "[meeting]",
            "[código importante]",
            "say [silence] now",
            "[silence] continue",
            "silence",
        ] {
            assert!(!is_non_speech_only(dictation), "{dictation}");
        }
    }

    #[test]
    fn strips_known_annotations_around_speech_but_preserves_unknown_brackets() {
        assert_eq!(
            strip_known_non_speech_annotations("[silence] hello [BLANK_AUDIO] world [music]"),
            "hello world"
        );
        assert_eq!(
            strip_known_non_speech_annotations("hello [meeting notes] [BLANK_AUDIO] world"),
            "hello [meeting notes] world"
        );
        assert_eq!(
            strip_known_non_speech_annotations("[SEM FALA] [ áudio vazio ]"),
            ""
        );
        assert_eq!(
            strip_known_non_speech_annotations("  raw   spacing , untouched  "),
            "  raw   spacing , untouched  "
        );
        assert_eq!(
            strip_known_non_speech_annotations("hello  there [music]   world"),
            "hello  there world"
        );
        assert_eq!(
            strip_known_non_speech_annotations("hello [music] [meeting notes] world"),
            "hello [meeting notes] world"
        );
        assert_eq!(
            strip_known_non_speech_annotations("hello [music] [silence] world"),
            "hello world"
        );
    }
}
