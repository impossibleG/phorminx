//! Investigations: on-device OCR of generated screenshots (small and full
//! HD), link preview fetches of public pages, metadata parsing, and the time a
//! local model takes to answer a question about an investigation.

use std::time::Instant;

use anyhow::{Context, Result};
use phorminx_app::investigation_assistant::ANSWER_INSTRUCTIONS;
use phorminx_app::investigation_links::{fetch_preview, parse_metadata};
use phorminx_assistant::{ChatMessage, ChatRole};
use phorminx_core::benchmark_report::{Example, Measure, character_error_rate, mean};
use phorminx_ocr::OcrEngine;
use serde_json::json;
use url::Url;

use super::fixtures::SCREENSHOTS;
use super::meetings::chat_runs;
use super::{Assets, FeatureResult, run};

pub const FEATURE: &str = "investigations";

/// Stable public pages, fetched read-only as the link card does.
const PREVIEW_PAGES: &[(&str, &str)] = &[
    ("example.com", "https://example.com/"),
    ("rust-lang.org", "https://www.rust-lang.org/"),
    ("wikipedia.org", "https://en.wikipedia.org/wiki/Phorminx"),
];

const SAVED_PAGE: &str = r#"<!doctype html><html lang="en"><head><meta charset="utf-8">
<title>Quarterly operations report | Example Co</title>
<meta name="description" content="Revenue grew 14% to $2.8 million while support tickets fell.">
<meta property="og:title" content="Quarterly operations report">
<meta property="og:site_name" content="Example Co">
<meta property="og:image" content="/images/report-cover.png">
<link rel="icon" href="/favicon.ico"></head><body><main><h1>Report</h1><p>Body text.</p></main></body></html>"#;

/// An investigation as the assistant sees it: outline plus relevant cards.
const CONTEXT: &str = "Investigation: Hosting decision\n\nOutline of cards (oldest first):\n\
- [#1] Screenshot: Quarterly operations report\n\
- [#2] Link: Hosting offer from Northwind Cloud\n\
- [#3] Note: Call with the finance team\n\
- [#4] Screenshot: Uptime dashboard, last 90 days\n\
\nConnections drawn by the user:\n- [#2] Link: Hosting offer from Northwind Cloud → [#3] Note: Call with the finance team\n\
\nCard contents most relevant to the question:\n\
\n<card id=\"1\" kind=\"screenshot\">\nRevenue grew 14% to $2.8 million. Next steps: renew the cloud contract and publish the security audit before March 31.\n</card>\n\
\n<card id=\"2\" kind=\"link\">\nNorthwind Cloud: 3-year contract, $41,000 per year, 99.95% uptime commitment, data stays in the EU, migration support included.\n</card>\n\
\n<card id=\"3\" kind=\"note\">\nFinance can approve up to $45,000 per year without a board vote. They prefer contracts of two years or less.\n</card>\n\
\n<card id=\"4\" kind=\"screenshot\">\nCurrent provider uptime: 99.71% over 90 days, two incidents longer than an hour.\n</card>\n";

const QUESTION: &str = "Should we switch to Northwind Cloud? What would block it?";

