//! Every model Phorminx can use, in one list: installed Ollama models (typed
//! by the capabilities Ollama reports), the speech and screenshot models, and
//! a short curated list of models worth installing. Each entry says what kind
//! of model it is, so pickers only offer models that fit the job, and which
//! features currently use it.
//!
//! Installing by name goes through Ollama's own pull. Nothing here sends data
//! anywhere but the local Ollama service.

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::Duration;

use phorminx_assistant::{ActionPayloadMode, AssistantConfig, ConfigStore, ProviderKind};
use phorminx_ollama::{CancellationToken, ClientTimeouts, ModelName, OllamaClient, OllamaEndpoint};
use serde::Serialize;

use crate::investigation_ocr::{OCR_MODEL_DIRECTORY, verify_installed};
use crate::settings::{Settings, SettingsStore};

const OLLAMA: &str = "http://127.0.0.1:11434";

/// Kinds of model. A slot accepts one kind; a model can have several.
pub const LANGUAGE: &str = "language";
pub const EMBEDDING: &str = "embedding";
pub const VISION: &str = "vision";
pub const TOOLS: &str = "tools";
pub const SPEECH: &str = "speech";
pub const OCR: &str = "ocr";

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RegistryEntry {
    /// Ollama tag (`qwen2.5:7b`), Whisper file or Vosk directory.
    pub name: String,
    /// `ollama`, `whisper`, `vosk` or `ocr`.
    pub family: &'static str,
    pub tags: Vec<&'static str>,
    pub installed: bool,
    pub size_bytes: Option<u64>,
    /// Features that use this model right now.
    pub used_by: Vec<String>,
    /// A short reason to pick it, for curated models.
    pub note: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PullView {
    pub name: String,
    /// `idle`, `pulling`, `done` or `failed`.
    pub state: &'static str,
    pub percent: Option<u8>,
    pub message: String,
}

/// Models worth suggesting, with the kinds Ollama reports for them.
const CURATED: &[(&str, &[&str], &str)] = &[
    (
        "qwen2.5:1.5b",
        &[LANGUAGE, TOOLS],
        "Fastest formatting; the starter model.",
    ),
    (
        "qwen2.5:7b",
        &[LANGUAGE, TOOLS],
        "Careful formatting that turns spoken lists into lists.",
    ),
    (
        "gemma3:4b",
        &[LANGUAGE, VISION],
        "Good with lists; needs about 4 GB of GPU memory.",
    ),
    (
        "qwen2.5:3b-instruct",
        &[LANGUAGE, TOOLS],
        "A middle ground between 1.5B and 7B.",
    ),
    ("llama3.2:1b", &[LANGUAGE, TOOLS], "Very small and quick."),
    ("gemma3:1b", &[LANGUAGE], "Very small and quick."),
    (
        "nomic-embed-text:latest",
        &[EMBEDDING],
        "Search across history and meetings; the starter model.",
    ),
];

/// `name` and `name:latest` are the same Ollama model.
pub fn same_model(left: &str, right: &str) -> bool {
    let normalize = |name: &str| {
        let name = name.trim().to_ascii_lowercase();
        if name.contains(':') {
            name
        } else {
            format!("{name}:latest")
        }
    };
    normalize(left) == normalize(right)
}

/// Phorminx kinds for Ollama's capability names.
pub fn kinds_from_capabilities(capabilities: &[String]) -> Vec<&'static str> {
    let mut kinds = Vec::new();
    for capability in capabilities {
        let kind = match capability.as_str() {
            "completion" => LANGUAGE,
            "embedding" => EMBEDDING,
            "vision" => VISION,
            "tools" => TOOLS,
            _ => continue,
        };
        if !kinds.contains(&kind) {
            kinds.push(kind);
        }
    }
    kinds
}

/// Which features use which model, from the saved settings.
pub fn uses(settings: &Settings, assistant: &AssistantConfig) -> Vec<(String, String)> {
    let mut uses = Vec::new();
    let mut add = |model: &str, feature: &str| {
        if !model.trim().is_empty() {
            uses.push((model.trim().to_owned(), feature.to_owned()));
        }
    };
    if let Some(model) = &settings.formatting.ollama_model {
        add(model, "Formatting");
    }
    if let Some(model) = &settings.search.embedding_model {
        add(model, "Search");
    }
    if assistant.provider.kind == ProviderKind::Ollama {
        add(&assistant.provider.model, "Meetings");
        add(&assistant.provider.model, "Investigations");
    }
    add(&assistant.fallback_model, "Meeting fallback");
    add(&assistant.translation_model, "Live translation");
    for action in &assistant.actions {
        if action.payload_mode != ActionPayloadMode::Template {
            add(&action.payload_model, &format!("Action: {}", action.name));
        }
    }
    let file_name = |path: &Path| {
        path.file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default()
    };
    add(
        &file_name(&settings.recognition.model_path),
        "Dictation (Accurate)",
    );
    add(
        &file_name(&settings.recognition.instant_model_path),
        "Dictation (Instant)",
    );
    uses
}

