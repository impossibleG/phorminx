//! The published benchmark suite: measures every feature on this machine with
//! synthetic inputs and merges the results into `docs/benchmarks/benchmarks.json`
//! (schema in `phorminx_core::benchmark_report`). Each feature can be rerun on
//! its own; `finalize` adds the verdict, archives the report and rebuilds the
//! history the trend chart reads.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use phorminx_core::benchmark_report::{
    BENCHMARK_SCHEMA, BenchmarkFile, Example, FEATURES, FeatureStatus, HistoryEntry, Measure,
    MetricInfo, Run, Skipped,
};
use serde_json::Value;

pub mod app;
pub mod dictation;
pub mod fixtures;
pub mod formatting;
pub mod investigations;
pub mod meetings;
pub mod messaging;
pub mod verdict;

/// Model files and runtimes the suite reads; none are committed.
#[derive(Clone, Debug)]
pub struct Assets {
    pub root: PathBuf,
}

impl Assets {
    pub fn vosk_runtime(&self) -> PathBuf {
        self.root.join("vosk/vosk-win64-0.3.45")
    }
    pub fn vosk_model(&self) -> PathBuf {
        self.root.join("vosk/vosk-model-small-en-us-0.15")
    }
    pub fn whisper(&self, file: &str) -> PathBuf {
        self.root.join("whisper").join(file)
    }
    pub fn ocr(&self, file: &str) -> PathBuf {
        self.root.join("ocr").join(file)
    }
    /// Generated speech and screenshots.
    pub fn fixtures(&self) -> PathBuf {
        self.root.join("fixtures")
    }
}

/// What one feature measured.
#[derive(Default)]
pub struct FeatureResult {
    pub runs: Vec<Run>,
    pub examples: Vec<Example>,
    pub skipped: Vec<Skipped>,
    /// `measured` or `partial`; `skipped` when there are no runs.
    pub note: String,
}

impl FeatureResult {
    pub fn skip(&mut self, feature: &str, what: &str, reason: &str) {
        println!("skip {feature}: {what} ({reason})");
        self.skipped.push(Skipped {
            feature: feature.to_owned(),
            what: what.to_owned(),
            reason: reason.to_owned(),
        });
    }
}

/// Builds a run; `config` pairs become the run's config object.
pub fn run(
    feature: &str,
    model: Option<&str>,
    backend: &str,
    config: &[(&str, Value)],
    repeats: usize,
    metrics: Vec<(&str, Measure)>,
) -> Run {
    let mut id = vec![feature.to_owned()];
    id.extend(model.map(str::to_owned));
    id.push(backend.to_owned());
    id.extend(config.iter().map(|(key, value)| match value {
        Value::String(text) => format!("{key}={text}"),
        other => format!("{key}={other}"),
    }));
    Run {
        id: id.join("|"),
        feature: feature.to_owned(),
        model: model.map(str::to_owned),
        backend: backend.to_owned(),
        config: config
            .iter()
            .map(|(key, value)| ((*key).to_owned(), value.clone()))
            .collect(),
        repeats: repeats as u32,
        metrics: metrics
            .into_iter()
            .map(|(name, measure)| (name.to_owned(), measure))
            .collect(),
    }
}

pub fn read(path: &Path) -> Result<BenchmarkFile> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    let file: BenchmarkFile = serde_json::from_str(&text)
        .with_context(|| format!("{} is not a benchmark file", path.display()))?;
    if file.schema_version > BENCHMARK_SCHEMA {
        bail!(
            "{} uses newer schema {}",
            path.display(),
            file.schema_version
        );
    }
    Ok(file)
}

pub fn write(path: &Path, file: &BenchmarkFile) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let mut text = serde_json::to_string_pretty(file)?;
    text.push('\n');
    std::fs::write(path, text).with_context(|| format!("failed to write {}", path.display()))
}

/// Opens the current file, or starts today's report.
pub fn open_or_start(path: &Path) -> Result<BenchmarkFile> {
    let mut file = if path.exists() {
        read(path)?
    } else {
        BenchmarkFile {
            schema_version: BENCHMARK_SCHEMA,
            ..Default::default()
        }
    };
    let today = today();
    if file.date != today {
        // A new day starts a new report; history keeps the old one.
        file.id = today.clone();
        file.date = today;
        file.verdict = None;
        file.summary.clear();
    }
    file.title = "Phorminx benchmarks".into();
    file.app_version = app_version();
    file.app_commit = git(&["rev-parse", "--short", "HEAD"]).unwrap_or_default();
    file.metrics = metric_catalog();
    Ok(file)
}

