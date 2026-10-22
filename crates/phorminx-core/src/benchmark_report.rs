//! Published benchmark results: `docs/benchmarks/benchmarks.json`, written by
//! `phorminx-bench suite` and displayed, read-only, by the app's Benchmarks
//! page and the website.
//!
//! Results come from synthetic inputs only: generated speech, generated
//! screenshots and written sample dictations. They describe no machine: no
//! hardware, host or user names, addresses or file paths.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Bump when a field changes meaning; readers skip newer schemas.
pub const BENCHMARK_SCHEMA: u32 = 1;

/// Feature ids and titles, in display order.
pub const FEATURES: &[(&str, &str)] = &[
    ("dictation", "Dictation"),
    ("formatting", "Formatting"),
    ("meetings", "Meetings"),
    ("investigations", "Investigations"),
    ("messaging", "Messaging"),
    ("app", "App"),
];

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BenchmarkFile {
    pub schema_version: u32,
    pub id: String,
    pub title: String,
    /// `YYYY-MM-DD`.
    pub date: String,
    pub app_version: String,
    pub app_commit: String,
    pub verdict: Option<Verdict>,
    /// Findings in Markdown; displayed as text, never as HTML.
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub features: Vec<FeatureStatus>,
    #[serde(default)]
    pub metrics: BTreeMap<String, MetricInfo>,
    #[serde(default)]
    pub runs: Vec<Run>,
    #[serde(default)]
    pub examples: Vec<Example>,
    #[serde(default)]
    pub skipped: Vec<Skipped>,
    #[serde(default)]
    pub key_metrics: Vec<KeyMetric>,
    #[serde(default)]
    pub history: Vec<HistoryEntry>,
}

