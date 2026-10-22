//! Formatting: every strength mode on every model, one and two passes, English
//! and Portuguese, cold (model unloaded first) and warm.

use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use phorminx_core::benchmark_report::{Example, Measure, mean, word_change_share};
use phorminx_ollama::{
    CancellationToken, ClientTimeouts, KeepAlive, ModelName, OllamaClient, OllamaEndpoint,
    restructures,
};
use serde_json::json;

use super::{FeatureResult, run};
use crate::format_bench::{SAMPLES, Sample, list_items, profile_from_name};

pub const FEATURE: &str = "formatting";
pub const MODES: &[&str] = &["light", "balanced", "strong", "structured", "custom"];
pub const CUSTOM: &str = "Use short bullet points.";

pub struct Config {
    pub models: Vec<String>,
    pub passes: Vec<u8>,
    pub repeats: u32,
    pub cold_repeats: u32,
}

/// What a good result keeps and drops, per sample.
struct Expectation {
    keep: &'static [&'static str],
    fillers: &'static [&'static str],
    /// List lines expected from modes that restructure.
    list_min: usize,
}

fn expectation(sample: &str) -> Expectation {
    match sample {
        "status-update" => Expectation {
            keep: &[
                "login page",
                "settings screen",
                "dark mode",
                "language picker",
                "export",
                "friday",
                "3",
                "Ana",
                "agenda",
            ],
            fillers: &["um"],
            list_min: 3,
        },
        "email-numbers-url" => Expectation {
            keep: &[
                "Carlos",
                "12,500",
                "phase 1",
                "march 14",
                "https://example.com/spec",
                "ops@example.com",
            ],
            fillers: &[],
            list_min: 0,
        },
        "rambling-idea" => Expectation {
            keep: &["onboarding", "tour", "one screen", "shortcut"],
            fillers: &["um", "like"],
            list_min: 0,
        },
        "short-note" => Expectation {
            keep: &["dentist", "tomorrow morning"],
            fillers: &[],
            list_min: 0,
        },
        "shopping-list" => Expectation {
            keep: &["sunscreen", "towels", "charger", "snacks", "passports"],
            fillers: &["um"],
            list_min: 4,
        },
        "portuguese" => Expectation {
            keep: &[
                "reunião",
                "10",
                "orçamento",
                "segundo semestre",
                "designer",
                "lançamento",
                "novembro",
            ],
            fillers: &[],
            list_min: 3,
        },
        "portuguese-tasks" => Expectation {
            keep: &["pão", "leite", "café", "João", "4.500", "Marta", "sexta"],
            fillers: &[],
            list_min: 3,
        },
        "portuguese-email" => Expectation {
            keep: &[
                "Fernanda",
                "proposta",
                "15 de dezembro",
                "R$",
                "contato@exemplo.com.br",
            ],
            fillers: &["tipo", "né"],
            list_min: 0,
        },
        _ => Expectation {
            keep: &[],
            fillers: &[],
            list_min: 0,
        },
    }
}

/// 0-100, see the `quality` metric description.
pub fn quality(sample: &str, mode: &str, output: &str) -> f64 {
    let expected = expectation(sample);
    let lower = output.to_lowercase();
    let squashed: String = lower
        .chars()
        .filter(|c| !matches!(c, ',' | '.' | ' '))
        .collect();
    let kept = if expected.keep.is_empty() {
        1.0
    } else {
        expected
            .keep
            .iter()
            .filter(|token| {
                let token = token.to_lowercase();
                // Numbers may be written 12500, 12,500 or 12.500.
                lower.contains(&token)
                    || (token.chars().any(|c| c.is_ascii_digit())
                        && squashed.contains(&token.replace([',', '.', ' '], "")))
            })
            .count() as f64
            / expected.keep.len() as f64
    };
    let words: Vec<String> = lower
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_owned)
        .collect();
    let fillers_removed = if expected.fillers.is_empty() {
        1.0
    } else {
        expected
            .fillers
            .iter()
            .filter(|filler| !words.iter().any(|word| word == *filler))
            .count() as f64
            / expected.fillers.len() as f64
    };
    let trimmed = output.trim();
    let starts_upper = trimmed
        .trim_start_matches(['-', '*', ' ', '#'])
        .chars()
        .next()
        .is_some_and(|c| c.is_uppercase() || c.is_ascii_digit());
    let punctuated = trimmed.ends_with(['.', '!', '?', ':']) || list_items(trimmed) > 0;
    let sentence = (f64::from(u8::from(starts_upper)) + f64::from(u8::from(punctuated))) / 2.0;
    let structure = if expected.list_min > 0 && wants_structure(mode) {
        (list_items(output) as f64 / expected.list_min as f64).min(1.0)
    } else {
        1.0
    };
    (50.0 * kept + 20.0 * fillers_removed + 15.0 * sentence + 15.0 * structure).round()
}

