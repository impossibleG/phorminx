use thiserror::Error;

use crate::{AudioSpanError, SampleRange};

/// Hard bounds for a single transcript ledger.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LedgerLimits {
    pub max_samples: u64,
    pub max_events: usize,
    pub max_blocks: usize,
    pub max_text_bytes: usize,
}

impl Default for LedgerLimits {
    fn default() -> Self {
        Self {
            max_samples: u64::from(super::CANONICAL_SAMPLE_RATE) * 60 * 60 * 8,
            max_events: 65_536,
            max_blocks: 262_144,
            max_text_bytes: 16 * 1024 * 1024,
        }
    }
}

/// A transcript block. Silence is explicit so checkpoint coverage cannot hide holes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TranscriptBlock {
    Speech {
        range: SampleRange,
        separator_before: String,
        text: String,
    },
    Silence {
        range: SampleRange,
    },
}

impl TranscriptBlock {
    pub fn speech(range: SampleRange, text: impl Into<String>) -> Self {
        Self::speech_with_separator(range, "", text)
    }

    pub fn speech_with_separator(
        range: SampleRange,
        separator_before: impl Into<String>,
        text: impl Into<String>,
    ) -> Self {
        Self::Speech {
            range,
            separator_before: separator_before.into(),
            text: text.into(),
        }
    }

    pub const fn silence(range: SampleRange) -> Self {
        Self::Silence { range }
    }

    pub const fn range(&self) -> SampleRange {
        match self {
            Self::Speech { range, .. } | Self::Silence { range } => *range,
        }
    }

    pub fn text(&self) -> Option<&str> {
        match self {
            Self::Speech { text, .. } => Some(text),
            Self::Silence { .. } => None,
        }
    }

    pub fn separator_before(&self) -> Option<&str> {
        match self {
            Self::Speech {
                separator_before, ..
            } => Some(separator_before),
            Self::Silence { .. } => None,
        }
    }

