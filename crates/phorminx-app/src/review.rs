//! Shared wording for what happened to a dictation after recognition, used by
//! the status pill, History and the review panel, and the review panel's own
//! state.

use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::Duration;

use phorminx_ollama::{
    CancellationToken, ClientTimeouts, FallbackReason, FormatProfile, KeepAlive, ModelName,
    OllamaClient, OllamaEndpoint, SelectionPolicy, ValidationError,
};

use crate::settings::{FormattingStrength, OllamaLifecycle, OllamaModelIdentity, Settings};

const MAX_REVIEW_TEXT_BYTES: usize = 64 * 1024;
const MAX_ONE_OFF_INSTRUCTION_BYTES: usize = 1024;

/// What the runtime hands the review panel after a successful dictation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReviewSeed {
    /// The text exactly as it was inserted or placed on the clipboard.
    pub text: String,
    /// Whether the text was pasted into a control that Replace can update.
    pub can_replace: bool,
    pub strength: FormattingStrength,
    pub custom_instructions: Option<String>,
    pub formatting_note: Option<String>,
}

/// Snapshot the review window renders.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReviewView {
    /// Changes whenever the panel's text is replaced by a new dictation or a
    /// finished reformat, so the window knows when to overwrite its editor.
    pub revision: u64,
    pub text: String,
    pub strength: FormattingStrength,
    pub can_replace: bool,
    pub busy: bool,
    pub formatting_note: Option<String>,
    pub notice: String,
}

/// Result of one reformat: the new text, or a short reason it was kept.
pub type ReformatResult = Result<Reformatted, String>;

/// A successful reformat and, for multi-pass runs, how many passes applied.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Reformatted {
    pub text: String,
    pub note: Option<String>,
}

#[derive(Default)]
pub struct ReviewPanel {
    seed: Option<ReviewSeed>,
    revision: u64,
    text: String,
    strength: FormattingStrength,
    notice: String,
    job: Option<Receiver<ReformatResult>>,
}

impl ReviewPanel {
    pub fn open(&mut self, seed: ReviewSeed) {
        self.revision = self.revision.wrapping_add(1);
        self.text = seed.text.clone();
        self.strength = match seed.strength {
            FormattingStrength::Raw => FormattingStrength::Light,
            strength => strength,
        };
        self.notice = String::new();
        self.job = None;
        self.seed = Some(seed);
    }

    pub fn is_open(&self) -> bool {
        self.seed.is_some()
    }

    pub fn close(&mut self) {
        self.seed = None;
        self.job = None;
        self.notice.clear();
    }

    pub fn set_notice(&mut self, notice: impl Into<String>) {
        self.notice = notice.into();
    }

    /// Records text the user replaced into the target, so a later Replace
    /// looks for this version.
    pub fn replaced(&mut self, text: &str) {
        if let Some(seed) = self.seed.as_mut() {
            seed.text = text.to_owned();
        }
    }

    pub fn snapshot(&mut self) -> ReviewView {
        self.poll();
        let seed = self.seed.as_ref();
        ReviewView {
            revision: self.revision,
            text: self.text.clone(),
            strength: self.strength,
            can_replace: seed.is_some_and(|seed| seed.can_replace),
            busy: self.job.is_some(),
            formatting_note: seed.and_then(|seed| seed.formatting_note.clone()),
            notice: self.notice.clone(),
        }
    }

    /// Starts one reformat. `run` performs the model call off the caller's
    /// thread and receives the profile and the edited text.
    pub fn start_reformat(
        &mut self,
        text: &str,
        strength: FormattingStrength,
        instruction: Option<&str>,
        run: impl FnOnce(FormatProfile, String) -> ReformatResult + Send + 'static,
    ) -> Result<(), String> {
        let seed = self.seed.as_ref().ok_or("Nothing to reformat.")?;
        if self.job.is_some() {
            return Err("A reformat is already running.".into());
        }
        if text.trim().is_empty() {
            return Err("There is no text to reformat.".into());
        }
        if text.len() > MAX_REVIEW_TEXT_BYTES {
            return Err("This text is too long to reformat here.".into());
        }
        let profile = reformat_profile(strength, seed.custom_instructions.as_deref(), instruction)?;
        self.strength = strength;
        self.text = text.to_owned();
        let (sender, receiver) = mpsc::sync_channel(1);
        let text = text.to_owned();
        std::thread::Builder::new()
            .name("phorminx-review-reformat".into())
            .spawn(move || {
                let _ = sender.send(run(profile, text));
            })
            .map_err(|_| "Reformat could not start.".to_owned())?;
        self.job = Some(receiver);
        self.notice = "Reformatting with the local model…".into();
        Ok(())
    }

