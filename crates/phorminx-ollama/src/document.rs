use std::ops::Range;

use crate::{ModelName, ProtectedTokens};

const DEFAULT_TARGET_BYTES: usize = 8 * 1024;
const DEFAULT_MAXIMUM_CHUNK_BYTES: usize = 12 * 1024;
const DEFAULT_MAXIMUM_DOCUMENT_BYTES: usize = 256 * 1024;
const MINIMUM_TARGET_BYTES: usize = 32;
const MINIMUM_CHUNK_BYTES: usize = 48;
const MAXIMUM_CHUNK_COUNT: usize = 2_048;
pub const MAXIMUM_DOCUMENT_OUTPUT_BYTES: usize = 768 * 1024;

/// Bounded policy for splitting a long transcript before local formatting.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DocumentChunkPolicy {
    pub target_bytes: usize,
    pub maximum_chunk_bytes: usize,
    pub maximum_document_bytes: usize,
}

impl Default for DocumentChunkPolicy {
    fn default() -> Self {
        Self {
            target_bytes: DEFAULT_TARGET_BYTES,
            maximum_chunk_bytes: DEFAULT_MAXIMUM_CHUNK_BYTES,
            maximum_document_bytes: DEFAULT_MAXIMUM_DOCUMENT_BYTES,
        }
    }
}

impl DocumentChunkPolicy {
    fn validate(self) -> Result<Self, DocumentChunkError> {
        if self.target_bytes < MINIMUM_TARGET_BYTES
            || self.maximum_chunk_bytes < MINIMUM_CHUNK_BYTES
            || self.maximum_document_bytes == 0
            || self.target_bytes > self.maximum_chunk_bytes
            || self.maximum_chunk_bytes > self.maximum_document_bytes
            || self.maximum_document_bytes > DEFAULT_MAXIMUM_DOCUMENT_BYTES
        {
            return Err(DocumentChunkError::InvalidPolicy);
        }
        Ok(self)
    }
}

/// One exact, ordered source slice. Concatenating every chunk reproduces the
/// input byte-for-byte, including whitespace and paragraph separators.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DocumentChunk {
    pub sequence: u32,
    pub source_range: Range<usize>,
    pub text: String,
    /// Exact source whitespace between this formatting unit and the next one.
    /// It is never sent through a model and is restored during assembly.
    pub separator_after: String,
}