    fn output_bytes(&self) -> usize {
        match self {
            Self::Speech {
                separator_before,
                text,
                ..
            } => separator_before.len().saturating_add(text.len()),
            Self::Silence { .. } => 0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CheckpointKind {
    Regular,
    Recovery {
        failed_generation: u64,
        failed_sequence: u64,
    },
}

/// An immutable recognition result covering one exact, contiguous audio interval.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TranscriptCheckpoint {
    pub generation: u64,
    pub sequence: u64,
    pub coverage: SampleRange,
    pub kind: CheckpointKind,
    pub blocks: Vec<TranscriptBlock>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UnresolvedFrontier {
    pub failed_generation: u64,
    pub failed_sequence: u64,
    pub from_sample: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApplyOutcome {
    Applied,
    Duplicate,
}

#[derive(Clone, Debug)]
enum Receipt {
    Applied { checkpoint_index: usize },
    Deferred { from_sample: u64 },
}

/// Append-only transcript state with explicit coverage and recovery invariants.
pub struct TranscriptLedger {
    origin_sample: u64,
    committed_through: u64,
    generation: u64,
    next_sequence: u64,
    unresolved: Option<UnresolvedFrontier>,
    checkpoints: Vec<TranscriptCheckpoint>,
    generation_receipts: Vec<Receipt>,
    text_bytes: usize,
    block_count: usize,
    event_count: usize,
    expected_final_end: Option<u64>,
    assembled: bool,
    limits: LedgerLimits,
}

impl TranscriptLedger {
    pub fn new(
        origin_sample: u64,
        generation: u64,
        limits: LedgerLimits,
    ) -> Result<Self, LedgerError> {
        if limits.max_samples == 0
            || limits.max_events == 0
            || limits.max_blocks == 0
            || limits.max_text_bytes == 0
        {
            return Err(LedgerError::InvalidLimits);
        }
        Ok(Self {
            origin_sample,
            committed_through: origin_sample,
            generation,
            next_sequence: 0,
            unresolved: None,
            checkpoints: Vec::new(),
            generation_receipts: Vec::new(),
            text_bytes: 0,
            block_count: 0,
            event_count: 0,
            expected_final_end: None,
            assembled: false,
            limits,
        })
    }

    pub const fn origin_sample(&self) -> u64 {
        self.origin_sample
    }

    pub const fn committed_through(&self) -> u64 {
        self.committed_through
    }

    pub const fn generation(&self) -> u64 {
        self.generation
    }

    pub const fn next_sequence(&self) -> u64 {
        self.next_sequence
    }

    pub const fn unresolved_frontier(&self) -> Option<UnresolvedFrontier> {
        self.unresolved
    }

    pub const fn expected_final_end(&self) -> Option<u64> {
        self.expected_final_end
    }

    pub fn checkpoints(&self) -> &[TranscriptCheckpoint] {
        &self.checkpoints
    }

    /// Seals the ledger with the capture subsystem's authoritative final sample frontier.
    /// Recognition recovery may continue up to this boundary, but assembly cannot occur before it.
    pub fn seal(&mut self, expected_final_end: u64) -> Result<(), LedgerError> {
        self.ensure_open()?;
        let captured_samples = expected_final_end
            .checked_sub(self.origin_sample)
            .ok_or(LedgerError::CoverageBeforeOrigin)?;
        if captured_samples > self.limits.max_samples {
            return Err(LedgerError::SampleLimitExceeded);
        }
        if expected_final_end < self.committed_through {
            return Err(LedgerError::FinalFrontierBehindCommitted {
                committed_through: self.committed_through,
                expected_final_end,
            });
        }
        match self.expected_final_end {
            Some(existing) if existing == expected_final_end => Ok(()),
            Some(existing) => Err(LedgerError::ConflictingFinalFrontier {
                existing,
                actual: expected_final_end,
            }),
            None => {
                self.expected_final_end = Some(expected_final_end);
                Ok(())
            }
        }
    }

    /// Advances to exactly the next worker generation and invalidates older in-flight results.
    pub fn advance_generation(&mut self, generation: u64) -> Result<(), LedgerError> {
        self.ensure_open()?;
        let expected = self
            .generation
            .checked_add(1)
            .ok_or(LedgerError::GenerationOverflow)?;
        if generation < expected {
            return Err(LedgerError::StaleGeneration {
                expected,
                actual: generation,
            });
        }
        if generation > expected {
            return Err(LedgerError::GenerationGap {
                expected,
                actual: generation,
            });
        }
        self.generation = generation;
        self.next_sequence = 0;
        self.generation_receipts.clear();
        Ok(())
    }

    /// Records failed recognition at the current frontier without pretending coverage advanced.
    pub fn defer(
        &mut self,
        generation: u64,
        sequence: u64,
        from_sample: u64,
    ) -> Result<ApplyOutcome, LedgerError> {
        self.ensure_open()?;
        if self.expected_final_end.is_some() {
            return Err(LedgerError::DeferredAfterSeal);
        }
        self.validate_generation(generation)?;
        if let Some(outcome) = self.replayed_defer(sequence, from_sample)? {
            return Ok(outcome);
        }
        self.validate_expected_sequence(sequence)?;
        self.preflight_event()?;
        if self.unresolved.is_some() {
            return Err(LedgerError::AlreadyUnresolved);
        }
        if from_sample != self.committed_through {
            return Err(LedgerError::NonContiguousCoverage {
                expected_start: self.committed_through,
                actual_start: from_sample,
            });
        }

        self.unresolved = Some(UnresolvedFrontier {
            failed_generation: generation,
            failed_sequence: sequence,
            from_sample,
        });
        self.generation_receipts
            .push(Receipt::Deferred { from_sample });
        self.next_sequence += 1;
        self.event_count += 1;
        Ok(ApplyOutcome::Applied)
    }

    pub fn apply(&mut self, checkpoint: TranscriptCheckpoint) -> Result<ApplyOutcome, LedgerError> {
        self.ensure_open()?;
        self.validate_generation(checkpoint.generation)?;
        if let Some(outcome) = self.replayed_checkpoint(&checkpoint)? {
            return Ok(outcome);
        }
        self.validate_expected_sequence(checkpoint.sequence)?;
        self.validate_checkpoint(&checkpoint)?;
        self.validate_checkpoint_kind(&checkpoint)?;
        self.preflight_checkpoint(&checkpoint)?;

        let new_text_bytes = checkpoint
            .blocks
            .iter()
            .map(TranscriptBlock::output_bytes)
            .sum::<usize>();
        let index = self.checkpoints.len();
        self.committed_through = checkpoint.coverage.end();
        if matches!(checkpoint.kind, CheckpointKind::Recovery { .. }) {
            self.unresolved = None;
        }
        self.text_bytes += new_text_bytes;
        self.block_count += checkpoint.blocks.len();
        self.event_count += 1;
        self.next_sequence += 1;
        self.checkpoints.push(checkpoint);
        self.generation_receipts.push(Receipt::Applied {
            checkpoint_index: index,
        });
        Ok(ApplyOutcome::Applied)
    }

    /// Produces the transcript once. Calling this is the finalization boundary.
    pub fn assemble_once(&mut self) -> Result<String, LedgerError> {
        self.ensure_open()?;
        if let Some(frontier) = self.unresolved {
            return Err(LedgerError::Unresolved(frontier));
        }
        let expected_final_end = self.expected_final_end.ok_or(LedgerError::Unsealed)?;
        if self.committed_through != expected_final_end {
            return Err(LedgerError::IncompleteFinalCoverage {
                committed_through: self.committed_through,
                expected_final_end,
            });
        }
        let mut output = String::new();
        output
            .try_reserve(self.text_bytes)
            .map_err(|_| LedgerError::AllocationFailed)?;
        for checkpoint in &self.checkpoints {
            for block in &checkpoint.blocks {
                if let TranscriptBlock::Speech {
                    separator_before,
                    text,
                    ..
                } = block
                {
                    output.push_str(separator_before);
                    output.push_str(text);
                }
            }
        }
        self.assembled = true;
        Ok(output)
    }

    fn ensure_open(&self) -> Result<(), LedgerError> {
        if self.assembled {
            Err(LedgerError::AlreadyAssembled)
        } else {
            Ok(())
        }
    }

    fn validate_generation(&self, actual: u64) -> Result<(), LedgerError> {
        if actual < self.generation {
            Err(LedgerError::StaleGeneration {
                expected: self.generation,
                actual,
            })
        } else if actual > self.generation {
            Err(LedgerError::FutureGeneration {
                expected: self.generation,
                actual,
            })
        } else {
            Ok(())
        }
    }

    fn validate_expected_sequence(&self, actual: u64) -> Result<(), LedgerError> {
        if actual > self.next_sequence {
            Err(LedgerError::SequenceGap {
                expected: self.next_sequence,
                actual,
            })
        } else if actual < self.next_sequence {
            Err(LedgerError::ConflictingReplay { sequence: actual })
        } else {
            Ok(())
        }
    }

    fn replayed_defer(
        &self,
        sequence: u64,
        from_sample: u64,
    ) -> Result<Option<ApplyOutcome>, LedgerError> {
        if sequence >= self.next_sequence {
            return Ok(None);
        }
        let index = usize::try_from(sequence)
            .ok()
            .filter(|index| *index < self.generation_receipts.len())
            .ok_or(LedgerError::ConflictingReplay { sequence })?;
        match self.generation_receipts[index] {
            Receipt::Deferred {
                from_sample: recorded,
            } if recorded == from_sample => Ok(Some(ApplyOutcome::Duplicate)),
            _ => Err(LedgerError::ConflictingReplay { sequence }),
        }
    }

    fn replayed_checkpoint(
        &self,
        checkpoint: &TranscriptCheckpoint,
    ) -> Result<Option<ApplyOutcome>, LedgerError> {
        if checkpoint.sequence >= self.next_sequence {
            return Ok(None);
        }
        let index = usize::try_from(checkpoint.sequence)
            .ok()
            .filter(|index| *index < self.generation_receipts.len())
            .ok_or(LedgerError::ConflictingReplay {
                sequence: checkpoint.sequence,
            })?;
        match self.generation_receipts[index] {
            Receipt::Applied { checkpoint_index }
                if self.checkpoints.get(checkpoint_index) == Some(checkpoint) =>
            {
                Ok(Some(ApplyOutcome::Duplicate))
            }
            _ => Err(LedgerError::ConflictingReplay {
                sequence: checkpoint.sequence,
            }),
        }
    }

    fn validate_checkpoint(&self, checkpoint: &TranscriptCheckpoint) -> Result<(), LedgerError> {
        if checkpoint.coverage.is_empty() {
            return Err(LedgerError::EmptyCoverage);
        }
        if checkpoint.coverage.start() != self.committed_through {
            return Err(LedgerError::NonContiguousCoverage {
                expected_start: self.committed_through,
                actual_start: checkpoint.coverage.start(),
            });
        }
        if checkpoint.blocks.is_empty() {
            return Err(LedgerError::MissingBlocks);
        }

        let mut expected = checkpoint.coverage.start();
        for block in &checkpoint.blocks {
            let range = block.range();
            if range.is_empty() {
                return Err(LedgerError::EmptyBlock);
            }
            if range.start() != expected {
                return Err(LedgerError::BlockCoverageGapOrOverlap {
                    expected_start: expected,
                    actual_start: range.start(),
                });
            }
            if !checkpoint.coverage.contains(range) {
                return Err(LedgerError::BlockOutsideCheckpoint);
            }
            if matches!(block, TranscriptBlock::Speech { text, .. } if text.is_empty()) {
                return Err(LedgerError::EmptySpeech);
            }
            expected = range.end();
        }
        if expected != checkpoint.coverage.end() {
            return Err(LedgerError::IncompleteBlockCoverage {
                expected_end: checkpoint.coverage.end(),
                actual_end: expected,
            });
        }
        Ok(())
    }

    fn validate_checkpoint_kind(
        &self,
        checkpoint: &TranscriptCheckpoint,
    ) -> Result<(), LedgerError> {
        match (self.unresolved, checkpoint.kind) {
            (None, CheckpointKind::Regular) => Ok(()),
            (Some(_), CheckpointKind::Regular) => Err(LedgerError::RecoveryRequired),
            (None, CheckpointKind::Recovery { .. }) => Err(LedgerError::UnexpectedRecovery),
            (
                Some(frontier),
                CheckpointKind::Recovery {
                    failed_generation,
                    failed_sequence,
                },
            ) if failed_generation == frontier.failed_generation
                && failed_sequence == frontier.failed_sequence
                && checkpoint.coverage.start() == frontier.from_sample =>
            {
                Ok(())
            }
            (Some(frontier), CheckpointKind::Recovery { .. }) => {
                Err(LedgerError::WrongRecovery { expected: frontier })
            }
        }
    }

    fn preflight_event(&self) -> Result<(), LedgerError> {
        if self.event_count >= self.limits.max_events {
            Err(LedgerError::EventLimitExceeded)
        } else {
            Ok(())
        }
    }

    fn preflight_checkpoint(&self, checkpoint: &TranscriptCheckpoint) -> Result<(), LedgerError> {
        self.preflight_event()?;
        if self
            .expected_final_end
            .is_some_and(|expected| checkpoint.coverage.end() > expected)
        {
            return Err(LedgerError::CheckpointBeyondFinalFrontier {
                expected_final_end: self.expected_final_end.expect("checked as present"),
                checkpoint_end: checkpoint.coverage.end(),
            });
        }
        let session_samples = checkpoint
            .coverage
            .end()
            .checked_sub(self.origin_sample)
            .ok_or(LedgerError::CoverageBeforeOrigin)?;
        if session_samples > self.limits.max_samples {
            return Err(LedgerError::SampleLimitExceeded);
        }
        let blocks = self
            .block_count
            .checked_add(checkpoint.blocks.len())
            .ok_or(LedgerError::BlockLimitExceeded)?;
        if blocks > self.limits.max_blocks {
            return Err(LedgerError::BlockLimitExceeded);
        }
        let new_text = checkpoint
            .blocks
            .iter()
            .try_fold(0usize, |sum, block| sum.checked_add(block.output_bytes()))
            .ok_or(LedgerError::TextLimitExceeded)?;
        let text_bytes = self
            .text_bytes
            .checked_add(new_text)
            .ok_or(LedgerError::TextLimitExceeded)?;
        if text_bytes > self.limits.max_text_bytes {
            return Err(LedgerError::TextLimitExceeded);
        }
        Ok(())
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum LedgerError {
    #[error("ledger limits must all be non-zero")]
    InvalidLimits,
    #[error("transcript has already been assembled")]
    AlreadyAssembled,
    #[error("capture frontier must be sealed before transcript assembly")]
    Unsealed,
    #[error("recognition failure cannot be deferred after the capture frontier is sealed")]
    DeferredAfterSeal,
    #[error(
        "transcript coverage ends at {committed_through}, but capture ended at {expected_final_end}"
    )]
    IncompleteFinalCoverage {
        committed_through: u64,
        expected_final_end: u64,
    },
    #[error(
        "capture frontier {expected_final_end} is behind committed transcript {committed_through}"
    )]
    FinalFrontierBehindCommitted {
        committed_through: u64,
        expected_final_end: u64,
    },
    #[error("capture frontier was already sealed at {existing}, not {actual}")]
    ConflictingFinalFrontier { existing: u64, actual: u64 },
    #[error(
        "checkpoint ends at {checkpoint_end}, beyond sealed capture frontier {expected_final_end}"
    )]
    CheckpointBeyondFinalFrontier {
        expected_final_end: u64,
        checkpoint_end: u64,
    },
    #[error("worker generation overflow")]
    GenerationOverflow,
    #[error("stale generation {actual}; current generation is {expected}")]
    StaleGeneration { expected: u64, actual: u64 },
    #[error("future generation {actual}; current generation is {expected}")]
    FutureGeneration { expected: u64, actual: u64 },
    #[error("generation gap: expected {expected}, got {actual}")]
    GenerationGap { expected: u64, actual: u64 },
    #[error("sequence gap: expected {expected}, got {actual}")]
    SequenceGap { expected: u64, actual: u64 },
    #[error("sequence {sequence} was replayed with different content or event type")]
    ConflictingReplay { sequence: u64 },
    #[error("coverage must start at {expected_start}, got {actual_start}")]
    NonContiguousCoverage {
        expected_start: u64,
        actual_start: u64,
    },
    #[error("checkpoint coverage must not be empty")]
    EmptyCoverage,
    #[error("checkpoint must contain explicit speech or silence blocks")]
    MissingBlocks,
    #[error("transcript block coverage must not be empty")]
    EmptyBlock,
    #[error("block coverage must start at {expected_start}, got {actual_start}")]
    BlockCoverageGapOrOverlap {
        expected_start: u64,
        actual_start: u64,
    },
    #[error("transcript block is outside its checkpoint")]
    BlockOutsideCheckpoint,
    #[error("speech block text must not be empty")]
    EmptySpeech,
    #[error("block coverage ends at {actual_end}, expected {expected_end}")]
    IncompleteBlockCoverage { expected_end: u64, actual_end: u64 },
    #[error("a recovery checkpoint is required at the unresolved frontier")]
    RecoveryRequired,
    #[error("received a recovery checkpoint without an unresolved frontier")]
    UnexpectedRecovery,
    #[error("recovery does not match unresolved frontier {expected:?}")]
    WrongRecovery { expected: UnresolvedFrontier },
    #[error("an unresolved frontier already exists")]
    AlreadyUnresolved,
    #[error("transcript remains unresolved at {0:?}")]
    Unresolved(UnresolvedFrontier),
    #[error("checkpoint coverage precedes the ledger origin")]
    CoverageBeforeOrigin,
    #[error("session sample limit exceeded")]
    SampleLimitExceeded,
    #[error("ledger event limit exceeded")]
    EventLimitExceeded,
    #[error("transcript block limit exceeded")]
    BlockLimitExceeded,
    #[error("transcript text limit exceeded")]
    TextLimitExceeded,
    #[error("transcript allocation failed")]
    AllocationFailed,
    #[error(transparent)]
    InvalidRange(#[from] AudioSpanError),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn range(start: u64, end: u64) -> SampleRange {
        SampleRange::new(start, end).unwrap()
    }