/// Merges one feature's results and refreshes the feature list.
pub fn merge(path: &Path, feature: &str, result: FeatureResult) -> Result<()> {
    let mut file = open_or_start(path)?;
    let status = if result.runs.is_empty() {
        "skipped"
    } else if result.skipped.is_empty() {
        "measured"
    } else {
        "partial"
    };
    let (runs, examples) = public_backends(result.runs, result.examples);
    file.replace_feature(feature, runs, examples, result.skipped);
    file.features.retain(|known| known.id != feature);
    file.features.push(FeatureStatus {
        id: feature.to_owned(),
        title: FEATURES
            .iter()
            .find(|(id, _)| *id == feature)
            .map(|(_, title)| (*title).to_owned())
            .unwrap_or_else(|| feature.to_owned()),
        status: status.to_owned(),
        note: result.note,
    });
    file.features.sort_by_key(|status| {
        FEATURES
            .iter()
            .position(|(id, _)| *id == status.id)
            .unwrap_or(usize::MAX)
    });
    write(path, &file)?;
    println!("merged {feature} into {}", path.display());
    Ok(())
}

/// Backends as published: the kind of compute only, never a device name or
/// version (`GPU (Vulkan)`, `CPU`, `Ollama`). Run ids and the examples that
/// point at them follow.
pub fn public_backend(backend: &str) -> String {
    let lower = backend.to_ascii_lowercase();
    if lower.starts_with("vulkan") {
        "GPU (Vulkan)".into()
    } else if lower == "cpu" {
        "CPU".into()
    } else if lower == "ntfs" {
        "Disk".into()
    } else if lower.starts_with("ollama") {
        "Ollama".into()
    } else {
        backend.to_owned()
    }
}

fn public_backends(mut runs: Vec<Run>, mut examples: Vec<Example>) -> (Vec<Run>, Vec<Example>) {
    for run in &mut runs {
        let public = public_backend(&run.backend);
        if public != run.backend {
            let old_id = run.id.clone();
            run.id = run
                .id
                .replace(&format!("|{}|", run.backend), &format!("|{public}|"));
            for example in examples
                .iter_mut()
                .filter(|example| example.run_id == old_id)
            {
                example.run_id = run.id.clone();
            }
            run.backend = public;
        }
    }
    (runs, examples)
}

/// Copies `feature` from another benchmark file, e.g. one written by a run
/// on its own while other features were being measured.
pub fn import(path: &Path, from: &Path, feature: &str) -> Result<()> {
    let source = read(from)?;
    let note = source
        .features
        .iter()
        .find(|status| status.id == feature)
        .map(|status| status.note.clone())
        .unwrap_or_default();
    merge(
        path,
        feature,
        FeatureResult {
            runs: source
                .runs
                .into_iter()
                .filter(|run| run.feature == feature)
                .collect(),
            examples: source
                .examples
                .into_iter()
                .filter(|example| example.feature == feature)
                .collect(),
            skipped: source
                .skipped
                .into_iter()
                .filter(|skip| skip.feature == feature)
                .collect(),
            note,
        },
    )
}

/// Adds the verdict and key metrics, archives the report under `reports/`
/// and rebuilds `history` from every archived report.
pub fn finalize(path: &Path) -> Result<()> {
    let mut file = open_or_start(path)?;
    verdict::apply(&mut file);
    let reports = path
        .parent()
        .context("the benchmark file has no folder")?
        .join("reports");
    std::fs::create_dir_all(&reports)?;
    let mut archived = file.clone();
    archived.history.clear();
    write(&reports.join(format!("{}.json", file.id)), &archived)?;

    let mut history = Vec::new();
    for entry in std::fs::read_dir(&reports)? {
        let entry_path = entry?.path();
        if entry_path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let report = read(&entry_path)?;
        history.push(HistoryEntry {
            file: format!("reports/{}.json", report.id),
            key_metrics: report.key_values(),
            id: report.id,
            date: report.date,
            app_version: report.app_version,
        });
    }
    history.sort_by(|left, right| left.date.cmp(&right.date).then(left.id.cmp(&right.id)));
    file.history = history;
    write(path, &file)?;
    println!("finalized {}", path.display());
    Ok(())
}

fn metric(label: &str, unit: &str, better: &str, description: &str) -> MetricInfo {
    MetricInfo {
        label: label.to_owned(),
        unit: unit.to_owned(),
        better: better.to_owned(),
        description: description.to_owned(),
    }
}

