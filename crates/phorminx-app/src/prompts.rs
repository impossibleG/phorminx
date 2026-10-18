//! Prompt references inside instruction fields.
//!
//! Instruction fields are plain text boxes. A reference to a library prompt is
//! written as `@[Prompt name]`, so the stored text stays readable and editable
//! anywhere. When the instructions are used, every reference is replaced by
//! that prompt's latest text. A reference to a prompt that does not exist
//! resolves to nothing and is reported as missing, so callers can say so
//! instead of silently losing text.
//!
//! The first test builds wrote references as `@{prompt:ID}`; those still
//! resolve.

use phorminx_persistence::{Persistence, Prompt};

const OPEN: &str = "@[";
const CLOSE: char = ']';
const LEGACY_OPEN: &str = "@{prompt:";
const LEGACY_CLOSE: char = '}';
/// The longest prompt name, matching the library's limit.
pub const MAX_NAME_CHARS: usize = 80;

/// What a reference points at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reference<'text> {
    Name(&'text str),
    /// `@{prompt:ID}` from the first test builds.
    Id(i64),
}

impl Reference<'_> {
    /// Whether this reference points at `prompt`.
    pub fn matches(&self, prompt: &Prompt) -> bool {
        match self {
            Self::Name(name) => same_name(name, &prompt.name),
            Self::Id(id) => *id == prompt.id,
        }
    }

    fn label(&self) -> String {
        match self {
            Self::Name(name) => (*name).to_owned(),
            Self::Id(id) => format!("#{id}"),
        }
    }
}

/// One piece of an instruction field.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Segment<'text> {
    Text(&'text str),
    Prompt(Reference<'text>),
}

/// Prompt names compare without case and surrounding spaces.
pub fn same_name(left: &str, right: &str) -> bool {
    left.trim().to_lowercase() == right.trim().to_lowercase()
}

/// Splits instructions into text and prompt references, in order. Anything
/// that is not a well-formed reference stays text exactly as typed.
pub fn segments(text: &str) -> Vec<Segment<'_>> {
    let mut parts = Vec::new();
    let mut text_start = 0;
    let mut cursor = 0;
    while let Some(offset) = text[cursor..].find('@') {
        let start = cursor + offset;
        match reference_at(&text[start..]) {
            Some((reference, length)) => {
                if start > text_start {
                    parts.push(Segment::Text(&text[text_start..start]));
                }
                parts.push(Segment::Prompt(reference));
                cursor = start + length;
                text_start = cursor;
            }
            None => cursor = start + 1,
        }
    }
    if text_start < text.len() {
        parts.push(Segment::Text(&text[text_start..]));
    }
    parts
}

/// The reference at the start of `text` and its length in bytes.
fn reference_at(text: &str) -> Option<(Reference<'_>, usize)> {
    if let Some(rest) = text.strip_prefix(OPEN) {
        let end = rest.find(CLOSE)?;
        let name = &rest[..end];
        let valid = !name.trim().is_empty()
            && name.chars().count() <= MAX_NAME_CHARS
            && !name.contains(['[', '\n', '\r']);
        return valid.then_some((Reference::Name(name.trim()), OPEN.len() + end + 1));
    }
    let rest = text.strip_prefix(LEGACY_OPEN)?;
    let end = rest.find(LEGACY_CLOSE)?;
    let id = rest[..end].parse::<i64>().ok().filter(|id| *id > 0)?;
    Some((Reference::Id(id), LEGACY_OPEN.len() + end + 1))
}