    fn checkpoint(
        generation: u64,
        sequence: u64,
        start: u64,
        end: u64,
        text: &str,
    ) -> TranscriptCheckpoint {
        TranscriptCheckpoint {
            generation,
            sequence,
            coverage: range(start, end),
            kind: CheckpointKind::Regular,
            blocks: vec![TranscriptBlock::speech_with_separator(
                range(start, end),
                if sequence == 0 { "" } else { " " },
                text,
            )],
        }
    }

    fn checkpoint_with_separator(
        generation: u64,
        sequence: u64,
        start: u64,
        end: u64,
        separator: &str,
        text: &str,
    ) -> TranscriptCheckpoint {
        TranscriptCheckpoint {
            generation,
            sequence,
            coverage: range(start, end),
            kind: CheckpointKind::Regular,
            blocks: vec![TranscriptBlock::speech_with_separator(
                range(start, end),
                separator,
                text,
            )],
        }
    }

    fn ledger() -> TranscriptLedger {
        TranscriptLedger::new(0, 7, LedgerLimits::default()).unwrap()
    }

    #[test]
    fn contiguous_commits_assemble_exactly_once() {
        let mut ledger = ledger();
        assert_eq!(
            ledger.apply(checkpoint(7, 0, 0, 10, "hello")).unwrap(),
            ApplyOutcome::Applied
        );
        assert_eq!(
            ledger.apply(checkpoint(7, 1, 10, 20, "world.")).unwrap(),
            ApplyOutcome::Applied
        );
        assert_eq!(ledger.committed_through(), 20);
        ledger.seal(20).unwrap();
        assert_eq!(ledger.assemble_once().unwrap(), "hello world.");
        assert_eq!(
            ledger.assemble_once().unwrap_err(),
            LedgerError::AlreadyAssembled
        );
        assert_eq!(
            ledger.apply(checkpoint(7, 2, 20, 30, "late")).unwrap_err(),
            LedgerError::AlreadyAssembled
        );
    }