fn wants_structure(mode: &str) -> bool {
    match mode {
        "custom" => true,
        "raw" => false,
        other => profile_from_name(other, None)
            .map(|profile| restructures(&profile))
            .unwrap_or(false),
    }
}

fn samples(language: &str) -> Vec<&'static Sample> {
    SAMPLES
        .iter()
        .filter(|sample| sample.language == language)
        .collect()
}

/// `Ollama 0.x · 100% GPU`, from `ollama ps` while `model` is loaded.
fn backend(model: &str) -> String {
    let version = std::process::Command::new("ollama")
        .arg("--version")
        .output()
        .ok()
        .map(|output| String::from_utf8_lossy(&output.stdout).to_string())
        .and_then(|text| text.split_whitespace().last().map(str::to_owned))
        .unwrap_or_default();
    let processor = std::process::Command::new("ollama")
        .arg("ps")
        .output()
        .ok()
        .map(|output| String::from_utf8_lossy(&output.stdout).to_string())
        .and_then(|text| {
            text.lines()
                .find(|line| line.split_whitespace().next() == Some(model))
                .and_then(|line| {
                    // Columns are padded with runs of spaces: NAME ID SIZE PROCESSOR ...
                    line.split("  ")
                        .map(str::trim)
                        .find(|part| part.contains("GPU") || part.contains("CPU"))
                        .map(str::to_owned)
                })
        })
        .unwrap_or_else(|| "unknown placement".into());
    format!("Ollama {version} · {processor}")
}

/// Below this a model is partly in system memory (an 8 GB card shared with
/// the desktop can push it there even when Ollama reports 100% GPU).
const MIN_DECODE_RATE: f64 = 12.0;

/// Decode speed of `model` in tokens per second, from Ollama's own timing.
pub fn decode_rate(model: &str) -> Option<f64> {
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(180)))
        .build()
        .new_agent();
    let body = json!({
        "model": model,
        "prompt": "Count from one to forty in words, separated by commas.",
        "stream": false,
        "keep_alive": "10m",
        "options": {"num_predict": 48, "temperature": 0},
    });
    let text = agent
        .post("http://127.0.0.1:11434/api/generate")
        .header("Content-Type", "application/json")
        .send(body.to_string())
        .ok()?
        .body_mut()
        .read_to_string()
        .ok()?;
    let response: serde_json::Value = serde_json::from_str(&text).ok()?;
    let count = response["eval_count"].as_f64()?;
    let nanos = response["eval_duration"].as_f64()?;
    (nanos > 0.0).then(|| count / (nanos / 1e9))
}

/// Loads `model` and reloads it, up to three times, until it decodes at GPU
/// speed. Returns the last measured rate.
pub fn settle(model: &str) -> f64 {
    let mut rate = 0.0;
    for attempt in 1..=3 {
        rate = decode_rate(model).unwrap_or(0.0);
        if rate >= MIN_DECODE_RATE || attempt == 3 {
            break;
        }
        println!("{model} decodes at {rate:.1} tok/s (attempt {attempt}); reloading");
        super::meetings::unload(model);
        std::thread::sleep(Duration::from_secs(5));
    }
    rate
}

struct Measured {
    seconds: f64,
    formatted: bool,
    quality: f64,
    later_change: Option<f64>,
    list_items: usize,
    output: String,
}