/// Whether `text` refers to `prompt`.
pub fn refers_to(text: &str, prompt: &Prompt) -> bool {
    segments(text).iter().any(
        |segment| matches!(segment, Segment::Prompt(reference) if reference.matches(prompt)),
    )
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resolved {
    pub text: String,
    /// Names (or `#ID` for old references) of prompts that do not exist.
    pub missing: Vec<String>,
}

/// Replaces every reference with the text `lookup` returns for it.
pub fn resolve(text: &str, mut lookup: impl FnMut(&Reference<'_>) -> Option<String>) -> Resolved {
    let mut resolved = String::with_capacity(text.len());
    let mut missing: Vec<String> = Vec::new();
    for segment in segments(text) {
        match segment {
            Segment::Text(text) => resolved.push_str(text),
            Segment::Prompt(reference) => match lookup(&reference) {
                Some(body) => resolved.push_str(&body),
                None => {
                    let label = reference.label();
                    if !missing.iter().any(|known| same_name(known, &label)) {
                        missing.push(label);
                    }
                }
            },
        }
    }
    Resolved {
        text: resolved.trim().to_owned(),
        missing,
    }
}

/// Resolves against the prompt library, using each prompt's latest version.
pub fn resolve_with(persistence: &Persistence, text: &str) -> Resolved {
    if !text.contains(OPEN) && !text.contains(LEGACY_OPEN) {
        return Resolved {
            text: text.to_owned(),
            missing: Vec::new(),
        };
    }
    let library = persistence.prompts().list().unwrap_or_default();
    resolve(text, |reference| {
        library
            .iter()
            .find(|prompt| reference.matches(prompt))
            .map(|prompt| prompt.body.clone())
    })
}

/// Where a prompt is referenced in meeting and Action instructions.
pub fn assistant_usages(
    config: &phorminx_assistant::AssistantConfig,
    prompt: &Prompt,
) -> Vec<String> {
    let mut usages = Vec::new();
    if refers_to(&config.preset, prompt) {
        usages.push("Meeting assistant instructions".to_owned());
    }
    for action in &config.actions {
        if refers_to(&action.payload_prompt, prompt) {
            usages.push(format!("Action \u{201c}{}\u{201d}", action.name));
        }
    }
    usages
}

/// The reference text an instruction field stores for a prompt.
pub fn reference(name: &str) -> String {
    format!("{OPEN}{}{CLOSE}", name.trim())
}

#[cfg(test)]
mod tests {
    use super::*;
    use phorminx_persistence::PromptDraft;

    fn prompt(id: i64, name: &str) -> Prompt {
        Prompt {
            id,
            name: name.into(),
            tags: Vec::new(),
            body: String::new(),
            version: 1,
            created_at_ms: 0,
            updated_at_ms: 0,
        }
    }

    #[test]
    fn references_are_parsed_in_order_and_malformed_ones_stay_text() {
        assert_eq!(
            segments("Be brief. @[Short email] Then @{prompt:12}!"),
            [
                Segment::Text("Be brief. "),
                Segment::Prompt(Reference::Name("Short email")),
                Segment::Text(" Then "),
                Segment::Prompt(Reference::Id(12)),
                Segment::Text("!"),
            ]
        );
        let malformed =
            "mail me@example.com @[] @[ ] @[a[b] @[open @{prompt:x} @{prompt:0} @{prompt:7";
        assert_eq!(resolve(malformed, |_| None).text, malformed);
        assert!(resolve(malformed, |_| None).missing.is_empty());
        let long = format!("@[{}]", "n".repeat(MAX_NAME_CHARS + 1));
        assert_eq!(segments(&long), [Segment::Text(&long)]);
        assert_eq!(
            segments("@[ Lists ]"),
            [Segment::Prompt(Reference::Name("Lists"))]
        );
        assert_eq!(reference(" Lists "), "@[Lists]");
    }

    #[test]
    fn names_match_without_case_and_old_ids_still_match() {
        let lists = prompt(9, "Lists");
        assert!(refers_to("x @[lists]", &lists));
        assert!(refers_to("x @{prompt:9}", &lists));
        assert!(!refers_to("x @[Lists2] @{prompt:8}", &lists));
        let missing = resolve("@[Gone] @[gone] @{prompt:4}", |_| None).missing;
        assert_eq!(missing, ["Gone", "#4"]);
    }

    #[test]
    fn assistant_usages_name_the_meeting_preset_and_actions() {
        let target = prompt(5, "Notes style");
        let mut config = phorminx_assistant::AssistantConfig {
            preset: format!("Be concise. {}", reference("Notes style")),
            ..Default::default()
        };
        config.actions.push(phorminx_assistant::ActionConfig {
            name: "Notes".into(),
            payload_prompt: reference("notes style"),
            ..Default::default()
        });
        assert_eq!(
            assistant_usages(&config, &target),
            [
                "Meeting assistant instructions",
                "Action \u{201c}Notes\u{201d}"
            ]
        );
        assert!(assistant_usages(&config, &prompt(6, "Other")).is_empty());
    }

    #[test]
    fn references_resolve_to_the_latest_version_and_missing_ones_are_reported() {
        let directory = tempfile::tempdir().unwrap();
        let persistence = Persistence::open(directory.path().join("p.db")).unwrap();
        let draft = |body: &str| PromptDraft {
            name: "Lists".into(),
            tags: Vec::new(),
            body: body.into(),
        };
        let id = persistence
            .prompts()
            .create(&draft("Use bullets."), 1)
            .unwrap();
        let instructions = format!("Keep names. {}", reference("Lists"));
        assert_eq!(
            resolve_with(&persistence, &instructions).text,
            "Keep names. Use bullets."
        );
        persistence
            .prompts()
            .save(id, &draft("Use numbered steps."), 2)
            .unwrap();
        assert_eq!(
            resolve_with(&persistence, &instructions).text,
            "Keep names. Use numbered steps."
        );
        assert_eq!(
            resolve_with(&persistence, &format!("Old: @{{prompt:{id}}}")).text,
            "Old: Use numbered steps."
        );
        persistence.prompts().delete(id).unwrap();
        let resolved = resolve_with(&persistence, &instructions);
        assert_eq!(resolved.text, "Keep names.");
        assert_eq!(resolved.missing, ["Lists"]);
        assert_eq!(resolve_with(&persistence, "plain").text, "plain");
    }
}