    #[test]
    fn duplicate_is_idempotent_but_conflicting_replay_is_rejected() {
        let mut ledger = ledger();
        let first = checkpoint(7, 0, 0, 10, "same");
        assert_eq!(ledger.apply(first.clone()).unwrap(), ApplyOutcome::Applied);
        assert_eq!(ledger.apply(first).unwrap(), ApplyOutcome::Duplicate);
        assert_eq!(ledger.committed_through(), 10);
        assert_eq!(ledger.checkpoints().len(), 1);
        assert_eq!(
            ledger
                .apply(checkpoint(7, 0, 0, 10, "different"))
                .unwrap_err(),
            LedgerError::ConflictingReplay { sequence: 0 }
        );
    }

    #[test]
    fn rejects_checkpoint_and_block_gaps_and_overlaps_without_mutation() {
        let mut ledger = ledger();
        ledger.apply(checkpoint(7, 0, 0, 10, "ok")).unwrap();
        for actual_start in [9, 11] {
            let bad = checkpoint(7, 1, actual_start, 20, "bad");
            assert!(matches!(
                ledger.apply(bad),
                Err(LedgerError::NonContiguousCoverage { .. })
            ));
            assert_eq!(ledger.committed_through(), 10);
            assert_eq!(ledger.next_sequence(), 1);
        }

        for second_start in [19, 21] {
            let bad_blocks = TranscriptCheckpoint {
                generation: 7,
                sequence: 1,
                coverage: range(10, 30),
                kind: CheckpointKind::Regular,
                blocks: vec![
                    TranscriptBlock::speech(range(10, 20), "a"),
                    TranscriptBlock::speech(range(second_start, 30), "b"),
                ],
            };
            assert!(matches!(
                ledger.apply(bad_blocks),
                Err(LedgerError::BlockCoverageGapOrOverlap { .. })
            ));
            assert_eq!(ledger.committed_through(), 10);
        }
    }