/// Every metric a run may carry. Units: s, ms, %, x, MiB, count, score, tok/s.
pub fn metric_catalog() -> BTreeMap<String, MetricInfo> {
    [
        ("latency", metric("Latency", "s", "lower", "Wall time from request to final text. Median of the repeats; p90 alongside.")),
        ("passRate", metric("Validator pass rate", "%", "higher", "Share of runs whose output passed Phorminx's output validator and was inserted. A rejected output falls back to the text as spoken.")),
        ("quality", metric("Quality score", "score", "higher", "0-100. Kept facts (names, numbers, links: 50), filler words removed (20), sentence case and punctuation (15), lists where the mode asks for structure (15). Scored on the text Phorminx would insert.")),
        ("laterPassChange", metric("Second-pass change", "%", "lower", "Share of words the second pass changed after the first.")),
        ("decodeRate", metric("Decode speed", "tok/s", "higher", "Tokens per second Ollama reported for a fixed 48-token prompt right after the run. Low values mean the model did not fit in GPU memory beside everything else on the card.")),
        ("listItems", metric("List items", "count", "higher", "Mean number of list lines in the output.")),
        ("wer", metric("Word error rate", "%", "lower", "Word edits needed to reach the reference text, divided by its length; case and punctuation ignored.")),
        ("rtf", metric("Real-time factor", "x", "lower", "Compute time divided by audio length. Below 1 keeps up with speech.")),
        ("firstText", metric("Time to first live text", "s", "lower", "From the first spoken word to the first text on screen, replaying the audio at real time.")),
        ("endToText", metric("End of speech to text", "s", "lower", "From the end of the audio to the final text, replaying at real time.")),
        ("loadTime", metric("Model load", "s", "lower", "Loading the recognition model into memory.")),
        ("coldStart", metric("Cold start to first text", "s", "lower", "Model load plus the first live text, in a process that had not loaded the model.")),
        ("finalize", metric("Finalization", "s", "lower", "From Stop to a saved, final transcript.")),
        ("firstToken", metric("Time to first token", "s", "lower", "From the request to the first streamed word of the answer.")),
        ("answer", metric("Answer time", "s", "lower", "From the request to the complete answer.")),
        ("cer", metric("Character error rate", "%", "lower", "Character edits needed to reach the text drawn on the screenshot, divided by its length.")),
        ("ocrTime", metric("Recognition time", "s", "lower", "Detecting and reading every line of one screenshot.")),
        ("fetch", metric("Preview fetch", "s", "lower", "Fetching a page, its preview image and icon, and parsing the metadata.")),
        ("parse", metric("Metadata parse", "ms", "lower", "Parsing title, description and image from a saved page.")),
        ("bootstrap", metric("Tor bootstrap", "s", "lower", "From turning messaging on to being reachable.")),
        ("redeem", metric("Invite redeem", "s", "lower", "From pasting an invite to both sides seeing the contact.")),
        ("firstMessage", metric("First message", "s", "lower", "Send to delivered for the first message to a new contact.")),
        ("rtt", metric("Message round trip", "s", "lower", "Send to delivered for later messages on an open connection.")),
        ("startup", metric("Startup to window", "s", "lower", "From launch to the main window being ready.")),
        ("settingsSave", metric("Settings save", "ms", "lower", "Validating and atomically writing settings.toml.")),
        ("idleMemory", metric("Memory at idle", "MiB", "lower", "Private memory 10 s after startup, the app plus the WebView2 processes it started.")),
    ]
    .into_iter()
    .map(|(key, info)| (key.to_owned(), info))
    .collect()
}

pub fn powershell(script: &str) -> Option<String> {
    let output = Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn git(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

/// The released version this build belongs to: the newest `v*` tag.
fn app_version() -> String {
    git(&["describe", "--tags", "--abbrev=0", "--match", "v*"])
        .map(|tag| tag.trim_start_matches('v').to_owned())
        .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_owned())
}

/// Today's UTC date as `YYYY-MM-DD`.
pub fn today() -> String {
    let days = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() / 86_400)
        .unwrap_or_default() as i64;
    let (year, month, day) = civil_from_days(days);
    format!("{year:04}-{month:02}-{day:02}")
}

/// Howard Hinnant's days-to-civil conversion.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(20_743), (2026, 10, 17));
    }

    #[test]
    fn run_ids_name_every_dimension() {
        let built = run(
            "formatting",
            Some("gemma3:4b"),
            "gpu",
            &[("mode", json!("balanced")), ("passes", json!(2))],
            3,
            vec![("latency", Measure::new(1.0, "s"))],
        );
        assert_eq!(built.id, "formatting|gemma3:4b|gpu|mode=balanced|passes=2");
        assert_eq!(built.config["passes"], json!(2));
    }

    #[test]
    fn merge_and_finalize_build_history() {
        let directory = std::env::temp_dir().join(format!("phorminx-suite-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        let path = directory.join("benchmarks.json");
        let mut result = FeatureResult::default();
        result.runs.push(run(
            "app",
            None,
            "cpu",
            &[("start", json!("warm"))],
            1,
            vec![("startup", Measure::new(1.5, "s"))],
        ));
        result.skip("app", "x", "y");
        merge(&path, "app", result).unwrap();
        merge(&path, "messaging", FeatureResult::default()).unwrap();
        finalize(&path).unwrap();
        let file = read(&path).unwrap();
        let statuses: Vec<(&str, &str)> = file
            .features
            .iter()
            .map(|feature| (feature.id.as_str(), feature.status.as_str()))
            .collect();
        assert_eq!(statuses, [("messaging", "skipped"), ("app", "partial")]);
        assert_eq!(file.history.len(), 1);
        assert!(
            directory
                .join(format!("reports/{}.json", file.id))
                .is_file()
        );
        assert!(file.metrics.contains_key("wer"));
        assert_eq!(public_backend("vulkan · Some GPU"), "GPU (Vulkan)");
        assert_eq!(public_backend("Ollama 0.34 · 100% GPU"), "Ollama");
        assert_eq!(public_backend("cpu"), "CPU");
        let _ = std::fs::remove_dir_all(&directory);
    }
}
