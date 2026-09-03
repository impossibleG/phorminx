use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;

/// A value whose spelling and multiplicity must survive model formatting.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProtectedToken {
    pub value: String,
    pub occurrences: usize,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ProtectedTokens {
    tokens: Vec<ProtectedToken>,
}

impl ProtectedTokens {
    pub fn extract(input: &str) -> Self {
        Self::extract_with_ranges(input).0
    }

    pub(crate) fn extract_with_ranges(input: &str) -> (Self, Vec<Range<usize>>) {
        let mut occurrences = BTreeSet::<(usize, usize)>::new();
        collect_delimited(input, '`', '`', &mut occurrences);
        collect_pair(input, "{{", "}}", &mut occurrences);
        collect_pair(input, "${", "}", &mut occurrences);
        collect_protected_words(input, &mut occurrences);

        let mut counts = BTreeMap::<String, usize>::new();
        let mut ranges = Vec::new();
        for (start, end) in occurrences {
            let candidate = &input[start..end];
            *counts.entry(candidate.to_owned()).or_default() += 1;
            ranges.push(start..end);
        }
        (
            Self {
                tokens: counts
                    .into_iter()
                    .map(|(value, occurrences)| ProtectedToken { value, occurrences })
                    .collect(),
            },
            ranges,
        )
    }

    pub fn tokens(&self) -> &[ProtectedToken] {
        &self.tokens
    }

    pub fn is_empty(&self) -> bool {
        self.tokens.is_empty()
    }
}

fn collect_delimited(
    input: &str,
    opening: char,
    closing: char,
    output: &mut BTreeSet<(usize, usize)>,
) {
    let opening = opening.to_string();
    let closing = closing.to_string();
    collect_pair(input, &opening, &closing, output);
}

fn collect_pair(input: &str, opening: &str, closing: &str, output: &mut BTreeSet<(usize, usize)>) {
    let mut cursor = 0;
    while let Some(relative_start) = input[cursor..].find(opening) {
        let start = cursor + relative_start;
        let after_open = &input[start + opening.len()..];
        let Some(end) = after_open.find(closing) else {
            break;
        };
        let full_end = start + opening.len() + end + closing.len();
        if full_end - start > opening.len() + closing.len() && full_end - start <= 512 {
            output.insert((start, full_end));
        }
        cursor = full_end;
    }
}

fn collect_protected_words(input: &str, output: &mut BTreeSet<(usize, usize)>) {
    let mut start = None;
    for (index, character) in input
        .char_indices()
        .chain(std::iter::once((input.len(), ' ')))
    {
        if character.is_whitespace() {
            if let Some(word_start) = start.take() {
                let range = trim_sentence_range(input, word_start, index);
                if range.start < range.end && is_protected_word(&input[range.clone()]) {
                    output.insert((range.start, range.end));
                }
            }
        } else if start.is_none() {
            start = Some(index);
        }
    }
}

fn trim_sentence_range(input: &str, mut start: usize, mut end: usize) -> Range<usize> {
    while start < end {
        let character = input[start..end].chars().next().expect("nonempty word");
        if !is_sentence_punctuation(character) {
            break;
        }
        start += character.len_utf8();
    }
    while start < end {
        let character = input[start..end]
            .chars()
            .next_back()
            .expect("nonempty word");
        if !is_sentence_punctuation(character) {
            break;
        }
        end -= character.len_utf8();
    }
    start..end
}

fn is_sentence_punctuation(character: char) -> bool {
    matches!(
        character,
        '"' | '\'' | '(' | ')' | '[' | ']' | '{' | '}' | ',' | '.' | '!' | '?' | ';'
    )
}

fn is_protected_word(value: &str) -> bool {
    if value.len() < 2 || value.len() > 512 {
        return false;
    }
    let lower = value.to_ascii_lowercase();
    lower.starts_with("http://")
        || lower.starts_with("https://")
        || lower.starts_with("www.")
        || looks_like_email(value)
        || value.starts_with("--")
        || value.starts_with("\\\\")
        || value.contains(":\\")
        || (value.starts_with('/') && value[1..].contains('/'))
        || value.chars().any(|character| character.is_ascii_digit())
        || (value.contains('_') && value.chars().any(char::is_alphabetic))
}

