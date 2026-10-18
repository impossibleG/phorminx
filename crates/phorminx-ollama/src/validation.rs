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
        '"' | '\''
            | '('
            | ')'
            | '['
            | ']'
            | '{'
            | '}'
            | '<'
            | '>'
            | '*'
            | ','
            | '.'
            | '!'
            | '?'
            | ';'
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

    /// Restructuring profiles legitimately add list markers, headings and
    /// line breaks, so they get more room to grow than in-place cleanup.
    pub fn relaxed_for_restructuring(&self) -> Self {
        Self {
            policy: ValidationPolicy {
                maximum_output_bytes: self.policy.maximum_output_bytes,
                maximum_growth_factor: self.policy.maximum_growth_factor.max(4),
                maximum_extra_bytes: self.policy.maximum_extra_bytes.max(1024),
            },
        }
    }

    pub fn validate(
        &self,
        input: &str,
        output: &str,
        protected: &ProtectedTokens,
    ) -> Result<String, ValidationError> {
        let output = strip_transcript_tags(output.trim());
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
        let input_lower = input.to_ascii_lowercase();
        let echoed_scaffold = [
            "begin transcript",
            "end transcript",
            "protected tokens",
            "occurrence(s)",
        ]
        .iter()
        .any(|marker| lower.contains(marker) && !input_lower.contains(marker));
        if lower.contains("<think>")
            || lower.contains("</think>")
            || lower.starts_with("here is the formatted")
            || lower.starts_with("here's the formatted")
            || echoed_scaffold
            || (output.contains("```") && !input.contains("```"))
        {
            return Err(ValidationError::ModelCommentary);
        }
        // List markers the model added ("1.", "2)") are layout, not numbers
        // from the dictation, so they are not counted against protected tokens.
        let without_markers = strip_list_markers(&unwrap_markdown_links(output));
        let actual_tokens = ProtectedTokens::extract(&without_markers);
        let actual_numbers = numeric_counts(&without_markers);
        let mut expected_numbers = BTreeMap::<String, (usize, &str)>::new();
        for token in protected.tokens() {
            if let Some(number) = canonical_number(&token.value) {
                let entry = expected_numbers.entry(number).or_insert((0, &token.value));
                entry.0 += token.occurrences;
                continue;
            }
            let actual = actual_tokens
                .tokens()
                .binary_search_by(|candidate| candidate.value.cmp(&token.value))
                .ok()
                .map_or(0, |index| actual_tokens.tokens()[index].occurrences);
            if actual != token.occurrences {
                return Err(ValidationError::ProtectedTokenChanged {
                    token: token.value.clone(),
                    expected: token.occurrences,
                    actual,
                });
            }
        }
        // Plain numbers may be re-spelled ("12500" as "$12,500", "14" as
        // "14th") as long as the same value appears the same number of times.
        for (number, (expected, original)) in expected_numbers {
            let actual = actual_numbers.get(&number).copied().unwrap_or(0);
            if actual != expected {
                return Err(ValidationError::ProtectedTokenChanged {
                    token: original.to_owned(),
                    expected,
                    actual,
                });
            }
        }
        Ok(output.to_owned())
    }
}

/// Some models wrap their answer in the tags that delimited the transcript.
fn strip_transcript_tags(output: &str) -> &str {
    output
        .strip_prefix("<transcript>")
        .and_then(|rest| rest.strip_suffix("</transcript>"))
        .map_or(output, str::trim)
}