pub fn measure(assets: &Assets, models: &[String], repeats: u32) -> Result<FeatureResult> {
    let mut result = FeatureResult {
        note: "OCR runs on the CPU with the app's pinned PaddleOCR models on generated screenshots. Link previews fetch public pages over the internet, so they vary with the network. Answers use the investigation assistant's request shape.".into(),
        ..Default::default()
    };
    let repeats = repeats.max(1) as usize;

    // OCR.
    let started = Instant::now();
    let engine = OcrEngine::load(
        &assets.ocr("det.onnx"),
        &assets.ocr("rec-latin.onnx"),
        &assets.ocr("latin-dict.txt"),
    )
    .context("failed to load the OCR models")?;
    let load = started.elapsed().as_secs_f64();
    let mut first = true;
    for shot in SCREENSHOTS {
        let path = shot.png(assets);
        let bytes = std::fs::read(&path).with_context(|| {
            format!(
                "failed to read {} (run `suite fixtures` first)",
                path.display()
            )
        })?;
        let mut times = Vec::new();
        let mut text = String::new();
        for _ in 0..repeats + 1 {
            let started = Instant::now();
            let lines = engine.recognize_encoded(&bytes).context("OCR failed")?;
            times.push(started.elapsed().as_secs_f64());
            text = lines
                .iter()
                .map(|line| line.text.as_str())
                .collect::<Vec<_>>()
                .join("\n");
        }
        let cold = times.remove(0);
        let cer = character_error_rate(&shot.reference(), &text);
        let config = |start: &str| {
            vec![
                ("task", json!("ocr")),
                ("size", json!(shot.size)),
                (
                    "resolution",
                    json!(format!("{}x{}", shot.width, shot.height)),
                ),
                ("language", json!(shot.language)),
                ("start", json!(start)),
            ]
        };
        let warm = run(
            FEATURE,
            Some("PaddleOCR latin v1"),
            "cpu",
            &config("warm"),
            times.len(),
            vec![
                ("ocrTime", Measure::of(&times, "s")),
                ("cer", Measure::new(cer, "%")),
            ],
        );
        println!(
            "ocr {} {}: {:.3}s CER {:.1}%",
            shot.size, shot.language, warm.metrics["ocrTime"].value, cer
        );
        result.examples.push(Example {
            feature: FEATURE.to_owned(),
            run_id: warm.id.clone(),
            sample: shot.id.to_owned(),
            language: shot.language.to_owned(),
            input: shot.reference(),
            output: text.clone(),
        });
        result.runs.push(warm);
        // The first image after loading pays for the load: that is the cold start.
        let mut metrics = vec![
            ("ocrTime", Measure::new(cold, "s")),
            ("cer", Measure::new(cer, "%")),
        ];
        if first {
            metrics.push(("loadTime", Measure::new(load, "s")));
            metrics.push(("coldStart", Measure::new(load + cold, "s")));
            first = false;
        }
        result.runs.push(run(
            FEATURE,
            Some("PaddleOCR latin v1"),
            "cpu",
            &config("cold"),
            1,
            metrics,
        ));
    }

    // Link previews.
    let images =
        std::env::temp_dir().join(format!("phorminx-preview-bench-{}", std::process::id()));
    std::fs::create_dir_all(&images)?;
    for (site, address) in PREVIEW_PAGES {
        let url = Url::parse(address)?;
        let mut times = Vec::new();
        let mut found = 0;
        for _ in 0..repeats {
            let started = Instant::now();
            if fetch_preview(&url, &images).is_some_and(|preview| preview.title.is_some()) {
                found += 1;
            }
            times.push(started.elapsed().as_secs_f64());
        }
        if found == 0 {
            result.skip(
                FEATURE,
                &format!("Link preview of {site}"),
                "the page could not be fetched from this network",
            );
            continue;
        }
        let built = run(
            FEATURE,
            None,
            "network",
            &[("task", json!("link-preview")), ("site", json!(site))],
            times.len(),
            vec![("fetch", Measure::of(&times, "s"))],
        );
        println!("preview {site}: {:.2}s", built.metrics["fetch"].value);
        result.runs.push(built);
    }
    let _ = std::fs::remove_dir_all(&images);
    let base = Url::parse("https://example.com/report")?;
    let mut parse = Vec::new();
    for _ in 0..200 {
        let started = Instant::now();
        std::hint::black_box(parse_metadata(std::hint::black_box(SAVED_PAGE), &base));
        parse.push(started.elapsed().as_secs_f64() * 1000.0);
    }
    result.runs.push(run(
        FEATURE,
        None,
        "cpu",
        &[("task", json!("link-parse"))],
        parse.len(),
        vec![("parse", Measure::new(mean(&parse), "ms"))],
    ));

    // Assistant answers.
    let messages = [
        ChatMessage {
            role: ChatRole::System,
            content: ANSWER_INSTRUCTIONS.into(),
        },
        ChatMessage {
            role: ChatRole::User,
            content: format!("{CONTEXT}\n\nQuestion: {QUESTION}"),
        },
    ];
    for model in models {
        chat_runs(
            FEATURE,
            model,
            "answer",
            "en",
            &messages,
            repeats,
            &mut result,
            "hosting-decision",
            QUESTION,
        )?;
    }
    Ok(result)
}
