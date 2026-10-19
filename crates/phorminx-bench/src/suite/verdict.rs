//! The nudges at the top of the page and the key numbers tracked across
//! reports. Nudges are deliberately plain: the fastest option, the best
//! result and a good default per feature, each with one line of why.

use phorminx_core::benchmark_report::{BenchmarkFile, KeyMetric, Nudge, Run, Verdict, mean};
use serde_json::{Value, json};

/// A formatting model above this warm average no longer feels instant.
const INTERACTIVE_SECONDS: f64 = 2.5;

fn matches(run: &Run, feature: &str, model: Option<&str>, config: &[(&str, Value)]) -> bool {
    run.feature == feature
        && model.is_none_or(|model| run.model.as_deref() == Some(model))
        && config
            .iter()
            .all(|(key, value)| run.config.get(*key) == Some(value))
}

fn find<'a>(
    file: &'a BenchmarkFile,
    feature: &str,
    model: Option<&str>,
    config: &[(&str, Value)],
) -> Option<&'a Run> {
    file.runs
        .iter()
        .find(|run| matches(run, feature, model, config))
}

fn value(run: &Run, metric: &str) -> Option<f64> {
    run.metrics.get(metric).map(|measure| measure.value)
}

/// Mean of `metric` per model over runs matching `config`, in first-seen order.
fn per_model(
    file: &BenchmarkFile,
    feature: &str,
    config: &[(&str, Value)],
    metric: &str,
) -> Vec<(String, f64)> {
    let mut models: Vec<String> = Vec::new();
    for run in file
        .runs
        .iter()
        .filter(|run| matches(run, feature, None, config))
    {
        if let Some(model) = &run.model
            && !models.contains(model)
        {
            models.push(model.clone());
        }
    }
    models
        .into_iter()
        .filter_map(|model| {
            let values: Vec<f64> = file
                .runs
                .iter()
                .filter(|run| matches(run, feature, Some(&model), config))
                .filter_map(|run| value(run, metric))
                .collect();
            (!values.is_empty()).then(|| (model, mean(&values)))
        })
        .collect()
}

fn lowest(values: &[(String, f64)]) -> Option<&(String, f64)> {
    values
        .iter()
        .min_by(|left, right| left.1.total_cmp(&right.1))
}

fn highest(values: &[(String, f64)]) -> Option<&(String, f64)> {
    values
        .iter()
        .max_by(|left, right| left.1.total_cmp(&right.1))
}

/// Every name within `tolerance` of the best value, best first.
fn tied(values: &[(String, f64)], best: f64, tolerance: f64) -> Vec<String> {
    let mut close: Vec<&(String, f64)> = values
        .iter()
        .filter(|(_, value)| (value - best).abs() <= tolerance)
        .collect();
    close.sort_by(|left, right| (left.1 - best).abs().total_cmp(&(right.1 - best).abs()));
    close.into_iter().map(|(name, _)| name.clone()).collect()
}

fn nudge(feature: &str, kind: &str, pick: impl Into<String>, why: impl Into<String>) -> Nudge {
    Nudge {
        feature: feature.to_owned(),
        kind: kind.to_owned(),
        pick: pick.into(),
        why: why.into(),
    }
}

fn label(model: &str) -> String {
    match model {
        "vosk-model-small-en-us-0.15" => "Instant (Vosk)".into(),
        other => format!("Accurate {other}"),
    }
}

fn key(
    file: &BenchmarkFile,
    key: &str,
    label: &str,
    feature: &str,
    model: Option<&str>,
    config: &[(&str, Value)],
    metric: &str,
) -> Option<KeyMetric> {
    let run = find(file, feature, model, config)?;
    let info = file.metrics.get(metric)?;
    run.metrics.get(metric)?;
    Some(KeyMetric {
        key: key.to_owned(),
        label: label.to_owned(),
        unit: info.unit.clone(),
        better: info.better.clone(),
        run_id: run.id.clone(),
        metric: metric.to_owned(),
    })
}

pub fn apply(file: &mut BenchmarkFile) {
    file.key_metrics = key_metrics(file);
    let nudges = nudges(file);
    let headline = {
        let pick = |feature: &str, kind: &str| {
            nudges
                .iter()
                .find(|n| n.feature == feature && n.kind == kind)
                .map(|n| n.pick.clone())
        };
        match (
            pick("dictation", "Fastest"),
            pick("formatting", "Good default"),
        ) {
            (Some(dictation), Some(formatting)) => {
                format!("{dictation} for live text, {formatting} for formatting")
            }
            (_, Some(formatting)) => format!("{formatting} is a good formatting default"),
            _ => "Quick nudges from the latest run".to_owned(),
        }
    };
    file.verdict = Some(Verdict { headline, nudges });
}