    #[test]
    fn explicit_silence_preserves_exact_coverage() {
        let mut ledger = ledger();
        let cp = TranscriptCheckpoint {
            generation: 7,
            sequence: 0,
            coverage: range(0, 30),
            kind: CheckpointKind::Regular,
            blocks: vec![
                TranscriptBlock::silence(range(0, 10)),
                TranscriptBlock::speech(range(10, 20), "spoken"),
                TranscriptBlock::silence(range(20, 30)),
            ],
        };
        ledger.apply(cp).unwrap();
        ledger.seal(30).unwrap();
        assert_eq!(ledger.assemble_once().unwrap(), "spoken");
    }

    #[test]
    fn assembly_uses_only_explicit_boundaries_for_punctuation_and_unicode() {
        let mut punctuation = ledger();
        punctuation
            .apply(checkpoint_with_separator(7, 0, 0, 10, "", "hello"))
            .unwrap();
        punctuation
            .apply(checkpoint_with_separator(7, 1, 10, 20, "", ","))
            .unwrap();
        punctuation.seal(20).unwrap();
        assert_eq!(punctuation.assemble_once().unwrap(), "hello,");

        let mut chinese = ledger();
        chinese
            .apply(checkpoint_with_separator(7, 0, 0, 10, "", "你"))
            .unwrap();
        chinese
            .apply(checkpoint_with_separator(7, 1, 10, 20, "", "好"))
            .unwrap();
        chinese.seal(20).unwrap();
        assert_eq!(chinese.assemble_once().unwrap(), "你好");
    }