fn measure_one(
    client: &OllamaClient,
    model: &ModelName,
    sample: &Sample,
    mode: &str,
    passes: u8,
    keep: &KeepAlive,
    cancel: &CancellationToken,
) -> Result<Measured> {
    let profile = profile_from_name(mode, Some(CUSTOM))?;
    let started = Instant::now();
    let outcome = client.format_passes(model, sample.text, &profile, keep.clone(), cancel, passes);
    let seconds = started.elapsed().as_secs_f64();
    let first = outcome
        .attempts
        .first()
        .filter(|attempt| !attempt.result.is_fallback())
        .map(|attempt| attempt.result.text().to_owned());
    let later_change = match first {
        Some(first) if passes > 1 && outcome.completed > 1 => {
            Some(word_change_share(&first, &outcome.text))
        }
        _ if passes > 1 => Some(0.0),
        _ => None,
    };
    Ok(Measured {
        seconds,
        formatted: outcome.completed > 0,
        quality: quality(sample.name, mode, &outcome.text),
        later_change,
        list_items: list_items(&outcome.text),
        output: outcome.text,
    })
}

fn summarize(measured: &[Measured]) -> Vec<(&'static str, Measure)> {
    let seconds: Vec<f64> = measured.iter().map(|m| m.seconds).collect();
    let pass = 100.0 * measured.iter().filter(|m| m.formatted).count() as f64
        / measured.len().max(1) as f64;
    let quality: Vec<f64> = measured.iter().map(|m| m.quality).collect();
    let lists: Vec<f64> = measured.iter().map(|m| m.list_items as f64).collect();
    let mut metrics = vec![
        ("latency", Measure::of(&seconds, "s")),
        ("passRate", Measure::new(pass, "%")),
        ("quality", Measure::new(mean(&quality), "score")),
        ("listItems", Measure::new(mean(&lists), "count")),
    ];
    let changes: Vec<f64> = measured.iter().filter_map(|m| m.later_change).collect();
    if !changes.is_empty() {
        metrics.push(("laterPassChange", Measure::new(mean(&changes), "%")));
    }
    metrics
}