/// Plain nudges per feature, not a ranking: "Fastest", "Best quality",
/// "Good default".
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Verdict {
    pub headline: String,
    pub nudges: Vec<Nudge>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Nudge {
    pub feature: String,
    /// `Fastest`, `Best quality`, `Good default`, ...
    pub kind: String,
    pub pick: String,
    pub why: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FeatureStatus {
    pub id: String,
    pub title: String,
    /// `measured`, `partial` or `skipped`.
    pub status: String,
    #[serde(default)]
    pub note: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MetricInfo {
    pub label: String,
    pub unit: String,
    /// `lower` or `higher`.
    pub better: String,
    #[serde(default)]
    pub description: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Run {
    pub id: String,
    pub feature: String,
    pub model: Option<String>,
    pub backend: String,
    /// Free-form setup, e.g. `mode`, `passes`, `language`, `start`.
    #[serde(default)]
    pub config: BTreeMap<String, Value>,
    pub repeats: u32,
    pub metrics: BTreeMap<String, Measure>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Measure {
    pub value: f64,
    pub unit: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub p90: Option<f64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Example {
    pub feature: String,
    pub run_id: String,
    pub sample: String,
    pub language: String,
    pub input: String,
    pub output: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Skipped {
    pub feature: String,
    pub what: String,
    pub reason: String,
}

/// A headline number tracked across reports for the trend chart.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyMetric {
    pub key: String,
    pub label: String,
    pub unit: String,
    pub better: String,
    pub run_id: String,
    pub metric: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryEntry {
    pub id: String,
    pub date: String,
    pub app_version: String,
    /// Path relative to `docs/benchmarks/`, always `reports/<id>.json`.
    pub file: String,
    pub key_metrics: BTreeMap<String, f64>,
}

impl BenchmarkFile {
    pub fn run(&self, id: &str) -> Option<&Run> {
        self.runs.iter().find(|run| run.id == id)
    }

    /// The current value of every key metric that has a run.
    pub fn key_values(&self) -> BTreeMap<String, f64> {
        self.key_metrics
            .iter()
            .filter_map(|key| {
                let value = self.run(&key.run_id)?.metrics.get(&key.metric)?.value;
                Some((key.key.clone(), value))
            })
            .collect()
    }

    /// Replaces every run, example and skip note of `feature`.
    pub fn replace_feature(
        &mut self,
        feature: &str,
        runs: Vec<Run>,
        examples: Vec<Example>,
        skipped: Vec<Skipped>,
    ) {
        self.runs.retain(|run| run.feature != feature);
        self.runs.extend(runs);
        self.examples.retain(|example| example.feature != feature);
        self.examples.extend(examples);
        self.skipped.retain(|skip| skip.feature != feature);
        self.skipped.extend(skipped);
    }
}

impl Measure {
    pub fn new(value: f64, unit: &str) -> Self {
        Self {
            value: round(value),
            unit: unit.to_owned(),
            p90: None,
        }
    }

    /// Median of `values`, with the 90th percentile when there are several.
    pub fn of(values: &[f64], unit: &str) -> Self {
        let mut sorted = values.to_vec();
        sorted.sort_by(f64::total_cmp);
        Self {
            value: round(percentile(&sorted, 0.5)),
            unit: unit.to_owned(),
            p90: (sorted.len() > 1).then(|| round(percentile(&sorted, 0.9))),
        }
    }
}

/// Three decimals are plenty for timings and rates; one above 100.
pub fn round(value: f64) -> f64 {
    if value.abs() >= 100.0 {
        (value * 10.0).round() / 10.0
    } else {
        (value * 1000.0).round() / 1000.0
    }
}

/// Nearest-rank percentile of already sorted values.
pub fn percentile(sorted: &[f64], fraction: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let rank = (fraction * sorted.len() as f64).ceil() as usize;
    sorted[rank.clamp(1, sorted.len()) - 1]
}

pub fn mean(values: &[f64]) -> f64 {
    if values.is_empty() {
        0.0
    } else {
        values.iter().sum::<f64>() / values.len() as f64
    }
}

/// Edit distance between two token sequences.
pub fn edit_distance<T: PartialEq>(left: &[T], right: &[T]) -> usize {
    let mut previous: Vec<usize> = (0..=right.len()).collect();
    for (row, item) in left.iter().enumerate() {
        let mut current = vec![row + 1];
        for (column, other) in right.iter().enumerate() {
            current.push(
                (previous[column] + usize::from(item != other))
                    .min(previous[column + 1] + 1)
                    .min(current[column] + 1),
            );
        }
        previous = current;
    }
    previous[right.len()]
}

/// Lowercase words without punctuation, for error rates.
pub fn normalized_words(text: &str) -> Vec<String> {
    text.chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect::<String>()
        .to_lowercase()
        .split_whitespace()
        .map(str::to_owned)
        .collect()
}

/// Word error rate in percent: word edits divided by reference words.
pub fn word_error_rate(reference: &str, hypothesis: &str) -> f64 {
    let reference = normalized_words(reference);
    if reference.is_empty() {
        return 0.0;
    }
    100.0 * edit_distance(&reference, &normalized_words(hypothesis)) as f64 / reference.len() as f64
}

/// Character error rate in percent, ignoring case and runs of whitespace.
pub fn character_error_rate(reference: &str, hypothesis: &str) -> f64 {
    let clean = |text: &str| -> Vec<char> {
        text.split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase()
            .chars()
            .collect()
    };
    let reference = clean(reference);
    if reference.is_empty() {
        return 0.0;
    }
    100.0 * edit_distance(&reference, &clean(hypothesis)) as f64 / reference.len() as f64
}

/// Share of words a later pass changed, in percent of the longer text.
pub fn word_change_share(before: &str, after: &str) -> f64 {
    let left: Vec<&str> = before.split_whitespace().collect();
    let right: Vec<&str> = after.split_whitespace().collect();
    let longest = left.len().max(right.len());
    if longest == 0 {
        return 0.0;
    }
    100.0 * edit_distance(&left, &right) as f64 / longest as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(id: &str, feature: &str, value: f64) -> Run {
        Run {
            id: id.into(),
            feature: feature.into(),
            model: None,
            backend: "cpu".into(),
            config: BTreeMap::new(),
            repeats: 1,
            metrics: BTreeMap::from([("latency".to_owned(), Measure::new(value, "s"))]),
        }
    }

    #[test]
    fn percentiles_and_measures() {
        assert_eq!(percentile(&[], 0.5), 0.0);
        assert_eq!(percentile(&[4.0], 0.9), 4.0);
        assert_eq!(percentile(&[1.0, 2.0, 3.0, 4.0], 0.5), 2.0);
        let measure = Measure::of(&[3.0, 1.0, 2.0], "s");
        assert_eq!((measure.value, measure.p90), (2.0, Some(3.0)));
        assert_eq!(Measure::of(&[1.23456], "s").p90, None);
        assert_eq!(round(1.23456), 1.235);
        assert_eq!(round(1234.56), 1234.6);
    }

    #[test]
    fn error_rates_ignore_case_and_punctuation() {
        assert_eq!(word_error_rate("The cat, sat.", "the cat sat"), 0.0);
        assert_eq!(word_error_rate("the cat sat down", "the cat sat"), 25.0);
        assert_eq!(word_error_rate("a b", "a b c d"), 100.0);
        assert_eq!(word_error_rate("", "x"), 0.0);
        assert_eq!(word_error_rate("Então às 10", "então às 10"), 0.0);
        assert_eq!(character_error_rate("Hello  World", "hello world"), 0.0);
        assert_eq!(character_error_rate("abcd", "abce"), 25.0);
        assert_eq!(word_change_share("a b c d", "a x c d"), 25.0);
        assert_eq!(word_change_share("", ""), 0.0);
    }

    #[test]
    fn features_are_replaced_and_key_values_resolve() {
        let mut file = BenchmarkFile {
            schema_version: BENCHMARK_SCHEMA,
            runs: vec![run("a", "formatting", 1.0), run("b", "dictation", 2.0)],
            key_metrics: vec![KeyMetric {
                key: "fmt".into(),
                label: "Formatting".into(),
                unit: "s".into(),
                better: "lower".into(),
                run_id: "c".into(),
                metric: "latency".into(),
            }],
            ..Default::default()
        };
        file.replace_feature(
            "formatting",
            vec![run("c", "formatting", 3.0)],
            vec![],
            vec![],
        );
        let ids: Vec<&str> = file.runs.iter().map(|run| run.id.as_str()).collect();
        assert_eq!(ids, ["b", "c"]);
        assert_eq!(file.key_values(), BTreeMap::from([("fmt".to_owned(), 3.0)]));
    }

    #[test]
    fn files_round_trip_with_camel_case_fields() {
        let file = BenchmarkFile {
            schema_version: BENCHMARK_SCHEMA,
            id: "2026-10-19".into(),
            app_version: "0.2.3".into(),
            runs: vec![run("a", "app", 1.0)],
            ..Default::default()
        };
        let text = serde_json::to_string(&file).unwrap();
        assert!(text.contains("\"schemaVersion\":1") && text.contains("\"appVersion\""));
        assert_eq!(serde_json::from_str::<BenchmarkFile>(&text).unwrap(), file);
    }
}
