//! Dictation: Instant (Vosk) and Accurate (Whisper, each model size, CPU and
//! Vulkan) on the generated English and Portuguese speech.
//!
//! Live timings replay the recording at real time: each audio batch becomes
//! available at its capture time and the recognizer works through them in
//! order, as the live pump does. Compute times are measured; the clock is
//! simulated, so a run takes seconds, not the length of the audio.

use std::time::Instant;

use anyhow::{Context, Result};
use phorminx_audio::read_wav;
use phorminx_core::benchmark_report::{Example, Measure, mean, word_error_rate};
use phorminx_core::{AudioClip, TranscriptionOptions};
use phorminx_vosk::VoskModel;
use phorminx_whisper::{WhisperBackend, WhisperBackendPreference, WhisperRecognizer};
use serde_json::json;

use super::fixtures::{DICTATION, LEADING_SILENCE, Speech};
use super::{Assets, FeatureResult, run};

pub const FEATURE: &str = "dictation";

/// Whisper files the app can install: (file, label, languages).
pub const WHISPER_MODELS: &[(&str, &str, &[&str])] = &[
    ("ggml-base.en.bin", "base.en", &["en"]),
    ("ggml-small.en-q5_1.bin", "small.en q5_1", &["en"]),
    ("ggml-base.bin", "base", &["en", "pt"]),
];

/// The first live decode waits for this much speech, then windows of up to
/// three seconds overlap by half a second (see `phorminx_app::incremental`).
const FIRST_WINDOW: f64 = 1.5;
const WINDOW: f64 = 3.0;
const OVERLAP: f64 = 0.5;

pub struct Live {
    pub first_text: f64,
    pub end_to_text: f64,
    pub compute: f64,
    pub text: String,
}

/// Replays `clip` through Vosk in 100 ms batches.
pub fn vosk_live(model: &VoskModel, clip: &AudioClip) -> Result<Live> {
    let rate = clip.sample_rate as usize;
    let batch = rate / 10;
    let mut session = model
        .session(clip.sample_rate)
        .map_err(|error| anyhow::anyhow!("Vosk session failed: {error}"))?;
    let (mut clock, mut compute, mut first) = (0.0_f64, 0.0_f64, None);
    let mut words = Vec::new();
    for (index, chunk) in clip.samples.chunks(batch).enumerate() {
        let available = ((index * batch + chunk.len()) as f64) / rate as f64;
        let started = Instant::now();
        let accepted = session
            .accept_f32(chunk)
            .map_err(|error| anyhow::anyhow!("Vosk failed: {error}"))?;
        let shown = match accepted.endpoint {
            Some(segment) => {
                let text = segment.text.clone();
                words.push(segment.text);
                text
            }
            None => session
                .partial_text()
                .map_err(|error| anyhow::anyhow!("Vosk failed: {error}"))?,
        };
        let took = started.elapsed().as_secs_f64();
        compute += took;
        clock = clock.max(available) + took;
        if first.is_none() && !shown.trim().is_empty() {
            first = Some(clock);
        }
    }
    let audio_end = clip.samples.len() as f64 / rate as f64;
    let started = Instant::now();
    let last = session
        .finish()
        .map_err(|error| anyhow::anyhow!("Vosk failed: {error}"))?;
    let took = started.elapsed().as_secs_f64();
    words.push(last.text);
    Ok(Live {
        first_text: first.unwrap_or(audio_end) - LEADING_SILENCE,
        end_to_text: clock.max(audio_end) + took - audio_end,
        compute: compute + took,
        text: words.join(" "),
    })
}

fn slice(clip: &AudioClip, start: f64, end: f64) -> AudioClip {
    let rate = clip.sample_rate as f64;
    let from = ((start.max(0.0) * rate) as usize).min(clip.samples.len());
    let to = ((end * rate) as usize).clamp(from, clip.samples.len());
    AudioClip {
        samples: clip.samples[from..to].to_vec(),
        sample_rate: clip.sample_rate,
    }
}

