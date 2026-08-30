//! Platform-neutral domain types and contracts for Phorminx.

use std::path::Path;
use std::time::Duration;

/// Mono floating-point PCM audio.
#[derive(Clone, Debug)]
pub struct AudioClip {
    pub samples: Vec<f32>,
    pub sample_rate: u32,
}

impl AudioClip {
    pub fn new(samples: Vec<f32>, sample_rate: u32) -> Result<Self, AudioError> {
        if sample_rate == 0 {
            return Err(AudioError::InvalidSampleRate);
        }

        Ok(Self {
            samples,
            sample_rate,
        })
    }

    pub fn duration(&self) -> Duration {
        Duration::from_secs_f64(self.samples.len() as f64 / self.sample_rate as f64)
    }

    pub fn peak_amplitude(&self) -> f32 {
        self.samples
            .iter()
            .copied()
            .map(f32::abs)
            .fold(0.0, f32::max)
    }

    pub fn rms(&self) -> f32 {
        if self.samples.is_empty() {
            return 0.0;
        }

        let mean_square = self
            .samples
            .iter()
            .map(|sample| f64::from(*sample) * f64::from(*sample))
            .sum::<f64>()
            / self.samples.len() as f64;

        mean_square.sqrt() as f32
    }
}

#[derive(Clone, Debug)]
pub struct TranscriptionOptions<'a> {
    pub language: Option<&'a str>,
    pub thread_count: Option<usize>,
    /// Experimental Whisper encoder context override. `None` uses the model default.
    pub audio_context: Option<u32>,
}

impl Default for TranscriptionOptions<'_> {
    fn default() -> Self {
        Self {
            language: Some("en"),
            thread_count: None,
            audio_context: None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Transcript {
    pub text: String,
    pub backend: &'static str,
    pub model_load_time: Duration,
    pub inference_time: Duration,
    pub audio_duration: Duration,
}

impl Transcript {
    pub fn realtime_factor(&self) -> f64 {
        let audio_seconds = self.audio_duration.as_secs_f64();
        if audio_seconds == 0.0 {
            return 0.0;
        }

        self.inference_time.as_secs_f64() / audio_seconds
    }
}

pub trait SpeechRecognizer {
    type Error: std::error::Error + Send + Sync + 'static;

    fn load(model_path: &Path) -> Result<Self, Self::Error>
    where
        Self: Sized;

    fn transcribe(
        &self,
        clip: &AudioClip,
        options: &TranscriptionOptions<'_>,
    ) -> Result<Transcript, Self::Error>;
}

#[derive(Debug, thiserror::Error)]
pub enum AudioError {
    #[error("sample rate must be greater than zero")]
    InvalidSampleRate,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn computes_audio_metrics() {
        let clip = AudioClip::new(vec![0.0, 0.5, -1.0, 0.5], 4).unwrap();

        assert_eq!(clip.duration(), Duration::from_secs(1));
        assert_eq!(clip.peak_amplitude(), 1.0);
        assert!((clip.rms() - 0.612_372_46).abs() < 1e-6);
    }

    #[test]
    fn rejects_zero_sample_rate() {
        assert!(matches!(
            AudioClip::new(Vec::new(), 0),
            Err(AudioError::InvalidSampleRate)
        ));
    }
}