fn key_metrics(file: &BenchmarkFile) -> Vec<KeyMetric> {
    let warm_en = |mode: &str| {
        vec![
            ("mode", json!(mode)),
            ("passes", json!(1)),
            ("language", json!("en")),
            ("start", json!("warm")),
        ]
    };
    let instant = [
        ("engine", json!("instant")),
        ("language", json!("en")),
        ("start", json!("warm")),
    ];
    let accurate = [
        ("engine", json!("accurate")),
        ("language", json!("en")),
        ("start", json!("warm")),
    ];
    let mut keys = Vec::new();
    keys.extend(key(
        file,
        "instant.firstText",
        "Instant: first live text",
        "dictation",
        Some("vosk-model-small-en-us-0.15"),
        &instant,
        "firstText",
    ));
    keys.extend(key(
        file,
        "accurate.endToText",
        "Accurate base.en: end of speech to text",
        "dictation",
        Some("base.en"),
        &accurate,
        "endToText",
    ));
    let models: Vec<String> = per_model(file, "formatting", &warm_en("balanced"), "latency")
        .into_iter()
        .map(|(model, _)| model)
        .collect();
    for model in models {
        keys.extend(key(
            file,
            &format!("formatting.{model}.latency"),
            &format!("{model}: Balanced, 1 pass"),
            "formatting",
            Some(&model),
            &warm_en("balanced"),
            "latency",
        ));
    }
    keys.extend(key(
        file,
        "ocr.fullhd",
        "OCR: full-HD screenshot",
        "investigations",
        None,
        &[
            ("task", json!("ocr")),
            ("size", json!("full-hd")),
            ("language", json!("en")),
            ("start", json!("warm")),
        ],
        "ocrTime",
    ));
    keys.extend(key(
        file,
        "messaging.tor.rtt",
        "Tor: message round trip",
        "messaging",
        None,
        &[("transport", json!("tor")), ("start", json!("cold"))],
        "rtt",
    ));
    keys.extend(key(
        file,
        "app.startup",
        "App: warm startup",
        "app",
        None,
        &[("task", json!("startup")), ("start", json!("warm"))],
        "startup",
    ));
    keys
}

