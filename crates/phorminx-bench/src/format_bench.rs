//! Formatting test bench: runs sample transcripts through every formatting
//! profile against a local Ollama model and reports the raw model output, the
//! validator's verdict and the text Phorminx would insert.
//!
//! The samples are synthetic dictations written for this bench. Nothing is
//! read from a user's history.

use std::fmt::Write as _;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use phorminx_ollama::{
    CancellationToken, ClientTimeouts, FallbackReason, FormatProfile, FormatResult, KeepAlive,
    ModelName, OllamaClient, OllamaEndpoint,
};

pub struct Sample {
    pub name: &'static str,
    pub text: &'static str,
}

pub const SAMPLES: &[Sample] = &[
    Sample {
        name: "status-update",
        text: "so um I wanted to give you an update on the project we finished the login page yesterday and the next thing is the settings screen I think we need three things first the dark mode toggle second the language picker and third a way to export the data also the meeting on friday moved to 3 pm and Ana will send the agenda before that",
    },
    Sample {
        name: "email-numbers-url",
        text: "hi Carlos thanks for the call today just to confirm the budget is 12500 dollars for phase 1 and the deadline is march 14 you can find the spec at https://example.com/spec and if anything changes send it to ops@example.com thanks",
    },
    Sample {
        name: "rambling-idea",
        text: "okay so I was thinking that maybe we should um change how the onboarding works because right now people get lost like they open the app and they don't know what to do so maybe we show them a short tour or maybe just one screen that explains the shortcut and then they try it once and that's it I don't know I think the one screen is better because tours are annoying",
    },
    Sample {
        name: "short-note",
        text: "remind me to call the dentist tomorrow morning",
    },
    Sample {
        name: "shopping-list",
        text: "for the trip we need sunscreen two towels the charger for the camera snacks for the car and um the passports don't forget the passports",
    },
    Sample {
        name: "portuguese",
        text: "então pessoal a reunião de amanhã vai ser às 10 horas e a gente precisa decidir três coisas o orçamento do segundo semestre a contratação do novo designer e a data do lançamento que provavelmente vai ficar para novembro",
    },
];

pub struct BenchConfig {
    pub model: String,
    pub profiles: Vec<String>,
    pub custom: Option<String>,
    pub samples: Option<Vec<String>>,
    pub output: Option<PathBuf>,
    pub timeout_seconds: u64,
}

pub fn run(config: BenchConfig) -> Result<()> {
    let model = ModelName::parse(config.model.clone()).context("invalid model name")?;
    let timeout = Duration::from_secs(config.timeout_seconds.max(1));
    let client = OllamaClient::new(
        OllamaEndpoint::default(),
        ClientTimeouts {
            connect: Duration::from_secs(2),
            response_headers: timeout,
            response_body: timeout,
            overall: timeout,
        },
    )
    .context("invalid Ollama client configuration")?;
    let cancel = CancellationToken::new();
    // Load once so the first case does not measure a cold model load.
    let _ = client.warm_up(&model, KeepAlive::For(Duration::from_secs(300)), &cancel);

    let mut report = String::new();
    let mut summary = Vec::new();
    writeln!(report, "# Formatting bench · {}\n", config.model)?;
    for sample in SAMPLES {
        if config
            .samples
            .as_ref()
            .is_some_and(|wanted| !wanted.iter().any(|name| name == sample.name))
        {
            continue;
        }
        writeln!(report, "## {}\n\n> {}\n", sample.name, sample.text)?;
        for profile_name in &config.profiles {
            let profile = profile_from_name(profile_name, config.custom.as_deref())?;
            let attempt = client.format_detailed(
                &model,
                sample.text,
                &profile,
                KeepAlive::For(Duration::from_secs(300)),
                &cancel,
            );
            let verdict = verdict(&attempt.result);
            let changed = attempt.result.text() != sample.text;
            summary.push((
                sample.name,
                profile_name.clone(),
                verdict.clone(),
                attempt.elapsed,
            ));
            writeln!(
                report,
                "### {profile_name} · {verdict} · {:.1}s\n",
                attempt.elapsed.as_secs_f64()
            )?;
            match attempt.raw_output.as_deref() {
                Some(raw) => writeln!(report, "Raw model output:\n\n```text\n{raw}\n```\n")?,
                None => writeln!(report, "Raw model output: (none)\n")?,
            }
            writeln!(
                report,
                "Inserted text{}:\n\n```text\n{}\n```\n",
                if changed { "" } else { " (unchanged)" },
                attempt.result.text()
            )?;
        }
    }
    writeln!(report, "## Summary\n")?;
    writeln!(report, "| sample | profile | verdict | seconds |")?;
    writeln!(report, "| --- | --- | --- | --- |")?;
    for (sample, profile, verdict, elapsed) in &summary {
        writeln!(
            report,
            "| {sample} | {profile} | {verdict} | {:.1} |",
            elapsed.as_secs_f64()
        )?;
    }
    let formatted = summary
        .iter()
        .filter(|(_, _, verdict, _)| verdict == "formatted")
        .count();
    writeln!(report, "\n{formatted}/{} formatted.", summary.len())?;

    match config.output {
        Some(path) => {
            std::fs::write(&path, &report)
                .with_context(|| format!("failed to write {}", path.display()))?;
            println!(
                "{formatted}/{} formatted; report: {}",
                summary.len(),
                path.display()
            );
        }
        None => print!("{report}"),
    }
    Ok(())
}

fn profile_from_name(name: &str, custom: Option<&str>) -> Result<FormatProfile> {
    Ok(match name {
        "light" => FormatProfile::Light,
        "balanced" => FormatProfile::Balanced,
        "strong" => FormatProfile::Strong,
        "structured" => FormatProfile::Structured,
        "custom" => FormatProfile::Custom(
            custom
                .context("--custom is required for the custom profile")?
                .to_owned(),
        ),
        other => anyhow::bail!("unknown profile {other}"),
    })
}

fn verdict(result: &FormatResult) -> String {
    match result {
        FormatResult::Formatted { .. } => "formatted".to_owned(),
        FormatResult::Fallback { reason, .. } => match reason {
            FallbackReason::RawProfile => "raw".to_owned(),
            FallbackReason::PromptRejected(error) => format!("prompt rejected: {error}"),
            FallbackReason::Cancelled => "cancelled".to_owned(),
            FallbackReason::ServiceUnavailable(error) => format!("service: {error}"),
            FallbackReason::OutputRejected(error) => format!("rejected: {error}"),
        },
    }
}
