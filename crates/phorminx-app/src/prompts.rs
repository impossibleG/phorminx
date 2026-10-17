//! Prompt references inside instruction fields.
//!
//! An instruction field stores free text with references such as
//! `@{prompt:12}`. The editor shows each reference as a chip. When the
//! instructions are used, every reference is replaced by that prompt's latest
//! text. A reference to a deleted prompt resolves to nothing and is reported
//! as missing, so callers can say so instead of silently losing text.

use phorminx_persistence::Persistence;

const OPEN: &str = "@{prompt:";
const CLOSE: char = '}';

/// One piece of an instruction field.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Segment<'text> {
    Text(&'text str),
    Prompt(i64),
}

/// Splits instructions into text and prompt references, in order.
pub fn segments(text: &str) -> Vec<Segment<'_>> {
    let mut parts = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find(OPEN) {
        let after = &rest[start + OPEN.len()..];
        let reference = after
            .find(CLOSE)
            .and_then(|end| Some((after[..end].parse::<i64>().ok()?, end)));
        match reference {
            Some((id, end)) if id > 0 => {
                if start > 0 {
                    parts.push(Segment::Text(&rest[..start]));
                }
                parts.push(Segment::Prompt(id));
                rest = &after[end + CLOSE.len_utf8()..];
            }
            _ => {
                // Not a well-formed reference: keep it as typed.
                let keep = start + OPEN.len();
                parts.push(Segment::Text(&rest[..keep]));
                rest = &rest[keep..];
            }
        }
    }
    if !rest.is_empty() {
        parts.push(Segment::Text(rest));
    }
    parts
}

/// The distinct prompt ids an instruction field refers to.
pub fn referenced_ids(text: &str) -> Vec<i64> {
    let mut ids = Vec::new();
    for segment in segments(text) {
        if let Segment::Prompt(id) = segment
            && !ids.contains(&id)
        {
            ids.push(id);
        }
    }
    ids
}

/// Whether `text` refers to the prompt `id`.
pub fn refers_to(text: &str, id: i64) -> bool {
    referenced_ids(text).contains(&id)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resolved {
    pub text: String,
    /// Ids of referenced prompts that no longer exist.
    pub missing: Vec<i64>,
}

/// Replaces every reference with the text `lookup` returns for it.
pub fn resolve(text: &str, mut lookup: impl FnMut(i64) -> Option<String>) -> Resolved {
    let mut resolved = String::with_capacity(text.len());
    let mut missing = Vec::new();
    for segment in segments(text) {
        match segment {
            Segment::Text(text) => resolved.push_str(text),
            Segment::Prompt(id) => match lookup(id) {
                Some(body) => resolved.push_str(&body),
                None => {
                    if !missing.contains(&id) {
                        missing.push(id);
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
    if !text.contains(OPEN) {
        return Resolved {
            text: text.to_owned(),
            missing: Vec::new(),
        };
    }
    resolve(text, |id| {
        persistence
            .prompts()
            .get(id)
            .ok()
            .flatten()
            .map(|prompt| prompt.body)
    })
}

/// Where a prompt is referenced in meeting and Action instructions.
pub fn assistant_usages(config: &phorminx_assistant::AssistantConfig, id: i64) -> Vec<String> {
    let mut usages = Vec::new();
    if refers_to(&config.preset, id) {
        usages.push("Meeting assistant instructions".to_owned());
    }
    for action in &config.actions {
        if refers_to(&action.payload_prompt, id) {
            usages.push(format!("Action \u{201c}{}\u{201d}", action.name));
        }
    }
    usages
}

/// The reference text an instruction field stores for prompt `id`.
pub fn reference(id: i64) -> String {
    format!("{OPEN}{id}{CLOSE}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use phorminx_persistence::PromptDraft;

    #[test]
    fn references_are_parsed_in_order_and_malformed_ones_stay_text() {
        assert_eq!(
            segments("Be brief. @{prompt:3} Then @{prompt:12}!"),
            [
                Segment::Text("Be brief. "),
                Segment::Prompt(3),
                Segment::Text(" Then "),
                Segment::Prompt(12),
                Segment::Text("!"),
            ]
        );
        let malformed = "mail me @{prompt:x} or @{prompt:0} or @{prompt:7";
        assert_eq!(resolve(malformed, |_| None).text, malformed);
        assert_eq!(referenced_ids(malformed), Vec::<i64>::new());
        assert_eq!(referenced_ids("@{prompt:4}@{prompt:4}@{prompt:5}"), [4, 5]);
        assert!(refers_to("x @{prompt:9}", 9));
        assert_eq!(reference(9), "@{prompt:9}");
    }

    #[test]
    fn assistant_usages_name_the_meeting_preset_and_actions() {
        let mut config = phorminx_assistant::AssistantConfig {
            preset: format!("Be concise. {}", reference(5)),
            ..Default::default()
        };
        config.actions.push(phorminx_assistant::ActionConfig {
            name: "Notes".into(),
            payload_prompt: reference(5),
            ..Default::default()
        });
        assert_eq!(
            assistant_usages(&config, 5),
            [
                "Meeting assistant instructions",
                "Action \u{201c}Notes\u{201d}"
            ]
        );
        assert!(assistant_usages(&config, 6).is_empty());
    }

    #[test]
    fn references_resolve_to_the_latest_version_and_missing_ones_are_reported() {
        let directory = tempfile::tempdir().unwrap();
        let persistence = Persistence::open(directory.path().join("p.db")).unwrap();
        let id = persistence
            .prompts()
            .create(
                &PromptDraft {
                    name: "Lists".into(),
                    tags: Vec::new(),
                    body: "Use bullets.".into(),
                },
                1,
            )
            .unwrap();
        let instructions = format!("Keep names. {}", reference(id));
        assert_eq!(
            resolve_with(&persistence, &instructions).text,
            "Keep names. Use bullets."
        );
        persistence
            .prompts()
            .save(
                id,
                &PromptDraft {
                    name: "Lists".into(),
                    tags: Vec::new(),
                    body: "Use numbered steps.".into(),
                },
                2,
            )
            .unwrap();
        assert_eq!(
            resolve_with(&persistence, &instructions).text,
            "Keep names. Use numbered steps."
        );
        persistence.prompts().delete(id).unwrap();
        let resolved = resolve_with(&persistence, &instructions);
        assert_eq!(resolved.text, "Keep names.");
        assert_eq!(resolved.missing, [id]);
        assert_eq!(resolve_with(&persistence, "plain").text, "plain");
    }
}
