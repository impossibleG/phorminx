//! Published benchmark results shown on the read-only Benchmarks page.
//!
//! The app ships `docs/benchmarks/benchmarks.json` and the archived reports as
//! they were when it was built. When asked, it refreshes them from the
//! repository's main branch. That request only downloads: nothing about this
//! machine is sent, redirects are refused, and a failed or invalid download
//! leaves the bundled copy in place.

use std::io::Read;
use std::time::Duration;

use phorminx_core::benchmark_report::{BENCHMARK_SCHEMA, BenchmarkFile};
use serde::Serialize;

include!(concat!(env!("OUT_DIR"), "/bundled_benchmarks.rs"));

pub const ONLINE_BASE: &str =
    "https://raw.githubusercontent.com/impossibleG/phorminx/main/docs/benchmarks/";
const MAX_FILE_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BenchmarkCollection {
    /// `bundled` or `online`.
    pub source: &'static str,
    /// Why the bundled copy is shown, when an online refresh failed.
    pub note: Option<String>,
    pub current: Option<BenchmarkFile>,
}

/// The results compiled into this build.
pub fn bundled() -> BenchmarkCollection {
    BenchmarkCollection {
        source: "bundled",
        note: None,
        current: parse(BUNDLED_CURRENT),
    }
}

/// The latest published results, or the bundled copy with the reason.
pub fn refreshed() -> BenchmarkCollection {
    match fetch("benchmarks.json").map(|text| parse(&text)) {
        Ok(Some(current)) => BenchmarkCollection {
            source: "online",
            note: None,
            current: Some(current),
        },
        Ok(None) => BenchmarkCollection {
            note: Some(
                "The published results could not be read; showing the copy in this build.".into(),
            ),
            ..bundled()
        },
        Err(reason) => BenchmarkCollection {
            note: Some(format!("{reason}; showing the copy in this build.")),
            ..bundled()
        },
    }
}

/// An archived report by id, from GitHub when `online`, else from this build.
pub fn report(id: &str, online: bool) -> Result<BenchmarkFile, String> {
    let file = format!("{id}.json");
    if !safe_report_name(&file) {
        return Err("That report name is not valid.".into());
    }
    if online
        && let Some(report) = fetch(&format!("reports/{file}"))
            .ok()
            .and_then(|text| parse(&text))
    {
        return Ok(report);
    }
    BUNDLED_REPORTS
        .iter()
        .find(|(name, _)| *name == file)
        .and_then(|(_, text)| parse(text))
        .ok_or_else(|| "That report is not available.".into())
}

fn fetch(path: &str) -> Result<String, String> {
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(10)))
        .max_redirects(0)
        .https_only(true)
        .build()
        .new_agent();
    let mut response = agent
        .get(format!("{ONLINE_BASE}{path}"))
        .call()
        .map_err(|_| "Could not reach GitHub".to_owned())?;
    let mut text = String::new();
    response
        .body_mut()
        .as_reader()
        .take(MAX_FILE_BYTES + 1)
        .read_to_string(&mut text)
        .map_err(|_| "The download was interrupted".to_owned())?;
    if text.len() as u64 > MAX_FILE_BYTES {
        return Err("The published file is too large".into());
    }
    Ok(text)
}

/// A benchmark file this build understands; newer schemas are skipped.
fn parse(text: &str) -> Option<BenchmarkFile> {
    serde_json::from_str::<BenchmarkFile>(text)
        .ok()
        .filter(|file| (1..=BENCHMARK_SCHEMA).contains(&file.schema_version))
}

/// Report files are plain `name.json` inside `reports/`, never a path.
fn safe_report_name(file: &str) -> bool {
    file.strip_suffix(".json").is_some_and(|stem| {
        !stem.is_empty()
            && stem.len() <= 64
            && stem
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL: &str = r#"{"schemaVersion":SCHEMA,"id":"a","title":"","date":"2026-10-19","appVersion":"0","appCommit":"","hardware":{"os":"","cpu":"","cpuThreads":0,"gpu":null,"ramGb":0},"verdict":null}"#;

    #[test]
    fn parse_accepts_known_schemas_only() {
        assert!(parse(&MINIMAL.replace("SCHEMA", "1")).is_some());
        assert!(parse(&MINIMAL.replace("SCHEMA", &(BENCHMARK_SCHEMA + 1).to_string())).is_none());
        assert!(parse(&MINIMAL.replace("SCHEMA", "0")).is_none());
        assert!(parse("{").is_none());
        assert!(parse("").is_none());
    }

    #[test]
    fn report_names_are_restricted() {
        assert!(safe_report_name("2026-10-19.json"));
        for bad in [
            "",
            ".json",
            "a.txt",
            "A.json",
            "a/b.json",
            r"..\a.json",
            "a.json.exe",
        ] {
            assert!(!safe_report_name(bad), "{bad}");
        }
        assert!(report("../secrets", false).is_err());
    }

    #[test]
    fn the_bundled_results_parse() {
        if !BUNDLED_CURRENT.is_empty() {
            let current = bundled()
                .current
                .expect("the bundled benchmark file parses");
            assert!(!current.runs.is_empty());
            for entry in &current.history {
                let id = entry
                    .file
                    .trim_start_matches("reports/")
                    .trim_end_matches(".json");
                assert!(report(id, false).is_ok(), "{id} is bundled");
            }
        }
        for (name, text) in BUNDLED_REPORTS {
            assert!(parse(text).is_some(), "{name}");
        }
    }
}
