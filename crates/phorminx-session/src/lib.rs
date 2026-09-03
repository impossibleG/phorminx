#![forbid(unsafe_code)]
//! Bounded, privacy-preserving primitives for extended Phorminx dictation sessions.
//!
//! This crate deliberately contains no logging. Callers may report content-free metrics,
//! but transcript text and audio samples must not be sent to logs.

mod audio;
mod ledger;
mod spool;

pub use audio::{AudioSpan, AudioSpanError, CANONICAL_SAMPLE_RATE, SampleRange};
pub use ledger::{
    ApplyOutcome, CheckpointKind, LedgerError, LedgerLimits, TranscriptBlock, TranscriptCheckpoint,
    TranscriptLedger, UnresolvedFrontier,
};
pub use spool::{
    EncryptedAudioSpool, FileStorage, ScavengeReport, SpoolError, SpoolQuota, SpoolStorage,
    scavenge_orphans,
};
