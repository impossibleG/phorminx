//! Safe, dynamically-loaded boundary around Vosk's stable C API.
//!
//! Phorminx does not fetch or install native code. The caller supplies a local
//! runtime bundle containing `vosk.dll` and a local unpacked model directory.

use std::ffi::{CStr, CString, c_char, c_float, c_int, c_void};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use libloading::Library;
use serde::Deserialize;

const WINDOWS_LIBRARY: &str = "libvosk.dll";
const WINDOWS_RUNTIME_FILES: [&str; 4] = [
    WINDOWS_LIBRARY,
    "libgcc_s_seh-1.dll",
    "libstdc++-6.dll",
    "libwinpthread-1.dll",
];

type ModelNew = unsafe extern "C" fn(*const c_char) -> *mut c_void;
type ModelFree = unsafe extern "C" fn(*mut c_void);
type RecognizerNew = unsafe extern "C" fn(*mut c_void, c_float) -> *mut c_void;
type RecognizerFree = unsafe extern "C" fn(*mut c_void);
type AcceptWaveform = unsafe extern "C" fn(*mut c_void, *const c_char, c_int) -> c_int;
type ResultFn = unsafe extern "C" fn(*mut c_void) -> *const c_char;
type SetLogLevel = unsafe extern "C" fn(c_int);

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Readiness {
    Ready { warning: Option<&'static str> },
    MissingRuntime { expected: PathBuf },
    MissingModel { expected: PathBuf },
    LoadFailed { component: &'static str },
    UnsupportedLanguage { language: String },
}

impl Readiness {
    pub fn is_ready(&self) -> bool {
        matches!(self, Self::Ready { .. })
    }
}

pub fn inspect(runtime_bundle: &Path, model: &Path, language: &str) -> Readiness {
    let language = language.trim().to_ascii_lowercase();
    if !matches!(language.as_str(), "en" | "en-us" | "pt" | "pt-br") {
        return Readiness::UnsupportedLanguage { language };
    }
    for required in WINDOWS_RUNTIME_FILES {
        let expected = runtime_bundle.join(required);
        if !expected.is_file() {
            return Readiness::MissingRuntime { expected };
        }
    }
    if !model.is_dir() {
        return Readiness::MissingModel {
            expected: model.to_path_buf(),
        };
    }
    Readiness::Ready {
        warning: matches!(language.as_str(), "pt" | "pt-br").then_some(
            "Instant Portuguese quality depends strongly on the selected Vosk model; Accurate mode is recommended when fidelity matters.",
        ),
    }
}

struct Api {
    _library: Library,
    model_new: ModelNew,
    model_free: ModelFree,
    recognizer_new: RecognizerNew,
    recognizer_free: RecognizerFree,
    accept_waveform: AcceptWaveform,
    result: ResultFn,
    final_result: ResultFn,
}

impl Api {
    unsafe fn load(path: &Path) -> Result<Self, VoskError> {
        // SAFETY: symbol types are copied immediately and the Library remains
        // owned by Api for at least as long as any copied function pointer.
        let library = unsafe { load_library(path) }?;
        macro_rules! symbol {
            ($name:literal, $type:ty) => {{
                // SAFETY: names and signatures are defined by Vosk's public C API.
                let found = unsafe { library.get::<$type>(concat!($name, "\0").as_bytes()) }
                    .map_err(|_| VoskError::MissingSymbol($name))?;
                *found
            }};
        }
        let model_new = symbol!("vosk_model_new", ModelNew);
        let model_free = symbol!("vosk_model_free", ModelFree);
        let recognizer_new = symbol!("vosk_recognizer_new", RecognizerNew);
        let recognizer_free = symbol!("vosk_recognizer_free", RecognizerFree);
        let accept_waveform = symbol!("vosk_recognizer_accept_waveform", AcceptWaveform);
        let result = symbol!("vosk_recognizer_result", ResultFn);
        let final_result = symbol!("vosk_recognizer_final_result", ResultFn);
        let set_log_level = symbol!("vosk_set_log_level", SetLogLevel);
        // SAFETY: public Vosk API; suppress native path/content-adjacent logs so
        // Phorminx owns its content-free diagnostic boundary.
        unsafe { set_log_level(-1) };
        Ok(Self {
            _library: library,
            model_new,
            model_free,
            recognizer_new,
            recognizer_free,
            accept_waveform,
            result,
            final_result,
        })
    }
}

#[cfg(windows)]
unsafe fn load_library(path: &Path) -> Result<Library, VoskError> {
    use libloading::os::windows::{
        LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR, LOAD_LIBRARY_SEARCH_SYSTEM32, Library as WindowsLibrary,
    };
    // SAFETY: caller accepts native initializer execution. Restrict dependency
    // lookup to the pinned bundle directory and System32; never search cwd/PATH.
    let library = unsafe {
        WindowsLibrary::load_with_flags(
            path,
            LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_SYSTEM32,
        )
    }
    .map_err(|source| VoskError::LoadLibrary(source.to_string()))?;
    Ok(library.into())
}

#[cfg(not(windows))]
unsafe fn load_library(path: &Path) -> Result<Library, VoskError> {
    // SAFETY: caller accepts native initializer execution.
    unsafe { Library::new(path) }.map_err(|source| VoskError::LoadLibrary(source.to_string()))
}

struct ModelInner {
    api: Api,
    raw: *mut c_void,
}

impl Drop for ModelInner {
    fn drop(&mut self) {
        // SAFETY: raw was returned by model_new and remains uniquely owned.
        unsafe { (self.api.model_free)(self.raw) };
    }
}

#[derive(Clone)]
pub struct VoskModel {
    inner: Rc<ModelInner>,
    load_time: Duration,
}

impl VoskModel {
    pub fn load(runtime_bundle: &Path, model: &Path, language: &str) -> Result<Self, VoskError> {
        match inspect(runtime_bundle, model, language) {
            Readiness::Ready { .. } => {}
            readiness => return Err(VoskError::NotReady(readiness)),
        }
        let model_text = model
            .to_str()
            .ok_or_else(|| VoskError::NonUtf8Path(model.to_path_buf()))?;
        let model_path = CString::new(model_text)
            .map_err(|_| VoskError::PathContainsNul(model.to_path_buf()))?;
        let started = Instant::now();
        let api = unsafe { Api::load(&runtime_bundle.join(WINDOWS_LIBRARY)) }?;
        // SAFETY: model_path is a valid NUL-terminated string for this call.
        let raw = unsafe { (api.model_new)(model_path.as_ptr()) };
        if raw.is_null() {
            return Err(VoskError::ModelLoadFailed);
        }
        Ok(Self {
            inner: Rc::new(ModelInner { api, raw }),
            load_time: started.elapsed(),
        })
    }

    pub fn load_time(&self) -> Duration {
        self.load_time
    }

    pub fn session(&self, sample_rate: u32) -> Result<VoskSession, VoskError> {
        if sample_rate == 0 {
            return Err(VoskError::InvalidSampleRate);
        }
        // SAFETY: model stays alive through the cloned Rc stored in session.
        let raw =
            unsafe { (self.inner.api.recognizer_new)(self.inner.raw, sample_rate as c_float) };
        if raw.is_null() {
            return Err(VoskError::RecognizerCreateFailed);
        }
        Ok(VoskSession {
            model: Rc::clone(&self.inner),
            raw,
            finalized: FinalizedText::default(),
        })
    }
}

pub struct VoskSession {
    model: Rc<ModelInner>,
    raw: *mut c_void,
    finalized: FinalizedText,
}

impl VoskSession {
    /// Accepts native-rate mono f32 samples. Conversion happens on the decoder
    /// worker, never in the audio callback.
    pub fn accept_f32(&mut self, samples: &[f32]) -> Result<bool, VoskError> {
        if samples.is_empty() {
            return Ok(false);
        }
        let mut pcm = Vec::with_capacity(samples.len() * 2);
        for sample in samples {
            pcm.extend_from_slice(
                &((*sample).clamp(-1.0, 1.0) * i16::MAX as f32)
                    .round()
                    .to_le_bytes(),
            );
        }
        let len = c_int::try_from(pcm.len()).map_err(|_| VoskError::AudioBatchTooLarge)?;
        // SAFETY: pcm remains live for the duration of the synchronous call.
        let accepted =
            unsafe { (self.model.api.accept_waveform)(self.raw, pcm.as_ptr().cast(), len) };
        match accepted {
            0 => Ok(false),
            1 => {
                let text = self.read_json(self.model.api.result)?;
                self.finalized.endpoint(text);
                Ok(true)
            }
            _ => Err(VoskError::DecoderFailed),
        }
    }

    pub fn finish(mut self) -> Result<String, VoskError> {
        let text = self.read_json(self.model.api.final_result)?;
        Ok(std::mem::take(&mut self.finalized).finish(text))
    }

    fn read_json(&self, function: ResultFn) -> Result<String, VoskError> {
        // SAFETY: Vosk owns the returned NUL-terminated buffer until the next
        // recognizer call; we deserialize before returning.
        let pointer = unsafe { function(self.raw) };
        if pointer.is_null() {
            return Err(VoskError::NullResult);
        }
        let bytes = unsafe { CStr::from_ptr(pointer) }.to_bytes();
        parse_text(bytes)
    }
}

#[derive(Default)]
struct FinalizedText {
    endpoints: Vec<String>,
}

impl FinalizedText {
    fn endpoint(&mut self, text: String) {
        if !text.is_empty() {
            self.endpoints.push(text);
        }
    }

    fn finish(mut self, terminal: String) -> String {
        if !terminal.is_empty() {
            self.endpoints.push(terminal);
        }
        self.endpoints.join(" ")
    }
}

impl Drop for VoskSession {
    fn drop(&mut self) {
        // SAFETY: raw was returned by recognizer_new and is uniquely owned.
        unsafe { (self.model.api.recognizer_free)(self.raw) };
    }
}

#[derive(Deserialize)]
struct ResultDocument {
    #[serde(default)]
    text: String,
}

fn parse_text(json: &[u8]) -> Result<String, VoskError> {
    serde_json::from_slice::<ResultDocument>(json)
        .map(|document| document.text.trim().to_owned())
        .map_err(VoskError::InvalidJson)
}

#[derive(Debug, thiserror::Error)]
pub enum VoskError {
    #[error("Vosk is not ready: {0:?}")]
    NotReady(Readiness),
    #[error("failed to load the Vosk runtime: {0}")]
    LoadLibrary(String),
    #[error("the Vosk runtime is missing required symbol {0}")]
    MissingSymbol(&'static str),
    #[error("Vosk model path is not UTF-8: {0}")]
    NonUtf8Path(PathBuf),
    #[error("Vosk model path contains a NUL byte: {0}")]
    PathContainsNul(PathBuf),
    #[error("Vosk could not load the selected model")]
    ModelLoadFailed,
    #[error("Vosk could not create a streaming recognizer")]
    RecognizerCreateFailed,
    #[error("sample rate must be positive")]
    InvalidSampleRate,
    #[error("audio batch is too large for the Vosk C API")]
    AudioBatchTooLarge,
    #[error("Vosk rejected the audio batch")]
    DecoderFailed,
    #[error("Vosk returned a null result")]
    NullResult,
    #[error("Vosk returned malformed result JSON: {0}")]
    InvalidJson(serde_json::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_text_without_exposing_other_result_fields() {
        assert_eq!(
            parse_text(br#"{"text":"  hello world  ","result":[{"word":"hello"}]}"#).unwrap(),
            "hello world"
        );
    }

    #[test]
    fn readiness_is_typed_and_language_checked_first() {
        let root = std::env::temp_dir().join("phorminx-vosk-never-created");
        assert!(matches!(
            inspect(&root, &root, "fr"),
            Readiness::UnsupportedLanguage { .. }
        ));
        assert!(matches!(
            inspect(&root, &root, "en"),
            Readiness::MissingRuntime { .. }
        ));
    }

    #[test]
    fn endpoint_segments_and_terminal_result_are_committed_exactly_once() {
        let mut text = FinalizedText::default();
        text.endpoint("the first phrase".to_owned());
        text.endpoint("the second phrase".to_owned());
        assert_eq!(
            text.finish("the final phrase".to_owned()),
            "the first phrase the second phrase the final phrase"
        );
    }

    #[test]
    fn natural_pauses_silence_and_legitimate_repeated_words_do_not_duplicate_segments() {
        let mut text = FinalizedText::default();
        text.endpoint("very very useful".to_owned());
        // Fifteen seconds of silence produces no endpoint text.
        text.endpoint(String::new());
        assert_eq!(
            text.finish("after the pause".to_owned()),
            "very very useful after the pause"
        );
    }

    #[test]
    #[ignore = "requires explicitly installed Vosk runtime and model assets"]
    fn installed_native_bundle_loads_and_streams() {
        let runtime = std::env::var_os("PHORMINX_VOSK_RUNTIME").unwrap();
        let model = std::env::var_os("PHORMINX_VOSK_MODEL").unwrap();
        let loaded = VoskModel::load(Path::new(&runtime), Path::new(&model), "en").unwrap();
        let mut session = loaded.session(16_000).unwrap();
        session.accept_f32(&vec![0.0; 16_000]).unwrap();
        assert!(session.finish().unwrap().is_empty());
    }
}