    fn poll(&mut self) {
        let Some(job) = self.job.as_ref() else {
            return;
        };
        let result = match job.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => Err("Reformat stopped unexpectedly.".into()),
        };
        self.job = None;
        match result {
            Ok(Reformatted { text, note }) => {
                self.text = text;
                self.revision = self.revision.wrapping_add(1);
                self.notice = match note {
                    Some(note) => format!("{note}. Copy it, or replace the pasted text."),
                    None => "Reformatted. Copy it, or replace the pasted text.".into(),
                };
            }
            Err(reason) => self.notice = format!("{reason} Your text is unchanged."),
        }
    }
}

/// The profile for one reformat: the chosen strength, plus an optional
/// instruction that applies to this text only.
pub fn reformat_profile(
    strength: FormattingStrength,
    custom_instructions: Option<&str>,
    one_off: Option<&str>,
) -> Result<FormatProfile, String> {
    let base = match strength {
        FormattingStrength::Raw | FormattingStrength::Light => FormatProfile::Light,
        FormattingStrength::Balanced => FormatProfile::Balanced,
        FormattingStrength::Strong => FormatProfile::Strong,
        FormattingStrength::Structured => FormatProfile::Structured,
        FormattingStrength::Custom => FormatProfile::Custom(
            custom_instructions
                .filter(|text| !text.trim().is_empty())
                .ok_or("Add custom instructions in Settings, or pick another formatting.")?
                .to_owned(),
        ),
    };
    let Some(one_off) = one_off.map(str::trim).filter(|text| !text.is_empty()) else {
        return Ok(base);
    };
    if one_off.len() > MAX_ONE_OFF_INSTRUCTION_BYTES {
        return Err("The one-off instruction is too long.".into());
    }
    let instruction = base.instruction().unwrap_or_default();
    Ok(FormatProfile::Custom(format!(
        "{instruction}\n\nFor this text only: {one_off}"
    )))
}

/// Reformats review text with the user's verified local model. The pinned
/// model identity is re-checked first, exactly as a dictation would.
pub fn reformat_with_local_model(
    settings: &Settings,
    profile: FormatProfile,
    text: String,
) -> ReformatResult {
    let model = settings
        .formatting
        .ollama_model
        .clone()
        .ok_or("Choose a local model in Settings → Dictation first.")?;
    let model = ModelName::parse(model).map_err(|_| "The selected local model name is invalid.")?;
    let identity = settings
        .formatting
        .ollama_model_identity
        .clone()
        .ok_or("Select the local model again in Settings to verify it.")?;
    let probe = OllamaClient::new(
        OllamaEndpoint::default(),
        ClientTimeouts {
            connect: Duration::from_secs(1),
            response_headers: Duration::from_secs(4),
            response_body: Duration::from_secs(2),
            overall: Duration::from_secs(5),
        },
    )
    .map_err(|error| error.to_string())?;
    if !identity_matches(&probe, &model, &identity) {
        return Err("The local model is not running or has changed.".into());
    }
    let client = OllamaClient::new(
        OllamaEndpoint::default(),
        ClientTimeouts {
            connect: Duration::from_secs(1),
            response_headers: Duration::from_secs(45),
            response_body: Duration::from_secs(10),
            overall: Duration::from_secs(60),
        },
    )
    .map_err(|error| error.to_string())?;
    let keep_alive = match settings.formatting.ollama_lifecycle {
        OllamaLifecycle::Instant => KeepAlive::Indefinite,
        OllamaLifecycle::Balanced => KeepAlive::For(Duration::from_secs(15 * 60)),
        OllamaLifecycle::MemorySaver => KeepAlive::UnloadAfterRequest,
    };
    let outcome = client.format_passes(
        &model,
        &text,
        &profile,
        keep_alive,
        &CancellationToken::new(),
        settings.formatting.passes,
    );
    match (outcome.completed, outcome.failure) {
        (0, Some(reason)) => {
            let label = format!("ollama_fallback:{}", fallback_label(&reason));
            let note = formatting_fallback_note(&[label])
                .unwrap_or("Unformatted: the model's rewrite was not used");
            Err(sentence(note.trim_start_matches("Unformatted: ")))
        }
        (completed, _) => Ok(Reformatted {
            text: phorminx_core::normalize_formatted(&outcome.text),
            note: (outcome.requested > 1).then(|| {
                passes_note(completed, outcome.requested).replacen("Formatted", "Reformatted", 1)
            }),
        }),
    }
}