fn used_by(uses: &[(String, String)], name: &str, family: &str) -> Vec<String> {
    let mut features: Vec<String> = uses
        .iter()
        .filter(|(model, _)| {
            if family == "ollama" {
                same_model(model, name)
            } else {
                model == name
            }
        })
        .map(|(_, feature)| feature.clone())
        .collect();
    features.dedup();
    features
}

/// Ollama capabilities by model digest; a digest never changes its contents.
static CAPABILITIES: LazyLock<Mutex<HashMap<String, Vec<String>>>> = LazyLock::new(Mutex::default);

fn agent(timeout: Option<Duration>) -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(2)))
        .timeout_global(timeout)
        .max_redirects(0)
        .build()
        .new_agent()
}

/// What Ollama says the model can do (`completion`, `embedding`, ...).
fn capabilities(name: &str) -> Option<Vec<String>> {
    let body = serde_json::json!({ "model": name }).to_string();
    let text = agent(Some(Duration::from_secs(5)))
        .post(format!("{OLLAMA}/api/show"))
        .header("Content-Type", "application/json")
        .send(body)
        .ok()?
        .body_mut()
        .with_config()
        .limit(8 * 1024 * 1024)
        .read_to_string()
        .ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    Some(
        value["capabilities"]
            .as_array()?
            .iter()
            .filter_map(|capability| capability.as_str().map(str::to_owned))
            .collect(),
    )
}

/// The whole list: installed first, then curated suggestions.
pub fn list(store: &SettingsStore) -> Vec<RegistryEntry> {
    let settings = store.load().unwrap_or_default();
    let directory = store
        .path()
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default();
    let assistant = ConfigStore::new(directory.join("assistant.json"))
        .load()
        .unwrap_or_default();
    let uses = uses(&settings, &assistant);
    let mut entries = Vec::new();

    let installed = OllamaClient::new(
        OllamaEndpoint::default(),
        ClientTimeouts {
            connect: Duration::from_millis(500),
            response_headers: Duration::from_secs(3),
            response_body: Duration::from_secs(3),
            overall: Duration::from_secs(4),
        },
    )
    .ok()
    .and_then(|client| client.discover(&CancellationToken::new()).ok());
    if let Some(catalog) = &installed {
        for model in catalog.models() {
            let name = model.name.as_str().to_owned();
            let key = model.digest.clone().unwrap_or_else(|| name.clone());
            let known = CAPABILITIES
                .lock()
                .ok()
                .and_then(|cache| cache.get(&key).cloned());
            let found = known.or_else(|| {
                let found = capabilities(&name)?;
                if let Ok(mut cache) = CAPABILITIES.lock() {
                    cache.insert(key, found.clone());
                }
                Some(found)
            });
            entries.push(RegistryEntry {
                used_by: used_by(&uses, &name, "ollama"),
                tags: found
                    .map(|found| kinds_from_capabilities(&found))
                    .unwrap_or_default(),
                name,
                family: "ollama",
                installed: true,
                size_bytes: model.size,
                note: String::new(),
            });
        }
    }
    for (name, tags, note) in CURATED {
        if entries
            .iter()
            .any(|entry| entry.family == "ollama" && same_model(&entry.name, name))
        {
            continue;
        }
        entries.push(RegistryEntry {
            name: (*name).to_owned(),
            family: "ollama",
            tags: tags.to_vec(),
            installed: false,
            size_bytes: None,
            used_by: used_by(&uses, name, "ollama"),
            note: (*note).to_owned(),
        });
    }

    let models = directory.join("models");
    for spec in crate::model::pinned_models().unwrap_or_default() {
        let path = store.resolve_model_path(&PathBuf::from("models").join(&spec.file_name));
        entries.push(RegistryEntry {
            installed: path.is_file(),
            size_bytes: Some(spec.bytes),
            used_by: used_by(&uses, &spec.file_name, "whisper"),
            note: format!(
                "Accurate recognition · {}",
                if spec.language == "en" {
                    "English"
                } else {
                    "many languages"
                }
            ),
            name: spec.file_name,
            family: "whisper",
            tags: vec![SPEECH],
        });
    }
    for choice in crate::vosk_models::MODELS {
        let selected = settings
            .recognition
            .instant_model_path
            .ends_with(choice.directory);
        entries.push(RegistryEntry {
            name: choice.directory.to_owned(),
            family: "vosk",
            tags: vec![SPEECH],
            installed: selected || models.join(choice.directory).is_dir(),
            size_bytes: Some(choice.bytes),
            used_by: used_by(&uses, choice.directory, "vosk"),
            note: format!("Instant recognition · {}", choice.label),
        });
    }
    let ocr_installed = verify_installed(&directory.join(OCR_MODEL_DIRECTORY));
    entries.push(RegistryEntry {
        name: "PaddleOCR latin".into(),
        family: "ocr",
        tags: vec![OCR],
        installed: ocr_installed,
        size_bytes: Some(crate::investigation_ocr::total_download_bytes()),
        used_by: if ocr_installed {
            vec!["Investigations".into()]
        } else {
            Vec::new()
        },
        note: "Reads text from screenshots in Investigations.".into(),
    });
    entries
}

