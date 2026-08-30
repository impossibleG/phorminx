use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, ensure};
use clap::{Parser, Subcommand};
use phorminx_audio::{
    WHISPER_SAMPLE_RATE, input_devices, read_wav, record_default, start_default, write_wav,
};
use phorminx_core::{SpeechRecognizer, TranscriptionOptions};
use phorminx_whisper::WhisperRecognizer;

#[derive(Debug, Parser)]
#[command(name = "phorminx-bench")]
#[command(about = "Phase 0 microphone and local Whisper benchmark")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// List microphone devices visible to CPAL/WASAPI.
    Devices,
    /// Record the default microphone and write 16 kHz mono WAV.
    Record {
        #[arg(long, default_value_t = 5)]
        seconds: u64,
        #[arg(long, default_value = "test-data/latest.wav")]
        output: PathBuf,
    },
    /// Transcribe an existing WAV file.
    Transcribe {
        #[arg(long)]
        model: PathBuf,
        #[arg(long)]
        input: PathBuf,
        #[arg(long, default_value = "en")]
        language: String,
        #[arg(long)]
        threads: Option<usize>,
        /// Experimental Whisper encoder context override (model default: 1500).
        #[arg(long)]
        audio_context: Option<u32>,
    },
    /// Record the default microphone and immediately transcribe it.
    CaptureTranscribe {
        #[arg(long)]
        model: PathBuf,
        #[arg(long, default_value_t = 8)]
        seconds: u64,
        #[arg(long, default_value = "en")]
        language: String,
        #[arg(long)]
        threads: Option<usize>,
        /// Experimental Whisper encoder context override (model default: 1500).
        #[arg(long)]
        audio_context: Option<u32>,
        #[arg(long)]
        save_wav: Option<PathBuf>,
    },
    /// Repeatedly exercise real default-microphone capture without retaining audio.
    #[command(hide = true)]
    CaptureSoak {
        #[arg(long, default_value_t = 500)]
        iterations: u32,
        #[arg(long, default_value_t = 100)]
        hold_ms: u64,
        #[arg(long, default_value_t = 25)]
        pause_ms: u64,
        #[arg(long, default_value_t = 25)]
        cancel_iterations: u32,
    },
}

fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Devices => {
            for device in input_devices().context("failed to list microphones")? {
                let marker = if device.is_default { "*" } else { " " };
                println!("{marker} {}", device.name);
            }
        }
        Command::Record { seconds, output } => {
            println!("Recording the default microphone for {seconds} seconds...");
            let clip = record_default(Duration::from_secs(seconds))?;
            ensure_parent(&output)?;
            write_wav(&output, &clip)?;
            print_audio_metrics(&clip);
            println!("Wrote {}", output.display());
        }
        Command::Transcribe {
            model,
            input,
            language,
            threads,
            audio_context,
        } => {
            let clip =
                read_wav(&input).with_context(|| format!("failed to read {}", input.display()))?;
            transcribe(&model, &clip, &language, threads, audio_context)?;
        }
        Command::CaptureTranscribe {
            model,
            seconds,
            language,
            threads,
            audio_context,
            save_wav,
        } => {
            println!("Recording the default microphone for {seconds} seconds...");
            let clip = record_default(Duration::from_secs(seconds))?;
            print_audio_metrics(&clip);

            if let Some(path) = save_wav {
                ensure_parent(&path)?;
                write_wav(&path, &clip)?;
                println!("Wrote {}", path.display());
            }

            transcribe(&model, &clip, &language, threads, audio_context)?;
        }
        Command::CaptureSoak {
            iterations,
            hold_ms,
            pause_ms,
            cancel_iterations,
        } => capture_soak(iterations, hold_ms, pause_ms, cancel_iterations)?,
    }

    Ok(())
}