/// `[label](target)` becomes `label target`, or just `target` when the model
/// repeated the URL as its own label, so each value is counted once.
fn unwrap_markdown_links(output: &str) -> String {
    let mut result = String::with_capacity(output.len());
    let mut rest = output;
    while let Some(open) = rest.find('[') {
        let after = &rest[open + 1..];
        let link = after.find("](").and_then(|close| {
            let label = &after[..close];
            let target_part = &after[close + 2..];
            let end = target_part.find(')')?;
            (!label.contains('\n') && !label.contains('['))
                .then(|| (label, &target_part[..end], &target_part[end + 1..]))
        });
        match link {
            Some((label, target, remainder)) => {
                result.push_str(&rest[..open]);
                if label != target {
                    result.push_str(label);
                    result.push(' ');
                }
                result.push_str(target);
                rest = remainder;
            }
            None => {
                result.push_str(&rest[..=open]);
                rest = after;
            }
        }
    }
    result.push_str(rest);
    result
}

fn strip_list_markers(output: &str) -> String {
    output
        .lines()
        .map(|line| {
            let trimmed = line.trim_start();
            let digits = trimmed
                .find(|character: char| !character.is_ascii_digit())
                .unwrap_or(trimmed.len());
            let rest = &trimmed[digits..];
            if (1..=2).contains(&digits) && (rest.starts_with(". ") || rest.starts_with(") ")) {
                &rest[2..]
            } else {
                line
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Canonical value of a plain number, or `None` for anything else (versions,
/// times, identifiers, URLs), which must survive exactly as written.
fn canonical_number(word: &str) -> Option<String> {
    let word = word.trim_matches(|character: char| {
        matches!(
            character,
            '*' | '_' | '"' | '\'' | '(' | ')' | ',' | '.' | '!' | '?' | ';' | ':'
        )
    });
    let word = word.trim_start_matches(['$', '€', '£']);
    let word = word.strip_prefix("R$").unwrap_or(word);
    let lower = word.to_ascii_lowercase();
    let body = ["st", "nd", "rd", "th", "%", "º", "ª"]
        .iter()
        .find_map(|suffix| lower.strip_suffix(suffix))
        .unwrap_or(&lower);
    if body.is_empty() || !body.starts_with(|character: char| character.is_ascii_digit()) {
        return None;
    }
    if !body
        .chars()
        .all(|character| character.is_ascii_digit() || matches!(character, ',' | '.'))
        || body.ends_with([',', '.'])
    {
        return None;
    }
    // A separator followed by exactly three digits groups thousands; any
    // other separator is a decimal point.
    let groups = body.split([',', '.']).collect::<Vec<_>>();
    if groups.len() == 1 {
        let trimmed = body.trim_start_matches('0');
        return Some(if trimmed.is_empty() {
            "0".to_owned()
        } else {
            trimmed.to_owned()
        });
    }
    let thousands = groups[1..].iter().all(|group| group.len() == 3) && groups[0].len() <= 3;
    if thousands {
        let joined = groups.concat();
        let trimmed = joined.trim_start_matches('0');
        return Some(if trimmed.is_empty() {
            "0".to_owned()
        } else {
            trimmed.to_owned()
        });
    }
    if groups.len() == 2 {
        return Some(format!("{}.{}", groups[0], groups[1]));
    }
    None
}

fn numeric_counts(output: &str) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for word in output.split_whitespace() {
        if let Some(number) = canonical_number(word) {
            *counts.entry(number).or_default() += 1;
        }
    }
    counts
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
    fn overlapping_protected_values_use_the_same_occurrence_semantics() {
        for input in [
            "Use v2 and v2beta exactly.",
            "Compare `v2` with v2.",
            "Keep {{API_KEY}} beside API_KEY.",
        ] {
            let tokens = ProtectedTokens::extract(input);
            assert!(
                OutputValidator::default()
                    .validate(input, input, &tokens)
                    .is_ok(),
                "unchanged source rejected: {input}"
            );
        }
    }

    #[test]
    fn plain_numbers_may_be_respelled_but_not_changed() {
        let validator = OutputValidator::default();
        let input = "the budget is 12500 dollars for phase 1 due march 14";
        let tokens = ProtectedTokens::extract(input);
        assert!(
            validator
                .validate(
                    input,
                    "The budget is $12,500 for phase 1, due March 14th.",
                    &tokens
                )
                .is_ok()
        );
        assert!(matches!(
            validator.validate(
                input,
                "The budget is $12,000 for phase 1, due March 14.",
                &tokens
            ),
            Err(ValidationError::ProtectedTokenChanged { .. })
        ));
        assert!(matches!(
            validator.validate(
                input,
                "The budget is $12,500 for phase 1, due March fourteenth.",
                &tokens
            ),
            Err(ValidationError::ProtectedTokenChanged { .. })
        ));
        let decimal = "the rate is 2.5 percent";
        assert!(
            validator
                .validate(
                    decimal,
                    "The rate is 2.5%.",
                    &ProtectedTokens::extract(decimal)
                )
                .is_ok()
        );
        assert!(matches!(
            validator.validate(
                decimal,
                "The rate is 25%.",
                &ProtectedTokens::extract(decimal)
            ),
            Err(ValidationError::ProtectedTokenChanged { .. })
        ));
        // Versions and times are not plain numbers and stay exact.
        let version = "ship v2 at 10:30";
        assert!(matches!(
            validator.validate(
                version,
                "Ship version 2 at 10:30.",
                &ProtectedTokens::extract(version)
            ),
            Err(ValidationError::ProtectedTokenChanged { .. })
        ));
    }

    #[test]
    fn markdown_links_bold_and_autolinks_keep_protected_values() {
        let validator = OutputValidator::default();
        let input = "the spec is at https://example.com/spec write to ops@example.com";
        let tokens = ProtectedTokens::extract(input);
        for output in [
            "- Spec: [https://example.com/spec](https://example.com/spec)\n- Contact: **ops@example.com**",
            "The spec is at <https://example.com/spec>. Write to ops@example.com.",
            "The [spec](https://example.com/spec) is ready; write to ops@example.com.",
        ] {
            assert!(
                validator.validate(input, output, &tokens).is_ok(),
                "rejected: {output}"
            );
        }
        assert!(matches!(
            validator.validate(
                input,
                "The [spec](https://example.com/other) is ready; write to ops@example.com.",
                &tokens
            ),
            Err(ValidationError::ProtectedTokenChanged { .. })
        ));
    }

    #[test]
    fn added_list_markers_do_not_count_as_numbers() {
        let validator = OutputValidator::default();
        let input = "we need 2 things the toggle and the picker";
        let tokens = ProtectedTokens::extract(input);
        assert!(
            validator
                .validate(
                    input,
                    "We need 2 things:\n1. The toggle\n2. The picker",
                    &tokens
                )
                .is_ok()
        );
    }

    #[test]
    fn echoed_prompt_scaffolding_is_commentary_and_tags_are_unwrapped() {
        let validator = OutputValidator::default();
        let input = "meet at 10 tomorrow";
        let tokens = ProtectedTokens::extract(input);
        assert_eq!(
            validator.validate(
                input,
                "- 10 (1 occurrence(s))\n--- BEGIN TRANSCRIPT ---\nMeet at 10 tomorrow.\n--- END TRANSCRIPT ---",
                &tokens
            ),
            Err(ValidationError::ModelCommentary)
        );
        assert_eq!(
            validator
                .validate(
                    input,
                    "<transcript>\nMeet at 10 tomorrow.\n</transcript>",
                    &tokens
                )
                .unwrap(),
            "Meet at 10 tomorrow."
        );
    }

    #[test]
    fn restructuring_profiles_allow_more_growth() {
        let input = "a b c";
        let output = format!("- a\n- b\n- c\n{}", "x".repeat(600));
        let empty = ProtectedTokens::default();
        assert!(
            OutputValidator::default()
                .validate(input, &output, &empty)
                .is_err()
        );
        assert!(
            OutputValidator::default()
                .relaxed_for_restructuring()
                .validate(input, &output, &empty)
                .is_ok()
        );
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