static PULL: LazyLock<Mutex<PullView>> = LazyLock::new(|| {
    Mutex::new(PullView {
        state: "idle",
        ..PullView::default()
    })
});

pub fn pull_status() -> PullView {
    PULL.lock().map(|view| view.clone()).unwrap_or_default()
}

fn set_pull(update: impl FnOnce(&mut PullView)) {
    if let Ok(mut view) = PULL.lock() {
        update(&mut view);
    }
}

/// An Ollama name such as `qwen2.5:7b` or `library/model:tag`: letters,
/// digits and `._-/:`, no spaces, and never a cloud model.
pub fn checked_name(name: &str) -> Result<String, String> {
    let name = name.trim();
    let valid = !name.is_empty()
        && name.len() <= 200
        && name
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphanumeric())
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '/' | ':'))
        && name.matches(':').count() <= 1
        && ModelName::parse(name.to_owned()).is_ok();
    if !valid {
        return Err("That is not a valid model name, for example qwen2.5:7b.".into());
    }
    let lower = name.to_ascii_lowercase();
    if lower.ends_with(":cloud") || lower.ends_with("-cloud") {
        return Err(
            "Cloud models run outside this computer; Phorminx installs local models only.".into(),
        );
    }
    Ok(name.to_owned())
}

/// Starts installing `name` from Ollama. `expected` is the kind the caller
/// needs; a model that turns out not to be that kind is reported.
pub fn start_pull(name: &str, expected: Option<&str>) -> Result<PullView, String> {
    let name = checked_name(name)?;
    let expected = expected
        .filter(|kind| [LANGUAGE, EMBEDDING, VISION, TOOLS].contains(kind))
        .map(str::to_owned);
    {
        let mut view = PULL.lock().map_err(|_| "Installing is unavailable.")?;
        if view.state == "pulling" {
            return Err(format!(
                "Still installing {}. Wait for it to finish.",
                view.name
            ));
        }
        *view = PullView {
            name: name.clone(),
            state: "pulling",
            percent: Some(0),
            message: format!("Installing {name}…"),
        };
    }
    std::thread::Builder::new()
        .name("phorminx-model-pull".into())
        .spawn(move || {
            let result = pull(&name);
            let message = match result {
                Err(error) => {
                    set_pull(|view| {
                        view.state = "failed";
                        view.message = error;
                    });
                    return;
                }
                Ok(()) => match (capabilities(&name), &expected) {
                    (Some(found), Some(expected)) if !kinds_from_capabilities(&found).contains(&expected.as_str()) => {
                        format!("{name} is installed, but it is not a {expected} model, so it will not appear where a {expected} model is needed.")
                    }
                    _ => format!("{name} is installed."),
                },
            };
            set_pull(|view| {
                view.state = "done";
                view.percent = Some(100);
                view.message = message;
            });
        })
        .map_err(|_| "Installing could not start.".to_owned())?;
    Ok(pull_status())
}

