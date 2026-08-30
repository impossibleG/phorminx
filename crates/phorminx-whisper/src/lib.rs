//! Local whisper.cpp-backed speech recognition.

use std::path::Path;
use std::time::{Duration, Instant};

use phorminx_core::{AudioClip, SpeechRecognizer, Transcript, TranscriptionOptions};
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

pub struct WhisperRecognizer {
    context: WhisperContext,
    model_load_time: Duration,
}

impl SpeechRecognizer for WhisperRecognizer {
    type Error = WhisperError;

    fn load(model_path: &Path) -> Result<Self, Self::Error> {
        if !model_path.is_file() {
            return Err(WhisperError::ModelNotFound(model_path.to_path_buf()));
        }

        let started = Instant::now();
        let mut context_parameters = WhisperContextParameters::default();
        context_parameters.use_gpu(cfg!(feature = "vulkan"));
        let context = WhisperContext::new_with_params(
            model_path
                .to_str()
                .ok_or_else(|| WhisperError::NonUtf8ModelPath(model_path.to_path_buf()))?,
            context_parameters,
        )?;

        Ok(Self {
            context,
            model_load_time: started.elapsed(),
        })
    }

    fn transcribe(
        &self,
        clip: &AudioClip,
        options: &TranscriptionOptions<'_>,
    ) -> Result<Transcript, Self::Error> {
        if clip.sample_rate != 16_000 {
            return Err(WhisperError::WrongSampleRate(clip.sample_rate));
        }
        if clip.samples.is_empty() {
            return Err(WhisperError::EmptyAudio);
        }

        let mut parameters = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        parameters.set_language(options.language);
        parameters.set_translate(false);
        parameters.set_no_context(true);
        parameters.set_no_timestamps(true);
        parameters.set_print_progress(false);
        parameters.set_print_realtime(false);
        parameters.set_print_special(false);
        let threads = options.thread_count.unwrap_or_else(default_thread_count);
        parameters.set_n_threads(threads as i32);
        if let Some(audio_context) = options.audio_context {
            parameters.set_audio_ctx(audio_context as i32);
        }

        let started = Instant::now();
        let mut state = self.context.create_state()?;
        state.full(parameters, &clip.samples)?;
        let text = state
            .as_iter()
            .map(|segment| segment.to_string())
            .collect::<String>()
            .trim()
            .to_owned();

        Ok(Transcript {
            text,
            backend: if cfg!(feature = "vulkan") {
                "vulkan"
            } else {
                "cpu"
            },
            model_load_time: self.model_load_time,
            inference_time: started.elapsed(),
            audio_duration: clip.duration(),
        })
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
    #[error(transparent)]
    Backend(#[from] whisper_rs::WhisperError),
}