fn capture_soak(
    iterations: u32,
    hold_ms: u64,
    pause_ms: u64,
    cancel_iterations: u32,
) -> Result<()> {
    ensure!(
        iterations > 0,
        "capture soak iterations must be greater than zero"
    );
    ensure!(
        hold_ms > 0,
        "capture soak hold time must be greater than zero"
    );

    let hold = Duration::from_millis(hold_ms);
    let pause = Duration::from_millis(pause_ms);
    let maximum_clip_duration = hold
        .checked_add(Duration::from_secs(1))
        .context("capture soak hold time is too large")?;
    let mut start_latencies = Vec::with_capacity(iterations as usize);
    let mut finish_latencies = Vec::with_capacity(iterations as usize);
    let mut cancel_start_latencies = Vec::with_capacity(cancel_iterations as usize);
    let mut cancel_drop_latencies = Vec::with_capacity(cancel_iterations as usize);
    let mut warning_count = 0_u64;
    let mut captured_frames = 0_u64;
    let soak_started = Instant::now();

    println!(
        "capture_soak_started iterations={iterations} hold_ms={hold_ms} pause_ms={pause_ms} cancel_iterations={cancel_iterations}"
    );

    for iteration in 1..=iterations {
        let started = Instant::now();
        let recording = start_default().with_context(|| {
            format!("capture soak iteration {iteration}/{iterations} failed to start")
        })?;
        start_latencies.push(started.elapsed());

        thread::sleep(hold);

        let finished = Instant::now();
        let captured = recording.finish_with_diagnostics().with_context(|| {
            format!("capture soak iteration {iteration}/{iterations} failed to finish")
        })?;
        finish_latencies.push(finished.elapsed());

        let clip = captured.clip;
        ensure!(
            clip.sample_rate == WHISPER_SAMPLE_RATE,
            "capture soak iteration {iteration}/{iterations} produced {} Hz audio",
            clip.sample_rate
        );
        ensure!(
            !clip.samples.is_empty(),
            "capture soak iteration {iteration}/{iterations} produced no audio"
        );
        ensure!(
            clip.samples.iter().all(|sample| sample.is_finite()),
            "capture soak iteration {iteration}/{iterations} produced non-finite audio"
        );
        ensure!(
            clip.duration() <= maximum_clip_duration,
            "capture soak iteration {iteration}/{iterations} produced an unexpectedly long clip"
        );

        warning_count = warning_count.saturating_add(captured.backend_warning_count);
        captured_frames = captured_frames.saturating_add(clip.samples.len() as u64);
        drop(clip);
        pause_between(iteration, iterations, pause);
    }

    for iteration in 1..=cancel_iterations {
        let started = Instant::now();
        let recording = start_default().with_context(|| {
            format!(
                "capture cancellation iteration {iteration}/{cancel_iterations} failed to start"
            )
        })?;
        cancel_start_latencies.push(started.elapsed());

        thread::sleep(hold);

        let dropped = Instant::now();
        drop(recording);
        cancel_drop_latencies.push(dropped.elapsed());
        pause_between(iteration, cancel_iterations, pause);
    }

    let audio_seconds = captured_frames as f64 / f64::from(WHISPER_SAMPLE_RATE);
    println!(
        "capture_soak_complete finished={iterations} cancelled={cancel_iterations} warnings={warning_count} frames={captured_frames} audio_seconds={audio_seconds:.3} elapsed_seconds={:.3}",
        soak_started.elapsed().as_secs_f64()
    );
    print_latency_summary("start", &mut start_latencies);
    print_latency_summary("finish", &mut finish_latencies);
    print_latency_summary("cancel_start", &mut cancel_start_latencies);
    print_latency_summary("cancel_drop", &mut cancel_drop_latencies);
    Ok(())
}

fn pause_between(iteration: u32, iterations: u32, pause: Duration) {
    if iteration != iterations && !pause.is_zero() {
        thread::sleep(pause);
    }
}

fn print_latency_summary(label: &str, samples: &mut [Duration]) {
    if samples.is_empty() {
        return;
    }

    samples.sort_unstable();
    println!(
        "capture_soak_latency operation={label} p50_ms={:.3} p95_ms={:.3} p99_ms={:.3} max_ms={:.3}",
        percentile(samples, 50).as_secs_f64() * 1_000.0,
        percentile(samples, 95).as_secs_f64() * 1_000.0,
        percentile(samples, 99).as_secs_f64() * 1_000.0,
        samples[samples.len() - 1].as_secs_f64() * 1_000.0,
    );
}

fn percentile(sorted: &[Duration], percentile: usize) -> Duration {
    let rank = (percentile * sorted.len()).div_ceil(100).saturating_sub(1);
    sorted[rank.min(sorted.len() - 1)]
}

fn transcribe(
    model: &Path,
    clip: &phorminx_core::AudioClip,
    language: &str,
    threads: Option<usize>,
    audio_context: Option<u32>,
) -> Result<()> {
    println!("Loading model {}", model.display());
    let recognizer = WhisperRecognizer::load(model)?;
    let options = TranscriptionOptions {
        language: Some(language),
        thread_count: threads,
        audio_context,
    };
    let result = recognizer.transcribe(clip, &options)?;

    println!("\n{}\n", result.text);
    println!("backend={}", result.backend);
    println!("audio_seconds={:.3}", result.audio_duration.as_secs_f64());
    println!(
        "model_load_ms={:.1}",
        result.model_load_time.as_secs_f64() * 1_000.0
    );
    println!(
        "inference_ms={:.1}",
        result.inference_time.as_secs_f64() * 1_000.0
    );
    println!("realtime_factor={:.3}", result.realtime_factor());
    Ok(())
}

fn print_audio_metrics(clip: &phorminx_core::AudioClip) {
    println!("sample_rate_hz={}", clip.sample_rate);
    println!("audio_seconds={:.3}", clip.duration().as_secs_f64());
    println!("peak={:.5}", clip.peak_amplitude());
    println!("rms={:.5}", clip.rms());
}

fn ensure_parent(path: &Path) -> Result<()> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    Ok(())
}