    #[test]
    fn explicit_separator_bytes_count_toward_text_limit() {
        let limits = LedgerLimits {
            max_samples: 20,
            max_events: 2,
            max_blocks: 2,
            max_text_bytes: 3,
        };
        let mut ledger = TranscriptLedger::new(0, 7, limits).unwrap();
        ledger
            .apply(checkpoint_with_separator(7, 0, 0, 10, "", "a"))
            .unwrap();
        assert_eq!(
            ledger
                .apply(checkpoint_with_separator(7, 1, 10, 20, "  ", "b"))
                .unwrap_err(),
            LedgerError::TextLimitExceeded
        );
        assert_eq!(ledger.committed_through(), 10);
    }

    #[test]
    fn unresolved_frontier_requires_matching_recovery() {
        let mut ledger = ledger();
        ledger.apply(checkpoint(7, 0, 0, 10, "before")).unwrap();
        assert_eq!(ledger.defer(7, 1, 10).unwrap(), ApplyOutcome::Applied);
        assert_eq!(ledger.defer(7, 1, 10).unwrap(), ApplyOutcome::Duplicate);
        assert_eq!(
            ledger
                .apply(checkpoint(7, 2, 10, 20, "ordinary"))
                .unwrap_err(),
            LedgerError::RecoveryRequired
        );

        let mut recovery = checkpoint(7, 2, 10, 20, "recovered");
        recovery.kind = CheckpointKind::Recovery {
            failed_generation: 7,
            failed_sequence: 1,
        };
        ledger.apply(recovery).unwrap();
        assert_eq!(ledger.unresolved_frontier(), None);
        ledger.seal(20).unwrap();
        assert_eq!(ledger.assemble_once().unwrap(), "before recovered");
    }

