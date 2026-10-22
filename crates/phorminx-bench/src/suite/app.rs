//! App: startup to window (first run of a new profile, then warm restarts),
//! settings saves and memory at idle. Every launch uses a throwaway profile
//! with its own settings, data and WebView2 folders, and `LOCALAPPDATA`
//! pointed inside it, so an installed Phorminx and its data are never used.

use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use phorminx_app::settings::{RecognitionMode, Settings, SettingsStore};
use phorminx_core::benchmark_report::{Measure, mean};
use serde_json::json;

use super::{Assets, FeatureResult, powershell, run};

pub const FEATURE: &str = "app";

struct Profile {
    root: PathBuf,
}

impl Profile {
    fn new(assets: &Assets, name: &str) -> Result<Self> {
        let root =
            std::env::temp_dir().join(format!("phorminx-app-bench-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("LocalAppData"))?;
        let mut settings = Settings::default();
        settings.recognition.mode = RecognitionMode::Instant;
        settings.recognition.language = "en".into();
        settings.recognition.instant_runtime_path = assets.vosk_runtime();
        settings.recognition.instant_model_path = assets.vosk_model();
        // Shortcuts nobody uses, so an installed copy keeps its own.
        settings.interaction.launcher_shortcut = "Ctrl+Alt+Shift+F11".into();
        settings.interaction.direct_dictation_shortcut = None;
        SettingsStore::new(root.join("settings.toml"))?.save(&settings)?;
        Ok(Self { root })
    }

    fn launch(&self, exe: &Path) -> Result<Child> {
        Command::new(exe)
            .arg("--config")
            .arg(self.root.join("settings.toml"))
            .arg("--background")
            .env("LOCALAPPDATA", self.root.join("LocalAppData"))
            .env("WEBVIEW2_USER_DATA_FOLDER", self.root.join("webview2"))
            .spawn()
            .with_context(|| format!("failed to start {}", exe.display()))
    }

    /// Milliseconds of the newest `window` startup phase after `seen` lines.
    fn window_phase(&self, seen: usize) -> Option<(f64, usize)> {
        let log = std::fs::read_to_string(self.root.join("logs/lifecycle.log")).ok()?;
        let lines: Vec<&str> = log.lines().collect();
        lines.iter().skip(seen).find_map(|line| {
            let rest = line.split("event=startup_phase phase=window ms=").nth(1)?;
            let ms: f64 = rest.split_whitespace().next()?.parse().ok()?;
            Some((ms / 1000.0, lines.len()))
        })
    }

    fn log_lines(&self) -> usize {
        std::fs::read_to_string(self.root.join("logs/lifecycle.log"))
            .map(|log| log.lines().count())
            .unwrap_or(0)
    }
}

impl Drop for Profile {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn stop(child: &mut Child) {
    let _ = Command::new("taskkill")
        .args(["/T", "/F", "/PID", &child.id().to_string()])
        .output();
    let _ = child.wait();
    std::thread::sleep(Duration::from_secs(2));
}

/// Private memory of `pid` and every process it started, in MiB.
fn tree_memory(pid: u32) -> Option<f64> {
    let script = format!(
        "$all=Get-CimInstance Win32_Process|Select-Object ProcessId,ParentProcessId;\
         $ids=@({pid});$i=0;while($i -lt $ids.Count){{$p=$ids[$i];$ids+=@($all|Where-Object {{$_.ParentProcessId -eq $p}}|ForEach-Object {{$_.ProcessId}});$i++}};\
         (Get-Process -Id $ids -ErrorAction SilentlyContinue|Measure-Object PrivateMemorySize64 -Sum).Sum"
    );
    powershell(&script)?
        .parse::<f64>()
        .ok()
        .map(|bytes| bytes / 1_048_576.0)
}

/// Launches once and waits for the window; returns seconds and idle MiB.
fn start_once(profile: &Profile, exe: &Path, memory: bool) -> Result<(f64, Option<f64>)> {
    let seen = profile.log_lines();
    let mut child = profile.launch(exe)?;
    let started = Instant::now();
    let found = loop {
        if let Some((seconds, _)) = profile.window_phase(seen) {
            break Some(seconds);
        }
        if started.elapsed() > Duration::from_secs(90) || child.try_wait()?.is_some() {
            break None;
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    let Some(seconds) = found else {
        stop(&mut child);
        bail!("the app did not report a window within 90 s (see the profile's logs/lifecycle.log)");
    };
    let idle = if memory {
        std::thread::sleep(Duration::from_secs(10));
        tree_memory(child.id())
    } else {
        None
    };
    stop(&mut child);
    Ok((seconds, idle))
}

pub fn measure(assets: &Assets, exe: &Path, repeats: u32) -> Result<FeatureResult> {
    let mut result = FeatureResult {
        note: "A release build started on throwaway profiles with Instant dictation set up. Cold is the first start of a new profile; warm restarts it.".into(),
        ..Default::default()
    };
    let repeats = repeats.max(1);
    let backend = "Windows · WebView2";

    let mut cold = Vec::new();
    let mut warm = Vec::new();
    let mut memory = Vec::new();
    for index in 0..repeats {
        let profile = Profile::new(assets, &format!("start-{index}"))?;
        cold.push(start_once(&profile, exe, false)?.0);
        let (seconds, idle) = start_once(&profile, exe, true)?;
        warm.push(seconds);
        memory.extend(idle);
        println!(
            "app start {index}: cold {:.2}s warm {seconds:.2}s idle {idle:?} MiB",
            cold[cold.len() - 1]
        );
    }
    result.runs.push(run(
        FEATURE,
        None,
        backend,
        &[("task", json!("startup")), ("start", json!("cold"))],
        cold.len(),
        vec![("startup", Measure::of(&cold, "s"))],
    ));
    let mut metrics = vec![("startup", Measure::of(&warm, "s"))];
    if !memory.is_empty() {
        metrics.push(("idleMemory", Measure::of(&memory, "MiB")));
    }
    result.runs.push(run(
        FEATURE,
        None,
        backend,
        &[("task", json!("startup")), ("start", json!("warm"))],
        warm.len(),
        metrics,
    ));

    // Settings saves, in process, as the Settings page does.
    let profile = Profile::new(assets, "settings")?;
    let store = SettingsStore::new(profile.root.join("settings.toml"))?;
    let mut settings = store.load()?;
    let mut saves = Vec::new();
    for index in 0..50 {
        settings.interaction.show_live_text = index % 2 == 0;
        let started = Instant::now();
        store.save(&settings)?;
        saves.push(started.elapsed().as_secs_f64() * 1000.0);
    }
    let built = run(
        FEATURE,
        None,
        "NTFS",
        &[("task", json!("settings-save"))],
        saves.len(),
        vec![("settingsSave", Measure::of(&saves, "ms"))],
    );
    println!(
        "settings save: {:.2} ms (mean {:.2})",
        built.metrics["settingsSave"].value,
        mean(&saves)
    );
    result.runs.push(built);
    Ok(result)
}