/// Replays `clip` through Whisper's overlapping live windows, then the final
/// tail after release. Returns the live timings and the whole-recording text.
pub fn whisper_live(
    recognizer: &WhisperRecognizer,
    clip: &AudioClip,
    language: &str,
) -> Result<Live> {
    let options = TranscriptionOptions {
        language: Some(language),
        thread_count: None,
        audio_context: None,
    };
    let audio_end = clip.samples.len() as f64 / clip.sample_rate as f64;
    let (mut clock, mut compute, mut first) = (0.0_f64, 0.0_f64, None);
    let (mut start, mut end) = (0.0, LEADING_SILENCE + FIRST_WINDOW);
    while end < audio_end {
        let window = slice(clip, start, end);
        let started = Instant::now();
        let text = recognizer
            .transcribe_detailed(&window, &options, None, None)
            .context("Whisper failed")?
            .transcript
            .text;
        let took = started.elapsed().as_secs_f64();
        compute += took;
        clock = clock.max(end) + took;
        if first.is_none() && !text.trim().is_empty() {
            first = Some(clock);
        }
        start = end - OVERLAP;
        end = start + WINDOW;
    }
    let tail = slice(clip, start, audio_end);
    let started = Instant::now();
    recognizer
        .transcribe_detailed(&tail, &options, None, None)
        .context("Whisper failed")?;
    let took = started.elapsed().as_secs_f64();
    compute += took;
    let finished = clock.max(audio_end) + took;
    let text = recognizer
        .transcribe_detailed(clip, &options, None, None)
        .context("Whisper failed")?
        .transcript
        .text;
    Ok(Live {
        first_text: first.unwrap_or(finished) - LEADING_SILENCE,
        end_to_text: finished - audio_end,
        compute,
        text,
    })
}

fn clip(assets: &Assets, speech: &Speech) -> Result<AudioClip> {
    let path = speech.wav(assets);
    read_wav(&path).with_context(|| {
        format!(
            "failed to read {} (run `suite fixtures` first)",
            path.display()
        )
    })
}

fn live_metrics(lives: &[Live], reference: &str, audio: f64) -> Vec<(&'static str, Measure)> {
    let wer: Vec<f64> = lives
        .iter()
        .map(|live| word_error_rate(reference, &live.text))
        .collect();
    vec![
        ("wer", Measure::new(mean(&wer), "%")),
        (
            "rtf",
            Measure::of(
                &lives
                    .iter()
                    .map(|live| live.compute / audio)
                    .collect::<Vec<_>>(),
                "x",
            ),
        ),
        (
            "firstText",
            Measure::of(
                &lives.iter().map(|live| live.first_text).collect::<Vec<_>>(),
                "s",
            ),
        ),
        (
            "endToText",
            Measure::of(
                &lives
                    .iter()
                    .map(|live| live.end_to_text)
                    .collect::<Vec<_>>(),
                "s",
            ),
        ),
    ]
}

pub fn measure(assets: &Assets, repeats: u32) -> Result<FeatureResult> {
    let mut result = FeatureResult {
        note: "Generated speech (Windows voices Zira for English, Maria for Portuguese), 16 kHz mono. Live timings replay the recording at real time; Accurate's error rate is for the whole-recording text.".into(),
        ..Default::default()
    };
    let repeats = repeats.max(1) as usize;

    // Instant: Vosk, English only.
    let english = &DICTATION[0];
    let english_clip = clip(assets, english)?;
    let audio = english_clip.samples.len() as f64 / english_clip.sample_rate as f64;
    let model_name = "vosk-model-small-en-us-0.15";
    let started = Instant::now();
    let vosk = VoskModel::load(&assets.vosk_runtime(), &assets.vosk_model(), "en")
        .map_err(|error| anyhow::anyhow!("Vosk failed to load: {error}"))?;
    let load = started.elapsed().as_secs_f64();
    let mut lives = Vec::new();
    for _ in 0..repeats + 1 {
        lives.push(vosk_live(&vosk, &english_clip)?);
    }
    let cold = lives.remove(0);
    push_runs(
        &mut result,
        "instant",
        model_name,
        "cpu",
        english,
        load,
        cold,
        &lives,
        audio,
    );
    for speech in &DICTATION[1..] {
        result.skip(
            FEATURE,
            &format!(
                "Instant in {}",
                if speech.language == "pt" {
                    "Portuguese"
                } else {
                    speech.language
                }
            ),
            "Instant ships English Vosk models only; other languages use Accurate.",
        );
    }

    // Accurate: Whisper, each size, CPU and Vulkan.
    for (file, label, languages) in WHISPER_MODELS {
        let path = assets.whisper(file);
        if !path.is_file() {
            result.skip(
                FEATURE,
                &format!("Accurate {label}"),
                "model file not present",
            );
            continue;
        }
        for preference in [
            WhisperBackendPreference::Cpu,
            WhisperBackendPreference::Vulkan,
        ] {
            let started = Instant::now();
            let recognizer = match WhisperRecognizer::load_with_backend(&path, preference) {
                Ok(recognizer) => recognizer,
                Err(error) => {
                    result.skip(
                        FEATURE,
                        &format!("Accurate {label} on {}", preference.as_str()),
                        &format!("could not load: {error}"),
                    );
                    continue;
                }
            };
            let load = started.elapsed().as_secs_f64();
            let readiness = recognizer.readiness();
            if preference == WhisperBackendPreference::Vulkan
                && readiness.backend != WhisperBackend::Vulkan
            {
                result.skip(
                    FEATURE,
                    &format!("Accurate {label} on Vulkan"),
                    "this build or GPU has no Vulkan backend",
                );
                continue;
            }
            let backend = match (readiness.backend, readiness.device_name) {
                (WhisperBackend::Vulkan, Some(device)) => format!("vulkan · {device}"),
                (backend, _) => backend.as_str().to_owned(),
            };
            for speech in DICTATION
                .iter()
                .filter(|speech| languages.contains(&speech.language))
            {
                let clip = clip(assets, speech)?;
                let audio = clip.samples.len() as f64 / clip.sample_rate as f64;
                let mut lives = Vec::new();
                for _ in 0..repeats + 1 {
                    lives.push(whisper_live(&recognizer, &clip, speech.language)?);
                }
                let cold = lives.remove(0);
                // Only the first language decoded after loading is a cold start.
                let load = if speech.language == languages[0] {
                    load
                } else {
                    f64::NAN
                };
                push_runs(
                    &mut result,
                    "accurate",
                    label,
                    &backend,
                    speech,
                    load,
                    cold,
                    &lives,
                    audio,
                );
            }
        }
    }
    Ok(result)
}