    #[test]
    fn assembly_requires_authoritative_complete_capture_frontier() {
        let mut empty = ledger();
        assert_eq!(empty.assemble_once().unwrap_err(), LedgerError::Unsealed);
        empty.seal(200).unwrap();
        assert_eq!(
            empty.assemble_once().unwrap_err(),
            LedgerError::IncompleteFinalCoverage {
                committed_through: 0,
                expected_final_end: 200,
            }
        );

        let mut prefix = ledger();
        prefix.apply(checkpoint(7, 0, 0, 100, "prefix")).unwrap();
        prefix.seal(200).unwrap();
        assert_eq!(
            prefix.assemble_once().unwrap_err(),
            LedgerError::IncompleteFinalCoverage {
                committed_through: 100,
                expected_final_end: 200,
            }
        );
        assert_eq!(
            prefix
                .apply(checkpoint(7, 1, 100, 201, "too far"))
                .unwrap_err(),
            LedgerError::CheckpointBeyondFinalFrontier {
                expected_final_end: 200,
                checkpoint_end: 201,
            }
        );
    }

    #[test]
    fn zero_length_capture_may_seal_and_assemble_empty_once() {
        let mut ledger = ledger();
        ledger.seal(0).unwrap();
        assert_eq!(ledger.assemble_once().unwrap(), "");
        assert_eq!(
            ledger.assemble_once().unwrap_err(),
            LedgerError::AlreadyAssembled
        );
    }

    #[test]
    fn sealing_rejects_frontiers_outside_session_bounds() {
        let limits = LedgerLimits {
            max_samples: 100,
            ..LedgerLimits::default()
        };
        let mut ledger = TranscriptLedger::new(50, 0, limits).unwrap();
        assert_eq!(
            ledger.seal(49).unwrap_err(),
            LedgerError::CoverageBeforeOrigin
        );
        assert_eq!(
            ledger.seal(151).unwrap_err(),
            LedgerError::SampleLimitExceeded
        );
        assert_eq!(ledger.expected_final_end(), None);
        ledger.seal(150).unwrap();
        assert_eq!(ledger.expected_final_end(), Some(150));
    }

    #[test]
    fn defer_after_seal_is_rejected_without_mutating_receipts() {
        let mut ledger = ledger();
        ledger.seal(100).unwrap();
        assert_eq!(ledger.defer(7, 0, 0), Err(LedgerError::DeferredAfterSeal));
        assert_eq!(ledger.next_sequence(), 0);
        assert_eq!(ledger.unresolved_frontier(), None);
        assert_eq!(ledger.expected_final_end(), Some(100));
    }