fn looks_like_email(value: &str) -> bool {
    let Some((local, domain)) = value.split_once('@') else {
        return false;
    };
    !local.is_empty() && domain.contains('.') && !domain.ends_with('.')
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValidationPolicy {
    pub maximum_output_bytes: usize,
    pub maximum_growth_factor: usize,
    pub maximum_extra_bytes: usize,
}

impl Default for ValidationPolicy {
    fn default() -> Self {
        Self {
            maximum_output_bytes: 64 * 1024,
            maximum_growth_factor: 3,
            maximum_extra_bytes: 512,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct OutputValidator {
    policy: ValidationPolicy,
}

impl OutputValidator {
    pub fn new(policy: ValidationPolicy) -> Self {
        Self { policy }
    }

    pub fn validate(
        &self,
        input: &str,
        output: &str,
        protected: &ProtectedTokens,
    ) -> Result<String, ValidationError> {
        let output = output.trim();
        if !input.trim().is_empty() && output.is_empty() {
            return Err(ValidationError::EmptyOutput);
        }
        if input.trim().is_empty() && !output.is_empty() {
            return Err(ValidationError::UnexpectedOutputForEmptyInput);
        }
        let relative_limit = input
            .len()
            .saturating_mul(self.policy.maximum_growth_factor)
            .saturating_add(self.policy.maximum_extra_bytes);
        let maximum = self.policy.maximum_output_bytes.min(relative_limit);
        if output.len() > maximum {
            return Err(ValidationError::OutputTooLarge {
                actual: output.len(),
                maximum,
            });
        }
        if output
            .chars()
            .any(|character| character.is_control() && !matches!(character, '\n' | '\r' | '\t'))
        {
            return Err(ValidationError::ControlCharacter);
        }
        let lower = output.to_ascii_lowercase();
        if lower.contains("<think>")
            || lower.contains("</think>")
            || lower.starts_with("here is the formatted")
            || lower.starts_with("here's the formatted")
            || (output.contains("```") && !input.contains("```"))
        {
            return Err(ValidationError::ModelCommentary);
        }
        for token in protected.tokens() {
            let actual = output.match_indices(&token.value).count();
            if actual != token.occurrences {
                return Err(ValidationError::ProtectedTokenChanged {
                    token: token.value.clone(),
                    expected: token.occurrences,
                    actual,
                });
            }
        }
        Ok(output.to_owned())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ValidationError {
    #[error("the model returned an empty transcript")]
    EmptyOutput,
    #[error("the model returned content for an empty transcript")]
    UnexpectedOutputForEmptyInput,
    #[error("formatted output is {actual} bytes; maximum is {maximum}")]
    OutputTooLarge { actual: usize, maximum: usize },
    #[error("formatted output contains an unsupported control character")]
    ControlCharacter,
    #[error("the model returned commentary or hidden reasoning instead of only the transcript")]
    ModelCommentary,
    #[error(
        "protected token `{token}` changed count: expected {expected} occurrence(s), found {actual}"
    )]
    ProtectedTokenChanged {
        token: String,
        expected: usize,
        actual: usize,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_urls_emails_paths_numbers_flags_placeholders_and_code() {
        let input = "Use `cargo test`, --release, v2, dev@example.com, https://x.dev/a, C:\\work\\x, {{name}}, ${HOME}, and API_KEY.";
        let protected = ProtectedTokens::extract(input);
        let values = protected
            .tokens()
            .iter()
            .map(|token| token.value.as_str())
            .collect::<Vec<_>>();
        for expected in [
            "`cargo test`",
            "--release",
            "v2",
            "dev@example.com",
            "https://x.dev/a",
            "C:\\work\\x",
            "{{name}}",
            "${HOME}",
            "API_KEY",
        ] {
            assert!(
                values.contains(&expected),
                "missing {expected:?} from {values:?}"
            );
        }
    }

    #[test]
    fn validates_exact_token_multiplicity() {
        let input = "Version 2.0 talks to https://example.com twice: https://example.com";
        let tokens = ProtectedTokens::extract(input);
        let validator = OutputValidator::default();
        assert!(validator.validate(input, input, &tokens).is_ok());
        assert!(matches!(
            validator.validate(
                "Use v2",
                "Use version two",
                &ProtectedTokens::extract("Use v2")
            ),
            Err(ValidationError::ProtectedTokenChanged { .. })
        ));
        assert!(matches!(
            validator.validate(input, "Version 2.0 talks to https://example.com", &tokens),
            Err(ValidationError::ProtectedTokenChanged { .. })
        ));
    }

    #[test]
    fn rejects_empty_oversized_control_and_meta_outputs() {
        let validator = OutputValidator::default();
        let empty = ProtectedTokens::default();
        assert_eq!(
            validator.validate("hello", "", &empty),
            Err(ValidationError::EmptyOutput)
        );
        assert!(matches!(
            validator.validate("x", &"y".repeat(516), &empty),
            Err(ValidationError::OutputTooLarge { .. })
        ));
        assert_eq!(
            validator.validate("hello", "bad\0text", &empty),
            Err(ValidationError::ControlCharacter)
        );
        assert_eq!(
            validator.validate("hello", "<think>secret</think>Hello", &empty),
            Err(ValidationError::ModelCommentary)
        );
    }
}