/// Content-free outcome for one source chunk. The category contains neither
/// transcript/model output nor transport error text.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DocumentChunkOutcome {
    Formatted,
    RawBypass,
    SeparatorOnly,
    FallbackPromptRejected,
    FallbackRequestFailed,
    FallbackOutputRejected,
    FallbackCancelled,
    SourceAfterCancellation,
    FallbackServiceUnavailable,
    SourceAfterServiceLoss,
    SourceAfterOutputLimit,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DocumentChunkReport {
    pub sequence: u32,
    pub source_bytes: usize,
    pub output_bytes: usize,
    pub outcome: DocumentChunkOutcome,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DocumentOutcomeCounts {
    pub formatted: usize,
    pub raw_bypass: usize,
    pub separator_only: usize,
    pub prompt_rejected: usize,
    pub request_failed: usize,
    pub output_rejected: usize,
    pub cancelled: usize,
    pub source_after_cancellation: usize,
    pub service_unavailable: usize,
    pub source_after_service_loss: usize,
    pub source_after_output_limit: usize,
}

impl DocumentOutcomeCounts {
    #[must_use]
    pub fn from_reports(reports: &[DocumentChunkReport]) -> Self {
        let mut counts = Self::default();
        for report in reports {
            match report.outcome {
                DocumentChunkOutcome::Formatted => counts.formatted += 1,
                DocumentChunkOutcome::RawBypass => counts.raw_bypass += 1,
                DocumentChunkOutcome::SeparatorOnly => counts.separator_only += 1,
                DocumentChunkOutcome::FallbackPromptRejected => counts.prompt_rejected += 1,
                DocumentChunkOutcome::FallbackRequestFailed => counts.request_failed += 1,
                DocumentChunkOutcome::FallbackOutputRejected => counts.output_rejected += 1,
                DocumentChunkOutcome::FallbackCancelled => counts.cancelled += 1,
                DocumentChunkOutcome::SourceAfterCancellation => {
                    counts.source_after_cancellation += 1;
                }
                DocumentChunkOutcome::FallbackServiceUnavailable => {
                    counts.service_unavailable += 1;
                }
                DocumentChunkOutcome::SourceAfterServiceLoss => {
                    counts.source_after_service_loss += 1;
                }
                DocumentChunkOutcome::SourceAfterOutputLimit => {
                    counts.source_after_output_limit += 1;
                }
            }
        }
        counts
    }

    #[must_use]
    pub const fn total(self) -> usize {
        self.formatted
            + self.raw_bypass
            + self.separator_only
            + self.prompt_rejected
            + self.request_failed
            + self.output_rejected
            + self.cancelled
            + self.source_after_cancellation
            + self.service_unavailable
            + self.source_after_service_loss
            + self.source_after_output_limit
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DocumentFormatDisposition {
    Completed,
    PartiallyFormatted,
    RawBypass,
    Cancelled,
    ServiceLost,
    SourceFallbackOutputLimit,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DocumentFormatResult {
    pub text: String,
    pub model: Option<ModelName>,
    pub disposition: DocumentFormatDisposition,
    pub chunks: Vec<DocumentChunkReport>,
    pub counts: DocumentOutcomeCounts,
}

impl DocumentFormatResult {
    pub(crate) fn new(
        text: String,
        model: Option<ModelName>,
        disposition: DocumentFormatDisposition,
        chunks: Vec<DocumentChunkReport>,
    ) -> Self {
        let counts = DocumentOutcomeCounts::from_reports(&chunks);
        Self {
            text,
            model,
            disposition,
            chunks,
            counts,
        }
    }

    #[must_use]
    pub fn is_complete(&self) -> bool {
        matches!(
            self.disposition,
            DocumentFormatDisposition::Completed | DocumentFormatDisposition::RawBypass
        )
    }
}

/// Splits a transcript on natural boundaries without splitting UTF-8 or any
/// token protected by the formatting validator.
pub fn chunk_document(
    input: &str,
    policy: DocumentChunkPolicy,
) -> Result<Vec<DocumentChunk>, DocumentChunkError> {
    let policy = policy.validate()?;
    if input.len() > policy.maximum_document_bytes {
        return Err(DocumentChunkError::DocumentTooLarge {
            actual: input.len(),
            maximum: policy.maximum_document_bytes,
        });
    }
    if input.is_empty() {
        return Ok(Vec::new());
    }

    let protected = protected_ranges(input);
    let mut chunks = Vec::new();
    let estimated_chunks = input
        .len()
        .div_ceil(policy.target_bytes)
        .min(MAXIMUM_CHUNK_COUNT);
    chunks
        .try_reserve(estimated_chunks)
        .map_err(|_| DocumentChunkError::AllocationFailed)?;
    let mut start = 0;
    while start < input.len() {
        if chunks.len() == MAXIMUM_CHUNK_COUNT {
            return Err(DocumentChunkError::TooManyChunks);
        }
        if input[start..]
            .chars()
            .next()
            .is_some_and(char::is_whitespace)
        {
            let end = whitespace_run_end(input, start);
            let sequence =
                u32::try_from(chunks.len()).map_err(|_| DocumentChunkError::TooManyChunks)?;
            chunks.push(DocumentChunk {
                sequence,
                source_range: start..end,
                text: String::new(),
                separator_after: input[start..end].to_owned(),
            });
            start = end;
            continue;
        }
        let remaining = input.len() - start;
        let split = if remaining <= policy.maximum_chunk_bytes {
            input.len()
        } else {
            choose_boundary(input, start, policy, &protected)?
        };
        let (text_end, end) = isolate_separator(input, start, split);
        let sequence =
            u32::try_from(chunks.len()).map_err(|_| DocumentChunkError::TooManyChunks)?;
        chunks.push(DocumentChunk {
            sequence,
            source_range: start..end,
            text: input[start..text_end].to_owned(),
            separator_after: input[text_end..end].to_owned(),
        });
        start = end;
    }
    Ok(chunks)
}

pub fn reconstruct_document(chunks: &[DocumentChunk]) -> Result<String, DocumentChunkError> {
    let mut expected_start = 0;
    let mut output = String::new();
    for (index, chunk) in chunks.iter().enumerate() {
        if chunk.sequence as usize != index
            || chunk.source_range.start != expected_start
            || chunk.source_range.end < chunk.source_range.start
            || chunk.source_range.end - chunk.source_range.start
                != chunk.text.len() + chunk.separator_after.len()
        {
            return Err(DocumentChunkError::InvalidAssembly);
        }
        output.push_str(&chunk.text);
        output.push_str(&chunk.separator_after);
        expected_start = chunk.source_range.end;
    }
    Ok(output)
}

fn isolate_separator(input: &str, start: usize, split: usize) -> (usize, usize) {
    let mut text_end = split;
    if input[start..split]
        .chars()
        .next_back()
        .is_some_and(char::is_whitespace)
    {
        while text_end > start {
            let previous = input[..text_end]
                .chars()
                .next_back()
                .expect("nonempty prefix");
            if !previous.is_whitespace() {
                break;
            }
            text_end -= previous.len_utf8();
        }
    }
    // Separators never enter the model. Keep a whitespace run indivisible even
    // when the requested boundary lands in its middle; its size is still
    // bounded by the document limit.
    let mut end = split;
    if text_end < split {
        end = text_end;
    }
    while end < input.len() {
        let character = input[end..].chars().next().expect("nonempty suffix");
        if !character.is_whitespace() {
            break;
        }
        end += character.len_utf8();
    }
    (text_end, end)
}

fn whitespace_run_end(input: &str, start: usize) -> usize {
    let mut end = start;
    while end < input.len() {
        let character = input[end..].chars().next().expect("nonempty suffix");
        if !character.is_whitespace() {
            break;
        }
        end += character.len_utf8();
    }
    end
}

fn choose_boundary(
    input: &str,
    start: usize,
    policy: DocumentChunkPolicy,
    protected: &[Range<usize>],
) -> Result<usize, DocumentChunkError> {
    let target = floor_char_boundary(input, start.saturating_add(policy.target_bytes));
    let hard = floor_char_boundary(input, start.saturating_add(policy.maximum_chunk_bytes));
    let minimum = floor_char_boundary(input, start.saturating_add(policy.target_bytes / 2));

    for kind in [
        BoundaryKind::Paragraph,
        BoundaryKind::Sentence,
        BoundaryKind::Whitespace,
    ] {
        if let Some(boundary) = preferred_boundary(input, start, minimum, target, hard, kind)
            && !inside_protected(boundary, protected)
        {
            return Ok(boundary);
        }
    }

    let mut boundary = hard;
    while boundary > start && inside_protected(boundary, protected) {
        boundary = previous_char_boundary(input, boundary);
    }
    if boundary == start {
        return Err(DocumentChunkError::UnsplittableSpan {
            start,
            maximum: policy.maximum_chunk_bytes,
        });
    }
    Ok(boundary)
}

#[derive(Clone, Copy)]
enum BoundaryKind {
    Paragraph,
    Sentence,
    Whitespace,
}

fn preferred_boundary(
    input: &str,
    start: usize,
    minimum: usize,
    target: usize,
    hard: usize,
    kind: BoundaryKind,
) -> Option<usize> {
    let candidates = input[start..hard]
        .char_indices()
        .filter_map(|(offset, character)| {
            let index = start + offset;
            let end = index + character.len_utf8();
            let matches = match kind {
                BoundaryKind::Paragraph => character == '\n' && input[end..].starts_with('\n'),
                BoundaryKind::Sentence => {
                    matches!(character, '.' | '!' | '?')
                        && input[end..].chars().next().is_none_or(char::is_whitespace)
                }
                BoundaryKind::Whitespace => character.is_whitespace(),
            };
            matches.then_some(end)
        })
        .filter(|candidate| *candidate >= minimum)
        .collect::<Vec<_>>();

    candidates
        .iter()
        .copied()
        .rfind(|candidate| *candidate <= target)
        .or_else(|| candidates.into_iter().find(|candidate| *candidate > target))
}

fn protected_ranges(input: &str) -> Vec<Range<usize>> {
    ProtectedTokens::extract_with_ranges(input).1
}

fn inside_protected(boundary: usize, protected: &[Range<usize>]) -> bool {
    protected
        .iter()
        .any(|range| range.start < boundary && boundary < range.end)
}

fn floor_char_boundary(input: &str, requested: usize) -> usize {
    let mut boundary = requested.min(input.len());
    while !input.is_char_boundary(boundary) {
        boundary -= 1;
    }
    boundary
}

fn previous_char_boundary(input: &str, current: usize) -> usize {
    let mut boundary = current.saturating_sub(1);
    while !input.is_char_boundary(boundary) {
        boundary -= 1;
    }
    boundary
}

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum DocumentChunkError {
    #[error("the document chunking policy is invalid")]
    InvalidPolicy,
    #[error("document is {actual} bytes; maximum is {maximum}")]
    DocumentTooLarge { actual: usize, maximum: usize },
    #[error("document requires too many chunks")]
    TooManyChunks,
    #[error("memory could not be reserved for document chunks")]
    AllocationFailed,
    #[error("cannot split source at byte {start} within the {maximum}-byte chunk bound")]
    UnsplittableSpan { start: usize, maximum: usize },
    #[error("document chunks are missing, reordered, overlapping, or malformed")]
    InvalidAssembly,
}

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum DocumentFormatError {
    #[error(transparent)]
    Chunk(#[from] DocumentChunkError),
    #[error("source document is {actual} bytes; safe output maximum is {maximum}")]
    SourceExceedsOutputLimit { actual: usize, maximum: usize },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tiny_policy() -> DocumentChunkPolicy {
        DocumentChunkPolicy {
            target_bytes: 32,
            maximum_chunk_bytes: 48,
            maximum_document_bytes: 4_096,
        }
    }

    #[test]
    fn exact_reconstruction_preserves_every_separator() {
        let input = "First paragraph has words.\n\nSecond paragraph has more words! And a tail.";
        let chunks = chunk_document(input, tiny_policy()).unwrap();
        assert!(chunks.len() > 1);
        assert!(chunks.iter().all(|chunk| chunk.text.len() <= 48));
        assert_eq!(reconstruct_document(&chunks).unwrap(), input);
        assert!(chunks.iter().any(|chunk| chunk.separator_after == "\n\n"));
    }

    #[test]
    fn trailing_and_separator_only_whitespace_stays_outside_text() {
        let trailing = chunk_document("alpha \t\r\n", tiny_policy()).unwrap();
        assert_eq!(trailing[0].text, "alpha");
        assert_eq!(trailing[0].separator_after, " \t\r\n");
        assert_eq!(reconstruct_document(&trailing).unwrap(), "alpha \t\r\n");

        let only = chunk_document(" \t\r\n", tiny_policy()).unwrap();
        assert_eq!(only[0].text, "");
        assert_eq!(only[0].separator_after, " \t\r\n");
        assert_eq!(reconstruct_document(&only).unwrap(), " \t\r\n");
    }

    #[test]
    fn whitespace_runs_crossing_the_hard_boundary_are_never_sent_to_the_model() {
        let input = format!("{}\r\nnext", "a".repeat(47));
        let chunks = chunk_document(&input, tiny_policy()).unwrap();
        assert_eq!(reconstruct_document(&chunks).unwrap(), input);
        assert!(chunks.iter().all(|chunk| {
            chunk.text.is_empty()
                || (!chunk.text.chars().next().is_some_and(char::is_whitespace)
                    && !chunk
                        .text
                        .chars()
                        .next_back()
                        .is_some_and(char::is_whitespace))
        }));
        assert!(chunks.iter().any(|chunk| chunk.separator_after == "\r\n"));
    }

    #[test]
    fn leading_whitespace_is_always_a_separator_only_unit() {
        let input = " \t\r\nwords remain";
        let chunks = chunk_document(input, tiny_policy()).unwrap();
        assert!(chunks[0].text.is_empty());
        assert_eq!(chunks[0].separator_after, " \t\r\n");
        assert_eq!(reconstruct_document(&chunks).unwrap(), input);
    }

    #[test]
    fn boundaries_never_split_utf8_or_protected_tokens() {
        let input = format!(
            "Português ação café {} final words after the protected value.",
            "https://example.com/a/very/long/path?q=12345"
        );
        let chunks = chunk_document(&input, tiny_policy()).unwrap();
        let token = "https://example.com/a/very/long/path?q=12345";
        assert_eq!(reconstruct_document(&chunks).unwrap(), input);
        assert!(chunks.iter().any(|chunk| chunk.text.contains(token)));
        assert_eq!(
            chunks
                .iter()
                .filter(|chunk| chunk.text.contains("https://"))
                .count(),
            1
        );
    }

    #[test]
    fn randomized_unicode_sources_reconstruct_exactly() {
        let atoms = ["hello ", "ação ", "世界 ", "sentence. ", "\n\n", "v2 "];
        let mut state = 0x7a91_d3e5_u64;
        for _case in 0..500 {
            let count = (next(&mut state) % 180 + 1) as usize;
            let mut input = String::new();
            for _ in 0..count {
                input.push_str(atoms[(next(&mut state) as usize) % atoms.len()]);
            }
            let chunks = chunk_document(&input, tiny_policy()).unwrap();
            assert_eq!(reconstruct_document(&chunks).unwrap(), input);
            assert!(chunks.iter().all(|chunk| chunk.text.len() <= 48));
        }
    }

    #[test]
    fn rejects_invalid_policy_and_oversized_document() {
        assert_eq!(
            chunk_document(
                "text",
                DocumentChunkPolicy {
                    target_bytes: 10,
                    maximum_chunk_bytes: 5,
                    maximum_document_bytes: 20,
                },
            ),
            Err(DocumentChunkError::InvalidPolicy)
        );
        assert!(matches!(
            chunk_document(&"x".repeat(4_097), tiny_policy()),
            Err(DocumentChunkError::DocumentTooLarge { .. })
        ));
        assert_eq!(
            chunk_document(
                "text",
                DocumentChunkPolicy {
                    target_bytes: 1,
                    maximum_chunk_bytes: 1,
                    maximum_document_bytes: 4,
                },
            ),
            Err(DocumentChunkError::InvalidPolicy)
        );
    }

    #[test]
    fn adversarial_chunk_counts_are_bounded() {
        assert_eq!(
            chunk_document(&"x".repeat(100_000), tiny_policy()),
            Err(DocumentChunkError::DocumentTooLarge {
                actual: 100_000,
                maximum: 4_096,
            })
        );
        let policy = DocumentChunkPolicy {
            maximum_document_bytes: 256 * 1_024,
            ..tiny_policy()
        };
        assert_eq!(
            chunk_document(&"x".repeat(100_000), policy),
            Err(DocumentChunkError::TooManyChunks)
        );
    }

    #[test]
    fn assembly_rejects_reordered_or_gapped_chunks() {
        let mut chunks = chunk_document(&"word ".repeat(40), tiny_policy()).unwrap();
        chunks.swap(0, 1);
        assert_eq!(
            reconstruct_document(&chunks),
            Err(DocumentChunkError::InvalidAssembly)
        );
    }

    #[test]
    fn outcome_counts_are_content_free_and_complete() {
        let reports = vec![
            DocumentChunkReport {
                sequence: 0,
                source_bytes: 10,
                output_bytes: 11,
                outcome: DocumentChunkOutcome::Formatted,
            },
            DocumentChunkReport {
                sequence: 1,
                source_bytes: 12,
                output_bytes: 12,
                outcome: DocumentChunkOutcome::FallbackOutputRejected,
            },
        ];
        let counts = DocumentOutcomeCounts::from_reports(&reports);
        assert_eq!(counts.formatted, 1);
        assert_eq!(counts.output_rejected, 1);
        assert_eq!(counts.total(), reports.len());
    }

    fn next(state: &mut u64) -> u64 {
        *state ^= *state << 13;
        *state ^= *state >> 7;
        *state ^= *state << 17;
        *state
    }
}