fn nudges(file: &BenchmarkFile) -> Vec<Nudge> {
    let mut out = Vec::new();

    // Dictation.
    let warm_en = [("language", json!("en")), ("start", json!("warm"))];
    let first = per_model(file, "dictation", &warm_en, "firstText");
    let wer = per_model(file, "dictation", &warm_en, "wer");
    if let Some((model, seconds)) = lowest(&first) {
        out.push(nudge(
            "dictation",
            "Fastest",
            label(model),
            format!("Live text after about {seconds:.1} s."),
        ));
    }
    if let Some((model, rate)) = lowest(&wer) {
        let ties = tied(&wer, *rate, 1.0);
        if ties.len() > 1 {
            out.push(nudge(
                "dictation",
                "Most accurate",
                "A tie",
                format!(
                    "{} engines got clear English speech about {rate:.0}% wrong; pick by speed.",
                    ties.len()
                ),
            ));
        } else {
            out.push(nudge(
                "dictation",
                "Most accurate",
                label(model),
                format!("About {rate:.0}% word errors on clear English speech."),
            ));
        }
    }
    let wer_pt = per_model(
        file,
        "dictation",
        &[("language", json!("pt")), ("start", json!("warm"))],
        "wer",
    );
    if let Some((model, rate)) = lowest(&wer_pt) {
        out.push(nudge(
            "dictation",
            "Portuguese",
            label(model),
            format!("Instant is English-only; this got about {rate:.0}% word errors."),
        ));
    }

    // Formatting: one pass, warm, both languages.
    let one_pass = [("passes", json!(1)), ("start", json!("warm"))];
    let latency = per_model(file, "formatting", &one_pass, "latency");
    let quality = per_model(file, "formatting", &one_pass, "quality");
    let pass_rate = per_model(file, "formatting", &one_pass, "passRate");
    if let Some((model, seconds)) = lowest(&latency) {
        out.push(nudge(
            "formatting",
            "Fastest",
            model.clone(),
            format!("About {seconds:.1} s per dictation."),
        ));
    }
    if let Some((model, score)) = highest(&quality) {
        let ties = tied(&quality, *score, 3.0);
        if ties.len() > 1 {
            out.push(nudge(
                "formatting",
                "Best quality",
                "Close call",
                format!(
                    "{} scored within 3 points of each other (about {score:.0}/100).",
                    ties.join(", ")
                ),
            ));
        } else {
            out.push(nudge(
                "formatting",
                "Best quality",
                model.clone(),
                format!("Kept facts and structure best ({score:.0}/100)."),
            ));
        }
    }
    // Structured and Custom ask for lists; small models often keep prose.
    let lists: Vec<(String, f64)> = ["structured", "custom"]
        .iter()
        .flat_map(|mode| {
            per_model(
                file,
                "formatting",
                &[
                    ("mode", json!(mode)),
                    ("passes", json!(1)),
                    ("start", json!("warm")),
                ],
                "listItems",
            )
        })
        .fold(
            Vec::<(String, Vec<f64>)>::new(),
            |mut acc, (model, items)| {
                match acc.iter_mut().find(|(m, _)| *m == model) {
                    Some((_, values)) => values.push(items),
                    None => acc.push((model, vec![items])),
                }
                acc
            },
        )
        .into_iter()
        .map(|(model, values)| (model, mean(&values)))
        .collect();
    if let (Some((model, items)), Some((_, fewest))) = (highest(&lists), lowest(&lists)) {
        out.push(nudge("formatting", "Best at lists", model.clone(), format!("About {items:.1} list lines per dictation in Structured and Custom, against {fewest:.1} for the least list-happy model.")));
    }
    // Good default: feels instant, rarely falls back, and lists when asked;
    // among those, the one with the best quality.
    let best_lists = highest(&lists).map(|(_, items)| *items).unwrap_or_default();
    let good = quality
        .iter()
        .filter(|(model, _)| {
            latency
                .iter()
                .any(|(m, s)| m == model && *s <= INTERACTIVE_SECONDS)
                && pass_rate.iter().any(|(m, r)| m == model && *r >= 90.0)
                && lists
                    .iter()
                    .any(|(m, items)| m == model && *items >= best_lists * 0.6)
        })
        .max_by(|left, right| left.1.total_cmp(&right.1));
    if let Some((model, score)) = good {
        let seconds = latency
            .iter()
            .find(|(m, _)| m == model)
            .map(|(_, s)| *s)
            .unwrap_or_default();
        out.push(nudge("formatting", "Good default", model.clone(), format!("Feels instant ({seconds:.1} s), scores {score:.0}/100 and turns spoken lists into lists.")));
    }
    let two_pass: Vec<(String, f64)> = per_model(
        file,
        "formatting",
        &[("passes", json!(2)), ("start", json!("warm"))],
        "laterPassChange",
    );
    let two_latency = per_model(
        file,
        "formatting",
        &[("passes", json!(2)), ("start", json!("warm"))],
        "latency",
    );
    if !two_pass.is_empty() && !latency.is_empty() && !two_latency.is_empty() {
        let change = mean(&two_pass.iter().map(|(_, c)| *c).collect::<Vec<_>>());
        let slower = mean(&two_latency.iter().map(|(_, s)| *s).collect::<Vec<_>>())
            / mean(&latency.iter().map(|(_, s)| *s).collect::<Vec<_>>()).max(1e-6);
        let pick = if change < 10.0 {
            "Usually skip it"
        } else {
            "Worth it for messy dictation"
        };
        out.push(nudge(
            "formatting",
            "Second pass",
            pick,
            format!("It changed about {change:.0}% of words and took {slower:.1}× as long."),
        ));
    }

    // Meetings.
    let summary = per_model(
        file,
        "meetings",
        &[("task", json!("summary")), ("start", json!("warm"))],
        "answer",
    );
    if let Some((model, seconds)) = lowest(&summary) {
        out.push(nudge(
            "meetings",
            "Fastest summary",
            model.clone(),
            format!("A one-minute meeting summarized in about {seconds:.0} s."),
        ));
    }
    let finalize = per_model(
        file,
        "meetings",
        &[("task", json!("transcribe")), ("language", json!("en"))],
        "finalize",
    );
    if let Some((model, seconds)) = lowest(&finalize) {
        out.push(nudge(
            "meetings",
            "Quickest stop",
            label(model),
            format!("Transcript saved about {seconds:.1} s after Stop."),
        ));
    }

    // Investigations.
    if let Some(run) = find(
        file,
        "investigations",
        None,
        &[
            ("task", json!("ocr")),
            ("size", json!("full-hd")),
            ("language", json!("en")),
            ("start", json!("warm")),
        ],
    ) {
        let seconds = value(run, "ocrTime").unwrap_or_default();
        let cer = value(run, "cer").unwrap_or_default();
        out.push(nudge("investigations", "Screenshots", "On-device OCR", format!("A full-HD screenshot reads in about {seconds:.1} s with {cer:.0}% character errors.")));
    }
    let answers = per_model(
        file,
        "investigations",
        &[("task", json!("answer")), ("start", json!("warm"))],
        "answer",
    );
    if let Some((model, seconds)) = lowest(&answers) {
        out.push(nudge(
            "investigations",
            "Fastest answers",
            model.clone(),
            format!("About {seconds:.0} s per answer."),
        ));
    }

    // Messaging.
    if let Some(run) = find(
        file,
        "messaging",
        None,
        &[("transport", json!("tor")), ("start", json!("cold"))],
    ) {
        let setup =
            value(run, "bootstrap").unwrap_or_default() + value(run, "redeem").unwrap_or_default();
        let rtt = value(run, "rtt").unwrap_or_default();
        out.push(nudge("messaging", "Expect", "A slow first contact", format!("About {:.0} s to connect and add a contact over Tor, then about {rtt:.1} s per message.", setup)));
    }

    // App.
    if let Some(run) = find(
        file,
        "app",
        None,
        &[("task", json!("startup")), ("start", json!("warm"))],
    ) {
        let seconds = value(run, "startup").unwrap_or_default();
        out.push(nudge(
            "app",
            "Startup",
            "Quick",
            format!("The window is ready about {seconds:.1} s after launch."),
        ));
    }
    out
}