/// Content-free warning recording how many formatting passes applied.
pub fn passes_warning(completed: u8, requested: u8) -> String {
    format!("formatting_passes:{completed}/{requested}")
}

fn passes_note(completed: u8, requested: u8) -> String {
    format!("Formatted in {completed} of {requested} passes")
}

/// "Formatted in 2 of 3 passes" for a multi-pass dictation.
pub fn formatting_passes_note(warnings: &[String]) -> Option<String> {
    warnings.iter().find_map(|warning| {
        let (completed, requested) = warning
            .strip_prefix("formatting_passes:")?
            .split_once('/')?;
        Some(passes_note(
            completed.parse().ok()?,
            requested.parse().ok()?,
        ))
    })
}

/// Everything worth saying about a dictation's formatting, for History and
/// the review panel: the passes that applied and why formatting fell back.
pub fn formatting_summary(warnings: &[String]) -> Option<String> {
    let parts = [
        formatting_passes_note(warnings),
        formatting_fallback_note(warnings).map(str::to_owned),
    ];
    let parts = parts.into_iter().flatten().collect::<Vec<_>>();
    (!parts.is_empty()).then(|| parts.join(" · "))
}

/// "the model added commentary" becomes "The model added commentary."
fn sentence(text: &str) -> String {
    let mut characters = text.chars();
    match characters.next() {
        Some(first) => format!("{}{}.", first.to_uppercase(), characters.as_str()),
        None => String::new(),
    }
}

fn identity_matches(
    client: &OllamaClient,
    model: &ModelName,
    expected: &OllamaModelIdentity,
) -> bool {
    client
        .discover(&CancellationToken::new())
        .ok()
        .and_then(|catalog| {
            catalog
                .select(&SelectionPolicy::Exact(model.clone()))
                .ok()
                .cloned()
        })
        .is_some_and(|installed| expected.matches(installed.digest.as_deref(), installed.size))
}

/// Content-free label stored in a dictation's warnings when formatting fell
/// back to the deterministic text.
pub fn fallback_label(reason: &FallbackReason) -> &'static str {
    match reason {
        FallbackReason::RawProfile => "raw_profile",
        FallbackReason::PromptRejected(_) => "prompt_rejected",
        FallbackReason::Cancelled => "cancelled",
        FallbackReason::ServiceUnavailable(message) => service_label(message),
        FallbackReason::OutputRejected(error) => match error {
            ValidationError::ProtectedTokenChanged { .. } => "output_rejected_protected",
            ValidationError::ModelCommentary => "output_rejected_commentary",
            ValidationError::OutputTooLarge { .. } => "output_rejected_size",
            ValidationError::EmptyOutput => "output_rejected_empty",
            ValidationError::UnexpectedOutputForEmptyInput | ValidationError::ControlCharacter => {
                "output_rejected"
            }
        },
    }
}

/// Splits request failures so the note says what actually happened: a slow
/// model, a request Ollama refused, or no Ollama at all.
fn service_label(message: &str) -> &'static str {
    if message.starts_with("Ollama request timed out") {
        "service_timeout"
    } else if [
        "Ollama returned HTTP status",
        "Ollama reported an error",
        "Ollama returned invalid JSON",
        "Ollama generation ended",
    ]
    .iter()
    .any(|prefix| message.starts_with(prefix))
    {
        "service_rejected"
    } else {
        "service_unavailable"
    }
}