pub fn measure(config: &Config) -> Result<FeatureResult> {
    let mut result = FeatureResult {
        note: format!(
            "{} synthetic dictations ({} English, {} Portuguese). Custom mode uses \"{CUSTOM}\" Temperature 0.",
            SAMPLES.len(),
            samples("en").len(),
            samples("pt").len()
        ),
        ..Default::default()
    };
    // Raw inserts the text as spoken: the baseline every mode is compared with.
    for language in ["en", "pt"] {
        let raw: Vec<Measured> = samples(language)
            .into_iter()
            .map(|sample| Measured {
                seconds: 0.0,
                formatted: true,
                quality: quality(sample.name, "raw", sample.text),
                later_change: None,
                list_items: list_items(sample.text),
                output: sample.text.to_owned(),
            })
            .collect();
        for start in ["warm", "cold"] {
            result.runs.push(run(
                FEATURE,
                None,
                "none",
                &[
                    ("mode", json!("raw")),
                    ("passes", json!(1)),
                    ("language", json!(language)),
                    ("start", json!(start)),
                ],
                raw.len(),
                summarize(&raw),
            ));
        }
    }

    // The app's own formatting policy: a request that takes longer falls
    // back to the text as spoken, so a slow model shows up in the pass rate.
    let client = OllamaClient::new(
        OllamaEndpoint::default(),
        ClientTimeouts {
            connect: Duration::from_secs(1),
            response_headers: Duration::from_secs(45),
            response_body: Duration::from_secs(10),
            overall: Duration::from_secs(60),
        },
    )
    .context("invalid Ollama client configuration")?;
    let cancel = CancellationToken::new();
    let catalog = client
        .discover(&cancel)
        .context("Ollama is not reachable")?;
    let keep = KeepAlive::For(Duration::from_secs(600));
    for tag in &config.models {
        let model = ModelName::parse(tag.clone()).context("invalid model name")?;
        if !catalog
            .models()
            .iter()
            .any(|candidate| candidate.name == model)
        {
            result.skip(FEATURE, tag, "not installed in Ollama");
            continue;
        }
        // Unload the other models under test so each one is measured alone.
        for other in &config.models {
            if let Ok(other) = ModelName::parse(other.clone()) {
                let _ = client.unload(&other, &cancel);
            }
        }
        let _ = client.warm_up(&model, keep.clone(), &cancel);
        // A model that does not fit beside everything else on the GPU stays
        // slow however often it is reloaded: measure it once per setup.
        let fits = settle(tag) >= MIN_DECODE_RATE;
        let (repeats, cold_repeats) = if fits {
            (config.repeats.max(1), config.cold_repeats.max(1))
        } else {
            result.skip(FEATURE, &format!("{tag} repeats"), "it did not fit in GPU memory on the test desktop and ran slowly, so each setup ran once");
            (1, 1)
        };
        let backend = backend(tag);
        println!("{tag}: {backend}");

        for language in ["en", "pt"] {
            let language_samples = samples(language);
            for mode in MODES {
                for &passes in &config.passes {
                    let mut warm = Vec::new();
                    for _ in 0..repeats {
                        for sample in &language_samples {
                            warm.push(measure_one(
                                &client, &model, sample, mode, passes, &keep, &cancel,
                            )?);
                        }
                    }
                    let config_pairs = [
                        ("mode", json!(mode)),
                        ("passes", json!(passes)),
                        ("language", json!(language)),
                        ("start", json!("warm")),
                    ];
                    let mut metrics = summarize(&warm);
                    metrics.push((
                        "decodeRate",
                        Measure::new(decode_rate(tag).unwrap_or(0.0), "tok/s"),
                    ));
                    let built = run(
                        FEATURE,
                        Some(tag),
                        &backend,
                        &config_pairs,
                        warm.len(),
                        metrics,
                    );
                    println!(
                        "{tag} {mode} x{passes} {language} warm {:.2}s q{:.0} pass {:.0}% {:.0} tok/s",
                        built.metrics["latency"].value,
                        built.metrics["quality"].value,
                        built.metrics["passRate"].value,
                        built.metrics["decodeRate"].value
                    );
                    result.examples.push(Example {
                        feature: FEATURE.to_owned(),
                        run_id: built.id.clone(),
                        sample: language_samples[0].name.to_owned(),
                        language: language.to_owned(),
                        input: language_samples[0].text.to_owned(),
                        output: warm[0].output.clone(),
                    });
                    result.runs.push(built);

                    // Cold: the model is unloaded before each request.
                    let mut cold = Vec::new();
                    for _ in 0..cold_repeats {
                        let _ = client.unload(&model, &cancel);
                        std::thread::sleep(Duration::from_secs(2));
                        cold.push(measure_one(
                            &client,
                            &model,
                            language_samples[0],
                            mode,
                            passes,
                            &keep,
                            &cancel,
                        )?);
                    }
                    let cold_pairs = [
                        ("mode", json!(mode)),
                        ("passes", json!(passes)),
                        ("language", json!(language)),
                        ("start", json!("cold")),
                    ];
                    let built = run(
                        FEATURE,
                        Some(tag),
                        &backend,
                        &cold_pairs,
                        cold.len(),
                        summarize(&cold),
                    );
                    println!(
                        "{tag} {mode} x{passes} {language} cold {:.2}s",
                        built.metrics["latency"].value
                    );
                    result.runs.push(built);
                    // A reload can land partly in system memory; fix that before the next warm set.
                    if fits {
                        settle(tag);
                    }
                }
            }
        }
        let _ = client.unload(&model, &cancel);
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quality_rewards_kept_facts_clean_sentences_and_requested_lists() {
        let raw = SAMPLES
            .iter()
            .find(|sample| sample.name == "shopping-list")
            .unwrap();
        let raw_score = quality("shopping-list", "raw", raw.text);
        let good = "For the trip we need:\n- Sunscreen\n- Two towels\n- The camera charger\n- Snacks for the car\n- Passports (don't forget them)";
        assert_eq!(quality("shopping-list", "structured", good), 100.0);
        assert!(raw_score < 80.0, "{raw_score}");
        let dropped = "For the trip we need sunscreen and towels.";
        assert!(
            quality("shopping-list", "balanced", dropped)
                < quality("shopping-list", "balanced", good)
        );
        assert_eq!(
            quality(
                "email-numbers-url",
                "light",
                "Hi Carlos, the budget is $12,500 for phase 1, due March 14. Spec: https://example.com/spec. Changes to ops@example.com."
            ),
            100.0
        );
    }

    #[test]
    fn every_sample_has_expectations() {
        for sample in SAMPLES {
            assert!(!expectation(sample.name).keep.is_empty(), "{}", sample.name);
        }
        assert!(wants_structure("structured") && wants_structure("custom"));
        assert!(!wants_structure("raw"));
    }
}
