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

/// Stable identifier used to correlate one activation across content-free logs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DictationId(pub u64);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeState {
    Starting,
    Idle,
    Listening,
    FinalizingAudio,
    Transcribing,
    Normalizing,
    ReadyToInsert,
    Inserting,
    Cancelled,
    Faulted,
}

/// The single-dictation state machine used by every shell implementation.
#[derive(Debug)]
pub struct RuntimeStateMachine {
    state: RuntimeState,
    active_id: Option<DictationId>,
    next_id: u64,
}

impl Default for RuntimeStateMachine {
    fn default() -> Self {
        Self {
            state: RuntimeState::Starting,
            active_id: None,
            next_id: 1,
        }
    }
}

impl RuntimeStateMachine {
    pub fn state(&self) -> RuntimeState {
        self.state
    }

    pub fn active_id(&self) -> Option<DictationId> {
        self.active_id
    }

    pub fn mark_ready(&mut self) -> Result<(), StateError> {
        self.transition(RuntimeState::Idle)
    }

    pub fn begin_dictation(&mut self) -> Result<DictationId, StateError> {
        if self.state != RuntimeState::Idle {
            return Err(StateError::Busy(self.state));
        }

        let id = DictationId(self.next_id);
        self.next_id = self.next_id.checked_add(1).unwrap_or(1);
        self.active_id = Some(id);
        self.transition(RuntimeState::Listening)?;
        Ok(id)
    }

    pub fn transition(&mut self, next: RuntimeState) -> Result<(), StateError> {
        if !valid_transition(self.state, next) {
            return Err(StateError::InvalidTransition {
                from: self.state,
                to: next,
            });
        }

        self.state = next;
        if next == RuntimeState::Idle {
            self.active_id = None;
        }
        Ok(())
    }

    pub fn cancel(&mut self) -> Result<(), StateError> {
        self.transition(RuntimeState::Cancelled)
    }

    pub fn fault(&mut self) -> Result<(), StateError> {
        self.transition(RuntimeState::Faulted)
    }
}

fn valid_transition(from: RuntimeState, to: RuntimeState) -> bool {
    matches!(
        (from, to),
        (RuntimeState::Starting, RuntimeState::Idle)
            | (RuntimeState::Starting, RuntimeState::Faulted)
            | (RuntimeState::Idle, RuntimeState::Listening)
            | (RuntimeState::Listening, RuntimeState::FinalizingAudio)
            | (RuntimeState::FinalizingAudio, RuntimeState::Transcribing)
            | (RuntimeState::Transcribing, RuntimeState::Normalizing)
            | (RuntimeState::Normalizing, RuntimeState::ReadyToInsert)
            | (RuntimeState::ReadyToInsert, RuntimeState::Inserting)
            | (RuntimeState::Inserting, RuntimeState::Idle)
            | (RuntimeState::Cancelled, RuntimeState::Idle)
            | (RuntimeState::Faulted, RuntimeState::Idle)
            | (RuntimeState::Listening, RuntimeState::Cancelled)
            | (RuntimeState::FinalizingAudio, RuntimeState::Cancelled)
            | (RuntimeState::Transcribing, RuntimeState::Cancelled)
            | (RuntimeState::Normalizing, RuntimeState::Cancelled)
            | (RuntimeState::ReadyToInsert, RuntimeState::Cancelled)
            | (RuntimeState::Inserting, RuntimeState::Cancelled)
            | (RuntimeState::Listening, RuntimeState::Faulted)
            | (RuntimeState::FinalizingAudio, RuntimeState::Faulted)
            | (RuntimeState::Transcribing, RuntimeState::Faulted)
            | (RuntimeState::Normalizing, RuntimeState::Faulted)
            | (RuntimeState::ReadyToInsert, RuntimeState::Faulted)
            | (RuntimeState::Inserting, RuntimeState::Faulted)
    )
}

/// Conservative deterministic normalization that never rewrites words.
pub fn normalize_transcript(input: &str) -> String {
    let collapsed = input.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut normalized = String::with_capacity(collapsed.len());

    for character in collapsed.chars() {
        if matches!(character, '.' | ',' | '!' | '?' | ';' | ':') && normalized.ends_with(' ') {
            normalized.pop();
        }
        normalized.push(character);
    }

    normalized
}

/// Chooses a bounded Whisper audio context for a completed utterance.
///
/// Whisper uses roughly 50 encoder context units per second. Rounding to a
/// multiple of 32 avoids undersizing while keeping short dictations responsive.
pub fn recommended_audio_context(duration: Duration) -> u32 {
    let required = (duration.as_secs_f64() * 50.0).ceil() as u32;
    let rounded = required.saturating_add(31) / 32 * 32;
    rounded.clamp(256, 1_500)
}

#[derive(Debug, thiserror::Error)]
pub enum AudioError {
    #[error("sample rate must be greater than zero")]
    InvalidSampleRate,
}

#[derive(Debug, thiserror::Error, Eq, PartialEq)]
pub enum StateError {
    #[error("Phorminx is busy in state {0:?}")]
    Busy(RuntimeState),
    #[error("invalid runtime transition from {from:?} to {to:?}")]
    InvalidTransition {
        from: RuntimeState,
        to: RuntimeState,
    },
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

    #[test]
    fn drives_the_successful_dictation_path() {
        let mut machine = RuntimeStateMachine::default();
        machine.mark_ready().unwrap();
        let id = machine.begin_dictation().unwrap();

        assert_eq!(id, DictationId(1));
        assert_eq!(machine.active_id(), Some(id));

        for state in [
            RuntimeState::FinalizingAudio,
            RuntimeState::Transcribing,
            RuntimeState::Normalizing,
            RuntimeState::ReadyToInsert,
            RuntimeState::Inserting,
            RuntimeState::Idle,
        ] {
            machine.transition(state).unwrap();
        }

        assert_eq!(machine.state(), RuntimeState::Idle);
        assert_eq!(machine.active_id(), None);
    }

    #[test]
    fn rejects_a_second_activation_while_listening() {
        let mut machine = RuntimeStateMachine::default();
        machine.mark_ready().unwrap();
        machine.begin_dictation().unwrap();

        assert_eq!(
            machine.begin_dictation(),
            Err(StateError::Busy(RuntimeState::Listening))
        );
    }

    #[test]
    fn normalizes_only_spacing() {
        assert_eq!(
            normalize_transcript("  Hello   world ,  this is Phorminx. "),
            "Hello world, this is Phorminx."
        );
    }

    #[test]
    fn sizes_audio_context_for_short_and_long_dictation() {
        assert_eq!(recommended_audio_context(Duration::from_secs(1)), 256);
        assert_eq!(recommended_audio_context(Duration::from_secs(20)), 1_024);
        assert_eq!(recommended_audio_context(Duration::from_secs(60)), 1_500);
    }
}