/// A short, content-free explanation when AI formatting did not apply.
///
/// `warnings` are the persisted `ollama_fallback:*` / `ollama_document:*`
/// labels of one dictation. `None` means formatting applied or was not
/// requested.
pub fn formatting_fallback_note(warnings: &[String]) -> Option<&'static str> {
    warnings.iter().find_map(|warning| {
        let reason = warning
            .strip_prefix("ollama_fallback:")
            .or_else(|| warning.strip_prefix("ollama_document:"))?;
        Some(match reason {
            "service_timeout" => "Unformatted: the local model did not answer in time",
            "service_rejected" => "Unformatted: Ollama rejected the request",
            "service_unavailable" | "service_lost" => "Unformatted: Ollama is not reachable",
            "model_identity_unverified" => "Unformatted: the local model is still starting",
            "prompt_rejected" => "Unformatted: check the custom instructions",
            "output_rejected_protected" => {
                "Unformatted: the rewrite changed a name, number or link"
            }
            "output_rejected_commentary" => "Unformatted: the model added commentary",
            "output_rejected_size" => "Unformatted: the rewrite was far longer than the dictation",
            "output_rejected_empty" => "Unformatted: the model returned no text",
            "output_rejected" => "Unformatted: the model's rewrite was rejected",
            "document_rejected" | "raw_bypass" => "Unformatted: this dictation was too long",
            "partially_formatted" | "output_limit" => {
                "Partly formatted: some passages kept as spoken"
            }
            "cancelled" => "Formatting was cancelled",
            "completed" | "raw_profile" => return None,
            _ => "Unformatted: formatting was skipped",
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn seed() -> ReviewSeed {
        ReviewSeed {
            text: "first draft".into(),
            can_replace: true,
            strength: FormattingStrength::Balanced,
            custom_instructions: Some("Use bullets.".into()),
            formatting_note: Some("Unformatted: the local model did not answer in time".into()),
        }
    }

    fn settle(panel: &mut ReviewPanel) -> ReviewView {
        let started = Instant::now();
        loop {
            let view = panel.snapshot();
            if !view.busy || started.elapsed() > Duration::from_secs(5) {
                return view;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn reformat_replaces_text_and_bumps_revision() {
        let mut panel = ReviewPanel::default();
        assert!(
            panel
                .start_reformat("x", FormattingStrength::Light, None, |_, text| {
                    Ok(Reformatted { text, note: None })
                })
                .is_err()
        );
        panel.open(seed());
        let opened = panel.snapshot();
        assert_eq!(opened.text, "first draft");
        assert!(opened.can_replace);
        assert_eq!(
            opened.formatting_note.as_deref(),
            seed().formatting_note.as_deref()
        );
        panel
            .start_reformat(
                "edited draft",
                FormattingStrength::Structured,
                None,
                |profile, text| {
                    assert_eq!(profile, FormatProfile::Structured);
                    Ok(Reformatted {
                        text: format!("- {text}"),
                        note: Some("Reformatted in 2 of 2 passes".into()),
                    })
                },
            )
            .unwrap();
        let done = settle(&mut panel);
        assert_eq!(done.text, "- edited draft");
        assert!(done.notice.starts_with("Reformatted in 2 of 2 passes."));
        assert_eq!(done.strength, FormattingStrength::Structured);
        assert!(done.revision > opened.revision);
        assert!(!done.busy);
    }

    #[test]
    fn failed_reformat_keeps_the_edited_text() {
        let mut panel = ReviewPanel::default();
        panel.open(seed());
        let before = panel.snapshot().revision;
        panel
            .start_reformat("my edit", FormattingStrength::Strong, None, |_, _| {
                Err("The local model did not answer.".into())
            })
            .unwrap();
        let done = settle(&mut panel);
        assert_eq!(done.text, "my edit");
        assert_eq!(
            done.revision, before,
            "a failure never overwrites the editor"
        );
        assert!(done.notice.contains("unchanged"));
    }

    #[test]
    fn passes_are_reported_with_any_fallback() {
        let warnings = |items: &[&str]| items.iter().map(|w| (*w).to_owned()).collect::<Vec<_>>();
        assert_eq!(passes_warning(1, 3), "formatting_passes:1/3");
        assert_eq!(
            formatting_summary(&warnings(&["formatting_passes:2/2"])).as_deref(),
            Some("Formatted in 2 of 2 passes")
        );
        assert_eq!(
            formatting_summary(&warnings(&[
                "formatting_passes:0/2",
                "ollama_fallback:service_timeout"
            ]))
            .as_deref(),
            Some(
                "Formatted in 0 of 2 passes · Unformatted: the local model did not answer in time"
            )
        );
        assert_eq!(
            formatting_summary(&warnings(&["recognition_repeated_phrase"])),
            None
        );
        assert_eq!(
            formatting_passes_note(&warnings(&["formatting_passes:x/2"])),
            None
        );
    }

    #[test]
    fn request_failures_say_what_happened() {
        let label =
            |message: &str| fallback_label(&FallbackReason::ServiceUnavailable(message.to_owned()));
        assert_eq!(label("Ollama request timed out"), "service_timeout");
        assert_eq!(label("Ollama returned HTTP status 400"), "service_rejected");
        assert_eq!(
            label("Ollama reported an error: time: missing unit in duration"),
            "service_rejected"
        );
        assert_eq!(
            label("Ollama transport failed: connection refused"),
            "service_unavailable"
        );
        assert_eq!(
            note(&["ollama_fallback:service_rejected"]),
            Some("Unformatted: Ollama rejected the request")
        );
    }

    #[test]
    fn reformat_failures_read_as_sentences() {
        assert_eq!(
            sentence("the model added commentary"),
            "The model added commentary."
        );
        assert_eq!(sentence(""), "");
        assert_eq!(
            fallback_label(&FallbackReason::OutputRejected(
                ValidationError::ModelCommentary
            )),
            "output_rejected_commentary"
        );
    }

    #[test]
    fn one_off_instructions_extend_the_chosen_formatting() {
        assert_eq!(
            reformat_profile(FormattingStrength::Strong, None, Some("  ")).unwrap(),
            FormatProfile::Strong
        );
        let FormatProfile::Custom(text) = reformat_profile(
            FormattingStrength::Structured,
            None,
            Some("make it an email"),
        )
        .unwrap() else {
            panic!("a one-off instruction becomes a custom profile");
        };
        assert!(text.starts_with("Organize the dictation"));
        assert!(text.ends_with("For this text only: make it an email"));
        assert!(reformat_profile(FormattingStrength::Custom, None, None).is_err());
        assert_eq!(
            reformat_profile(FormattingStrength::Custom, Some("Bullets"), None).unwrap(),
            FormatProfile::Custom("Bullets".into())
        );
        assert!(
            reformat_profile(FormattingStrength::Light, None, Some(&"x".repeat(2000))).is_err()
        );
    }

    fn note(warnings: &[&str]) -> Option<&'static str> {
        formatting_fallback_note(&warnings.iter().map(|w| (*w).to_owned()).collect::<Vec<_>>())
    }

    #[test]
    fn fallback_reasons_have_short_explanations() {
        assert_eq!(note(&[]), None);
        assert_eq!(note(&["recognition_repeated_phrase"]), None);
        assert_eq!(note(&["ollama_fallback:raw_profile"]), None);
        assert_eq!(
            note(&[
                "recognition_repeated_phrase",
                "ollama_fallback:service_unavailable"
            ]),
            Some("Unformatted: Ollama is not reachable")
        );
        assert_eq!(
            note(&["ollama_fallback:output_rejected_protected"]),
            Some("Unformatted: the rewrite changed a name, number or link")
        );
        assert_eq!(
            note(&["ollama_fallback:something_new"]),
            Some("Unformatted: formatting was skipped")
        );
        for warning in [
            "ollama_fallback:service_unavailable",
            "ollama_fallback:model_identity_unverified",
            "ollama_fallback:prompt_rejected",
            "ollama_fallback:output_rejected_commentary",
            "ollama_fallback:output_rejected_size",
            "ollama_fallback:output_rejected_empty",
            "ollama_document:partially_formatted",
        ] {
            // The status pill shows at most 80 characters.
            assert!(note(&[warning]).unwrap().chars().count() <= 80, "{warning}");
        }
    }
}