/// Ollama's pull, streamed for progress.
fn pull(name: &str) -> Result<(), String> {
    let body = serde_json::json!({ "model": name, "stream": true }).to_string();
    let mut response = agent(None)
        .post(format!("{OLLAMA}/api/pull"))
        .header("Content-Type", "application/json")
        .send(body)
        .map_err(|_| {
            "Ollama is not running. Start it, or run setup again from Setup & repair.".to_owned()
        })?;
    let reader = BufReader::new(response.body_mut().as_reader());
    for line in reader.lines() {
        let line = line.map_err(|_| "The download was interrupted. Try again.".to_owned())?;
        let Ok(event) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        if let Some(error) = event["error"].as_str() {
            return Err(
                if error.contains("not found") || error.contains("does not exist") {
                    format!("Ollama has no model named {name}. Check the name on ollama.com.")
                } else {
                    format!("Ollama could not install {name}: {error}")
                },
            );
        }
        let status = event["status"].as_str().unwrap_or_default().to_owned();
        let percent = match (event["completed"].as_u64(), event["total"].as_u64()) {
            (Some(done), Some(total)) if total > 0 => {
                Some((done.saturating_mul(100) / total).min(99) as u8)
            }
            _ => None,
        };
        set_pull(|view| {
            if percent.is_some() {
                view.percent = percent;
            }
            if !status.is_empty() {
                view.message = format!("Installing {name}: {status}");
            }
        });
        if status == "success" {
            return Ok(());
        }
    }
    Err("The download ended early. Try again.".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use phorminx_assistant::ActionConfig;

    #[test]
    fn capabilities_become_kinds() {
        let caps = |names: &[&str]| {
            names
                .iter()
                .map(|name| (*name).to_owned())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            kinds_from_capabilities(&caps(&["completion", "vision"])),
            [LANGUAGE, VISION]
        );
        assert_eq!(kinds_from_capabilities(&caps(&["embedding"])), [EMBEDDING]);
        assert_eq!(
            kinds_from_capabilities(&caps(&["thinking", "completion", "completion"])),
            [LANGUAGE]
        );
    }

    #[test]
    fn used_by_names_every_feature_that_uses_a_model() {
        let mut settings = Settings::default();
        settings.formatting.ollama_model = Some("qwen2.5:7b".into());
        settings.search.embedding_model = Some("nomic-embed-text".into());
        let mut assistant = AssistantConfig::default();
        assistant.provider.model = "gemma3:4b".into();
        assistant.translation_model = "qwen2.5:7b".into();
        assistant.actions.push(ActionConfig {
            name: "Summarize".into(),
            payload_mode: ActionPayloadMode::AiJson,
            payload_model: "gemma3:4b".into(),
            ..ActionConfig::default()
        });
        let uses = uses(&settings, &assistant);
        assert_eq!(
            used_by(&uses, "qwen2.5:7b", "ollama"),
            ["Formatting", "Live translation"]
        );
        assert_eq!(
            used_by(&uses, "nomic-embed-text:latest", "ollama"),
            ["Search"]
        );
        assert_eq!(
            used_by(&uses, "gemma3:4b", "ollama"),
            ["Meetings", "Investigations", "Action: Summarize"]
        );
        assert_eq!(
            used_by(&uses, "ggml-base.en.bin", "whisper"),
            ["Dictation (Accurate)"]
        );
        assert!(used_by(&uses, "qwen2.5:1.5b", "ollama").is_empty());
    }

    #[test]
    fn external_assistants_do_not_mark_local_models() {
        let mut assistant = AssistantConfig::default();
        assistant.provider.kind = ProviderKind::OpenAi;
        assistant.provider.model = "qwen2.5:7b".into();
        let uses = uses(&Settings::default(), &assistant);
        assert!(used_by(&uses, "qwen2.5:7b", "ollama").is_empty());
    }

    #[test]
    fn names_are_checked_before_anything_is_pulled() {
        for bad in [
            "not a name!",
            "",
            " ",
            "-flag",
            "a b",
            "x:y:z",
            "evil;calc",
            "../model",
        ] {
            assert!(checked_name(bad).is_err(), "{bad:?}");
        }
        assert!(
            checked_name("gpt-oss:120b-cloud")
                .unwrap_err()
                .contains("Cloud")
        );
        assert_eq!(checked_name(" qwen2.5:7b ").unwrap(), "qwen2.5:7b");
        assert!(checked_name("library/gemma3:4b").is_ok());
        assert!(start_pull("not a name!", None).is_err());
        assert!(
            CURATED
                .iter()
                .all(|(name, tags, _)| checked_name(name).is_ok() && !tags.is_empty())
        );
    }
}
