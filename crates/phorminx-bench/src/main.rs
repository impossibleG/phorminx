use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use phorminx_audio::{input_devices, read_wav, record_default, write_wav};
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
    }

    Ok(())
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
