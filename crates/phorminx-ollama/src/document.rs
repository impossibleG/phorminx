use std::ops::Range;

use crate::ProtectedTokens;

const DEFAULT_TARGET_BYTES: usize = 8 * 1024;
const DEFAULT_MAXIMUM_CHUNK_BYTES: usize = 12 * 1024;
const DEFAULT_MAXIMUM_DOCUMENT_BYTES: usize = 256 * 1024;

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
        if self.target_bytes == 0
            || self.maximum_chunk_bytes == 0
            || self.maximum_document_bytes == 0
            || self.target_bytes > self.maximum_chunk_bytes
            || self.maximum_chunk_bytes > self.maximum_document_bytes
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
    let mut start = 0;
    while start < input.len() {
        let remaining = input.len() - start;
        let split = if remaining <= policy.maximum_chunk_bytes {
            input.len()
        } else {
            choose_boundary(input, start, policy, &protected)?
        };
        let (text_end, end) = if split == input.len() {
            (split, split)
        } else {
            isolate_separator(input, start, split, policy.maximum_chunk_bytes)
        };
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

fn isolate_separator(
    input: &str,
    start: usize,
    split: usize,
    maximum_chunk_bytes: usize,
) -> (usize, usize) {
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
    if text_end == start {
        return (split, split);
    }

    let hard_end = start.saturating_add(maximum_chunk_bytes).min(input.len());
    let mut end = split;
    while end < hard_end {
        let character = input[end..].chars().next().expect("nonempty suffix");
        if !character.is_whitespace() || end + character.len_utf8() > hard_end {
            break;
        }
        end += character.len_utf8();
    }
    (text_end, end)
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
    let mut ranges = ProtectedTokens::extract(input)
        .tokens()
        .iter()
        .flat_map(|token| {
            input
                .match_indices(&token.value)
                .map(|(start, value)| start..start + value.len())
        })
        .collect::<Vec<_>>();
    ranges.sort_by_key(|range| (range.start, range.end));
    ranges
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
    #[error("cannot split source at byte {start} within the {maximum}-byte chunk bound")]
    UnsplittableSpan { start: usize, maximum: usize },
    #[error("document chunks are missing, reordered, overlapping, or malformed")]
    InvalidAssembly,
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

    fn next(state: &mut u64) -> u64 {
        *state ^= *state << 13;
        *state ^= *state >> 7;
        *state ^= *state << 17;
        *state
    }
}
