//! Local whisper.cpp-backed speech recognition.

use std::ffi::c_void;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use phorminx_core::{AudioClip, SpeechRecognizer, Transcript, TranscriptionOptions};
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

pub struct WhisperRecognizer {
    context: WhisperContext,
    model_load_time: Duration,
    backend: WhisperBackend,
    device_name: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum WhisperBackendPreference {
    #[default]
    Auto,
    Vulkan,
    Cpu,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WhisperBackend {
    Cpu,
    Vulkan,
}

impl WhisperBackend {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Cpu => "cpu",
            Self::Vulkan => "vulkan",
        }
    }
}

/// Reports the backend/device that a new resident recognizer would select.
/// Model readiness still requires a successful `load_with_backend` call.
pub fn probe_backend(
    preference: WhisperBackendPreference,
) -> Result<(WhisperBackend, Option<String>), WhisperError> {
    select_backend(preference)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WhisperReadiness {
    pub backend: WhisperBackend,
    pub device_name: Option<String>,
    pub model_load_time: Duration,
}

#[derive(Clone, Debug)]
pub struct TimedSegment {
    pub text: String,
    pub start: Duration,
    pub end: Duration,
}

#[derive(Clone, Debug)]
pub struct DetailedTranscript {
    pub transcript: Transcript,
    pub segments: Vec<TimedSegment>,
}

impl SpeechRecognizer for WhisperRecognizer {
    type Error = WhisperError;

    fn load(model_path: &Path) -> Result<Self, Self::Error> {
        Self::load_with_backend(model_path, WhisperBackendPreference::Auto)
    }

    fn transcribe(
        &self,
        clip: &AudioClip,
        options: &TranscriptionOptions<'_>,
    ) -> Result<Transcript, Self::Error> {
        self.transcribe_detailed(clip, options, None, None)
            .map(|detailed| detailed.transcript)
    }
}

impl WhisperRecognizer {
    pub fn load_with_backend(
        model_path: &Path,
        preference: WhisperBackendPreference,
    ) -> Result<Self, WhisperError> {
        if !model_path.is_file() {
            return Err(WhisperError::ModelNotFound(model_path.to_path_buf()));
        }

        let started = Instant::now();
        let mut context_parameters = WhisperContextParameters::default();
        let (backend, device_name) = select_backend(preference)?;
        context_parameters.use_gpu(backend == WhisperBackend::Vulkan);
        let context = WhisperContext::new_with_params(
            model_path
                .to_str()
                .ok_or_else(|| WhisperError::NonUtf8ModelPath(model_path.to_path_buf()))?,
            context_parameters,
        )?;

        Ok(Self {
            context,
            model_load_time: started.elapsed(),
            backend,
            device_name,
        })
    }

    pub fn readiness(&self) -> WhisperReadiness {
        WhisperReadiness {
            backend: self.backend,
            device_name: self.device_name.clone(),
            model_load_time: self.model_load_time,
        }
    }

    /// Runs Whisper with segment timestamps, bounded decoder context, and an
    /// optional cooperative abort flag. The prompt is decoder context only; it
    /// is never emitted or logged by this layer.
    pub fn transcribe_detailed(
        &self,
        clip: &AudioClip,
        options: &TranscriptionOptions<'_>,
        initial_prompt: Option<&str>,
        abort: Option<&Arc<AtomicBool>>,
    ) -> Result<DetailedTranscript, WhisperError> {
        if clip.sample_rate != 16_000 {
            return Err(WhisperError::WrongSampleRate(clip.sample_rate));
        }
        if clip.samples.is_empty() {
            return Err(WhisperError::EmptyAudio);
        }

        let mut parameters = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        parameters.set_language(options.language);
        parameters.set_translate(false);
        parameters.set_no_context(initial_prompt.is_none());
        parameters.set_no_timestamps(false);
        parameters.set_print_progress(false);
        parameters.set_print_realtime(false);
        parameters.set_print_special(false);
        let threads = options.thread_count.unwrap_or_else(default_thread_count);
        parameters.set_n_threads(threads as i32);
        if let Some(audio_context) = options.audio_context {
            parameters.set_audio_ctx(audio_context as i32);
        }
        if let Some(prompt) = initial_prompt.filter(|prompt| !prompt.is_empty()) {
            parameters.set_initial_prompt(prompt);
            parameters.set_n_max_text_ctx(128);
        }
        if let Some(abort) = abort {
            // SAFETY: `state.full` is synchronous, `abort` owns the AtomicBool
            // for the complete call, and the callback only performs an atomic
            // read through that stable Arc allocation.
            unsafe {
                parameters.set_abort_callback(Some(whisper_abort_callback));
                parameters
                    .set_abort_callback_user_data(Arc::as_ptr(abort).cast_mut().cast::<c_void>());
            }
        }

        let started = Instant::now();
        let mut state = self.context.create_state()?;
        let recognition = state.full(parameters, &clip.samples);
        if abort.is_some_and(|flag| flag.load(Ordering::Acquire)) {
            return Err(WhisperError::Aborted);
        }
        recognition?;
        let segments = state
            .as_iter()
            .map(|segment| TimedSegment {
                text: segment.to_string(),
                start: centiseconds(segment.start_timestamp()),
                end: centiseconds(segment.end_timestamp()),
            })
            .collect::<Vec<_>>();
        let text = segments
            .iter()
            .map(|segment| segment.text.as_str())
            .collect::<String>()
            .trim()
            .to_owned();

        Ok(DetailedTranscript {
            transcript: Transcript {
                text,
                backend: self.backend.as_str(),
                model_load_time: self.model_load_time,
                inference_time: started.elapsed(),
                audio_duration: clip.duration(),
            },
            segments,
        })
    }
}

unsafe extern "C" fn whisper_abort_callback(user_data: *mut c_void) -> bool {
    if user_data.is_null() {
        return false;
    }
    // SAFETY: the caller installs a pointer from a live Arc<AtomicBool> and
    // keeps that Arc alive until the synchronous Whisper call returns.
    unsafe { &*user_data.cast::<AtomicBool>() }.load(Ordering::Acquire)
}

fn centiseconds(value: i64) -> Duration {
    Duration::from_millis(value.max(0) as u64 * 10)
}

#[cfg(feature = "vulkan")]
fn select_backend(
    preference: WhisperBackendPreference,
) -> Result<(WhisperBackend, Option<String>), WhisperError> {
    if preference == WhisperBackendPreference::Cpu {
        return Ok((WhisperBackend::Cpu, None));
    }
    let device = whisper_rs::vulkan::list_devices().into_iter().next();
    match (preference, device) {
        (_, Some(device)) => Ok((WhisperBackend::Vulkan, Some(device.name))),
        (WhisperBackendPreference::Vulkan, None) => Err(WhisperError::VulkanDeviceUnavailable),
        (WhisperBackendPreference::Auto, None) => Ok((WhisperBackend::Cpu, None)),
        (WhisperBackendPreference::Cpu, _) => unreachable!(),
    }
}

#[cfg(not(feature = "vulkan"))]
fn select_backend(
    preference: WhisperBackendPreference,
) -> Result<(WhisperBackend, Option<String>), WhisperError> {
    match preference {
        WhisperBackendPreference::Vulkan => Err(WhisperError::VulkanNotCompiled),
        WhisperBackendPreference::Auto | WhisperBackendPreference::Cpu => {
            Ok((WhisperBackend::Cpu, None))
        }
    }
}

fn default_thread_count() -> usize {
    std::thread::available_parallelism()
        .map(|count| count.get().min(8))
        .unwrap_or(4)
}

#[derive(Debug, thiserror::Error)]
pub enum WhisperError {
    #[error("Whisper model was not found: {0}")]
    ModelNotFound(std::path::PathBuf),
    #[error("Whisper model path is not valid Unicode: {0}")]
    NonUtf8ModelPath(std::path::PathBuf),
    #[error("Whisper requires 16 kHz audio, received {0} Hz")]
    WrongSampleRate(u32),
    #[error("cannot transcribe an empty audio clip")]
    EmptyAudio,
    #[error("Whisper transcription was aborted")]
    Aborted,
    #[error("Vulkan was requested but this Phorminx build does not include it")]
    VulkanNotCompiled,
    #[error("Vulkan was requested but whisper.cpp found no compatible device")]
    VulkanDeviceUnavailable,
    #[error(transparent)]
    Backend(#[from] whisper_rs::WhisperError),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_cpu_never_claims_a_gpu() {
        assert_eq!(
            select_backend(WhisperBackendPreference::Cpu).unwrap(),
            (WhisperBackend::Cpu, None)
        );
    }

    #[cfg(not(feature = "vulkan"))]
    #[test]
    fn explicit_vulkan_fails_in_a_cpu_only_binary() {
        assert!(matches!(
            select_backend(WhisperBackendPreference::Vulkan),
            Err(WhisperError::VulkanNotCompiled)
        ));
    }
}