    #[test]
    fn generation_advance_invalidates_stale_work_and_is_gap_checked() {
        let mut ledger = ledger();
        ledger.apply(checkpoint(7, 0, 0, 10, "one")).unwrap();
        assert_eq!(
            ledger.advance_generation(9).unwrap_err(),
            LedgerError::GenerationGap {
                expected: 8,
                actual: 9
            }
        );
        ledger.advance_generation(8).unwrap();
        assert_eq!(ledger.next_sequence(), 0);
        assert!(matches!(
            ledger.apply(checkpoint(7, 1, 10, 20, "stale")),
            Err(LedgerError::StaleGeneration { .. })
        ));
        ledger.apply(checkpoint(8, 0, 10, 20, "two")).unwrap();
    }

    #[test]
    fn unresolved_work_can_be_recovered_by_the_next_generation() {
        let mut ledger = ledger();
        ledger.defer(7, 0, 0).unwrap();
        ledger.advance_generation(8).unwrap();
        let mut recovery = checkpoint(8, 0, 0, 10, "recovered");
        recovery.kind = CheckpointKind::Recovery {
            failed_generation: 7,
            failed_sequence: 0,
        };
        ledger.apply(recovery).unwrap();
        assert_eq!(ledger.unresolved_frontier(), None);
        assert_eq!(ledger.committed_through(), 10);
    }

    #[test]
    fn future_sequence_and_generation_results_do_not_mutate_state() {
        let mut ledger = ledger();
        assert_eq!(
            ledger.apply(checkpoint(7, 1, 0, 10, "gap")).unwrap_err(),
            LedgerError::SequenceGap {
                expected: 0,
                actual: 1
            }
        );
        assert_eq!(
            ledger.apply(checkpoint(8, 0, 0, 10, "future")).unwrap_err(),
            LedgerError::FutureGeneration {
                expected: 7,
                actual: 8
            }
        );
        assert_eq!(ledger.committed_through(), 0);
        assert_eq!(ledger.next_sequence(), 0);
        assert!(ledger.checkpoints().is_empty());
    }

    #[test]
    fn limits_fail_before_mutating_state() {
        let limits = LedgerLimits {
            max_samples: 10,
            max_events: 1,
            max_blocks: 1,
            max_text_bytes: 3,
        };
        let mut ledger = TranscriptLedger::new(100, 0, limits).unwrap();
        assert_eq!(
            ledger.apply(checkpoint(0, 0, 100, 111, "abc")).unwrap_err(),
            LedgerError::SampleLimitExceeded
        );
        assert_eq!(ledger.committed_through(), 100);
        assert_eq!(ledger.next_sequence(), 0);
        assert_eq!(
            ledger
                .apply(checkpoint(0, 0, 100, 110, "abcd"))
                .unwrap_err(),
            LedgerError::TextLimitExceeded
        );
        assert_eq!(ledger.next_sequence(), 0);
    }

    #[test]
    fn randomized_invalid_results_never_move_the_frontier() {
        let mut ledger = ledger();
        let mut state = 0x5eed_u64;
        let mut frontier = 0_u64;
        for sequence in 0..2_000_u64 {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1);
            let width = 1 + (state % 127);
            let valid = checkpoint(7, sequence, frontier, frontier + width, "x");

            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1);
            if state & 3 == 0 {
                let offset = if state & 4 == 0 { 1 } else { u64::MAX };
                let bad_start = if offset == 1 {
                    frontier + 1
                } else {
                    frontier.saturating_sub(1)
                };
                let bad = checkpoint(7, sequence, bad_start, frontier + width, "bad");
                assert!(ledger.apply(bad).is_err());
                assert_eq!(ledger.committed_through(), frontier);
                assert_eq!(ledger.next_sequence(), sequence);
            }

            ledger.apply(valid.clone()).unwrap();
            assert_eq!(ledger.apply(valid).unwrap(), ApplyOutcome::Duplicate);
            frontier += width;
            assert_eq!(ledger.committed_through(), frontier);
        }
    }

    #[test]
    fn large_eight_hour_session_stays_within_declared_bounds() {
        let mut ledger = ledger();
        let chunk = u64::from(super::super::CANONICAL_SAMPLE_RATE) * 60;
        let mut start = 0;
        for sequence in 0..480_u64 {
            let end = start + chunk;
            ledger
                .apply(checkpoint(7, sequence, start, end, "minute"))
                .unwrap();
            start = end;
        }
        assert_eq!(
            start,
            u64::from(super::super::CANONICAL_SAMPLE_RATE) * 60 * 60 * 8
        );
        assert_eq!(ledger.checkpoints().len(), 480);
    }
}