#[allow(clippy::too_many_arguments)]
fn push_runs(
    result: &mut FeatureResult,
    engine: &str,
    model: &str,
    backend: &str,
    speech: &Speech,
    load: f64,
    cold: Live,
    warm: &[Live],
    audio: f64,
) {
    let reference = speech.reference();
    let warm_run = run(
        FEATURE,
        Some(model),
        backend,
        &[
            ("engine", json!(engine)),
            ("language", json!(speech.language)),
            ("start", json!("warm")),
        ],
        warm.len(),
        live_metrics(warm, &reference, audio),
    );
    println!(
        "{engine} {model} {backend} {}: WER {:.1}% RTF {:.3} first {:.2}s end {:.2}s",
        speech.language,
        warm_run.metrics["wer"].value,
        warm_run.metrics["rtf"].value,
        warm_run.metrics["firstText"].value,
        warm_run.metrics["endToText"].value
    );
    result.examples.push(Example {
        feature: FEATURE.to_owned(),
        run_id: warm_run.id.clone(),
        sample: speech.id.to_owned(),
        language: speech.language.to_owned(),
        input: reference.clone(),
        output: warm
            .first()
            .map(|live| live.text.trim().to_owned())
            .unwrap_or_default(),
    });
    result.runs.push(warm_run);
    if !load.is_nan() {
        let mut metrics = live_metrics(std::slice::from_ref(&cold), &reference, audio);
        metrics.push(("loadTime", Measure::new(load, "s")));
        metrics.push(("coldStart", Measure::new(load + cold.first_text, "s")));
        result.runs.push(run(
            FEATURE,
            Some(model),
            backend,
            &[
                ("engine", json!(engine)),
                ("language", json!(speech.language)),
                ("start", json!("cold")),
            ],
            1,
            metrics,
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slices_clamp_to_the_clip() {
        let clip = AudioClip {
            samples: vec![0.0; 16_000],
            sample_rate: 16_000,
        };
        assert_eq!(slice(&clip, 0.5, 0.75).samples.len(), 4_000);
        assert_eq!(slice(&clip, 0.9, 5.0).samples.len(), 1_600);
        assert!(slice(&clip, 2.0, 3.0).samples.is_empty());
    }

    #[test]
    fn whisper_models_name_their_languages() {
        assert!(
            WHISPER_MODELS
                .iter()
                .all(|(_, _, languages)| languages.contains(&"en"))
        );
        assert!(
            WHISPER_MODELS
                .iter()
                .any(|(_, _, languages)| languages.contains(&"pt"))
        );
    }
}
