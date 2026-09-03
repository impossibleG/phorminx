use std::ops::Range;

use thiserror::Error;
use zeroize::Zeroizing;

/// The only sample rate accepted by the session layer.
pub const CANONICAL_SAMPLE_RATE: u32 = 16_000;

/// A half-open range of absolute canonical-audio sample indices.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SampleRange {
    start: u64,
    end: u64,
}

impl SampleRange {
    pub fn new(start: u64, end: u64) -> Result<Self, AudioSpanError> {
        if end < start {
            return Err(AudioSpanError::ReversedRange { start, end });
        }
        Ok(Self { start, end })
    }

    pub const fn start(self) -> u64 {
        self.start
    }

    pub const fn end(self) -> u64 {
        self.end
    }

    pub const fn len(self) -> u64 {
        self.end - self.start
    }

    pub const fn is_empty(self) -> bool {
        self.start == self.end
    }

    pub const fn contains(self, other: Self) -> bool {
        self.start <= other.start && other.end <= self.end
    }

    pub const fn intersects(self, other: Self) -> bool {
        self.start < other.end && other.start < self.end
    }
}

impl From<SampleRange> for Range<u64> {
    fn from(value: SampleRange) -> Self {
        value.start..value.end
    }
}

/// Owned canonical mono audio with absolute session coverage.
#[derive(Clone, Debug, PartialEq)]
pub struct AudioSpan {
    range: SampleRange,
    samples: Zeroizing<Vec<f32>>,
}

impl AudioSpan {
    /// Constructs a canonical 16 kHz span. The sample rate is intentionally not configurable.
    pub fn new(start_sample: u64, samples: Vec<f32>) -> Result<Self, AudioSpanError> {
        let mut samples = Zeroizing::new(samples);
        if samples.is_empty() {
            return Err(AudioSpanError::Empty);
        }
        if samples.iter().any(|sample| !sample.is_finite()) {
            return Err(AudioSpanError::NonFiniteSample);
        }
        let sample_count =
            u64::try_from(samples.len()).map_err(|_| AudioSpanError::LengthOverflow)?;
        let end = start_sample
            .checked_add(sample_count)
            .ok_or(AudioSpanError::LengthOverflow)?;
        Ok(Self {
            range: SampleRange::new(start_sample, end)?,
            samples: Zeroizing::new(std::mem::take(&mut *samples)),
        })
    }

    /// Rejects non-canonical input at the session boundary instead of silently resampling it.
    pub fn from_sample_rate(
        sample_rate: u32,
        start_sample: u64,
        samples: Vec<f32>,
    ) -> Result<Self, AudioSpanError> {
        let mut samples = Zeroizing::new(samples);
        if sample_rate != CANONICAL_SAMPLE_RATE {
            return Err(AudioSpanError::NonCanonicalSampleRate(sample_rate));
        }
        Self::new(start_sample, std::mem::take(&mut *samples))
    }

    pub const fn range(&self) -> SampleRange {
        self.range
    }

    pub fn samples(&self) -> &[f32] {
        &self.samples
    }

    /// Transfers ownership while retaining wipe-on-drop semantics for the recipient.
    pub fn into_samples(mut self) -> Zeroizing<Vec<f32>> {
        std::mem::replace(&mut self.samples, Zeroizing::new(Vec::new()))
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum AudioSpanError {
    #[error("audio span must contain at least one sample")]
    Empty,
    #[error("audio span contains a non-finite sample")]
    NonFiniteSample,
    #[error("audio span length overflows the absolute sample index")]
    LengthOverflow,
    #[error("sample range is reversed: {start}..{end}")]
    ReversedRange { start: u64, end: u64 },
    #[error("expected canonical 16000 Hz audio, got {0} Hz")]
    NonCanonicalSampleRate(u32),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn span_uses_absolute_canonical_coverage() {
        let span = AudioSpan::from_sample_rate(CANONICAL_SAMPLE_RATE, 41, vec![0.0; 3]).unwrap();
        assert_eq!(span.range(), SampleRange::new(41, 44).unwrap());
        let guarded_samples: Zeroizing<Vec<f32>> = span.into_samples();
        assert_eq!(&*guarded_samples, &[0.0; 3]);
    }

    #[test]
    fn rejects_wrong_rate_empty_nonfinite_and_overflow() {
        assert_eq!(
            AudioSpan::from_sample_rate(48_000, 0, vec![0.0]).unwrap_err(),
            AudioSpanError::NonCanonicalSampleRate(48_000)
        );
        assert_eq!(
            AudioSpan::new(0, vec![]).unwrap_err(),
            AudioSpanError::Empty
        );
        assert_eq!(
            AudioSpan::new(0, vec![f32::NAN]).unwrap_err(),
            AudioSpanError::NonFiniteSample
        );
        assert_eq!(
            AudioSpan::new(u64::MAX, vec![0.0]).unwrap_err(),
            AudioSpanError::LengthOverflow
        );
    }
}
