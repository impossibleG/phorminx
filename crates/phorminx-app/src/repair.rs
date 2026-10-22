//! The repair checklist on Setup & repair: one line per workflow saying
//! whether what it needs is installed, and how to put it back. "Run setup
//! again" (the bundled starter script) reinstalls the starter pieces;
//! screenshot text models have their own installer; chosen models that are
//! missing point to Models.

use std::path::Path;
use std::time::Duration;

use phorminx_assistant::{ConfigStore, ProviderKind};
use phorminx_ollama::{CancellationToken, ClientTimeouts, OllamaClient, OllamaEndpoint};
use serde::Serialize;

use crate::investigation_ocr::{OCR_MODEL_DIRECTORY, verify_installed};
use crate::model_registry::same_model;
use crate::settings::{RecognitionMode, SettingsStore};

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RepairItem {
    pub id: &'static str,
    /// The feature that needs it, e.g. "Dictation".
    pub workflow: &'static str,
    pub label: String,
    /// `ok`, `missing` or `unused` (not set up, nothing to repair).
    pub state: &'static str,
    pub detail: String,
    /// How to fix it: `setup` (run setup again), `ocr` or `models`.
    pub fix: Option<&'static str>,
}

/// What the checks found, gathered separately so the rules can be tested.
#[derive(Clone, Debug, Default)]
pub struct RepairInputs {
    pub instant_selected: bool,
    pub instant_ready: bool,
    pub accurate_selected: bool,
    pub accurate_ready: bool,
    pub vulkan_ready: bool,
    /// Installed Ollama models, or `None` when Ollama did not answer.
    pub ollama_models: Option<Vec<String>>,
    pub formatting_model: Option<String>,
    pub embedding_model: Option<String>,
    pub assistant_model: Option<String>,
    pub ocr_ready: bool,
}

fn item(
    id: &'static str,
    workflow: &'static str,
    label: &str,
    state: &'static str,
    detail: String,
    fix: Option<&'static str>,
) -> RepairItem {
    RepairItem {
        id,
        workflow,
        label: label.to_owned(),
        state,
        detail,
        fix: if state == "missing" { fix } else { None },
    }
}

pub fn evaluate(inputs: &RepairInputs) -> Vec<RepairItem> {
    let mut items = Vec::new();
    let speech = |selected: bool, ready: bool, what: &str| match (ready, selected) {
        (true, _) => ("ok", format!("{what} is installed.")),
        (false, true) => (
            "missing",
            format!("{what} is selected but its files are missing."),
        ),
        (false, false) => (
            "unused",
            format!("{what} is not installed; dictation uses the other engine."),
        ),
    };
    let (state, detail) = speech(
        inputs.instant_selected,
        inputs.instant_ready,
        "Instant recognition (Vosk)",
    );
    items.push(item(
        "instant",
        "Dictation",
        "Instant recognition",
        state,
        detail,
        Some("setup"),
    ));
    let (state, detail) = speech(
        inputs.accurate_selected,
        inputs.accurate_ready,
        "Accurate recognition (Whisper)",
    );
    items.push(item(
        "accurate",
        "Dictation",
        "Accurate recognition",
        state,
        detail,
        Some("setup"),
    ));
    items.push(if inputs.vulkan_ready {
        item(
            "vulkan",
            "Dictation",
            "GPU acceleration",
            "ok",
            "The Vulkan runtime is installed.".into(),
            None,
        )
    } else {
        item(
            "vulkan",
            "Dictation",
            "GPU acceleration",
            "missing",
            "The Vulkan runtime is missing, so Accurate runs on the CPU.".into(),
            Some("setup"),
        )
    });

    let Some(installed) = &inputs.ollama_models else {
        items.push(item(
            "ollama",
            "Formatting, meetings, search",
            "Local language models (Ollama)",
            "missing",
            "Ollama is not installed or not running.".into(),
            Some("setup"),
        ));
        items.push(item(
            "ocr",
            "Investigations",
            "Screenshot text recognition",
            if inputs.ocr_ready { "ok" } else { "unused" },
            ocr_detail(inputs.ocr_ready),
            Some("ocr"),
        ));
        return items;
    };
    items.push(item(
        "ollama",
        "Formatting, meetings, search",
        "Local language models (Ollama)",
        "ok",
        format!(
            "Ollama is running with {} model{}.",
            installed.len(),
            if installed.len() == 1 { "" } else { "s" }
        ),
        None,
    ));
    let has = |model: &str| installed.iter().any(|name| same_model(name, model));
    let chosen = |id: &'static str,
                  workflow: &'static str,
                  label: &str,
                  model: &Option<String>,
                  starter: bool| match model {
        Some(model) if has(model) => item(
            id,
            workflow,
            label,
            "ok",
            format!("{model} is installed."),
            None,
        ),
        // Run setup again installs the starter models; anything else is reinstalled from Models.
        Some(model) => item(
            id,
            workflow,
            label,
            "missing",
            format!("{model} is selected but not installed."),
            Some(if starter { "setup" } else { "models" }),
        ),
        None => item(
            id,
            workflow,
            label,
            "unused",
            "No model chosen.".into(),
            None,
        ),
    };
    let starter = |model: &Option<String>, name: &str| {
        model
            .as_deref()
            .is_some_and(|model| same_model(model, name))
    };
    items.push(chosen(
        "formatting",
        "Formatting",
        "Formatting model",
        &inputs.formatting_model,
        starter(&inputs.formatting_model, "qwen2.5:1.5b"),
    ));
    items.push(chosen(
        "embedding",
        "Search",
        "Search model",
        &inputs.embedding_model,
        starter(&inputs.embedding_model, "nomic-embed-text:latest"),
    ));
    items.push(chosen(
        "assistant",
        "Meetings and investigations",
        "Assistant model",
        &inputs.assistant_model,
        false,
    ));
    items.push(item(
        "ocr",
        "Investigations",
        "Screenshot text recognition",
        if inputs.ocr_ready { "ok" } else { "unused" },
        ocr_detail(inputs.ocr_ready),
        Some("ocr"),
    ));
    items
}

fn ocr_detail(ready: bool) -> String {
    if ready {
        "The screenshot text models are installed.".into()
    } else {
        "Optional: install the screenshot text models to read text from screenshots.".into()
    }
}

/// Runs every check on this computer. Ollama gets a short timeout so a
/// missing service does not hold the page.
pub fn check(store: &SettingsStore) -> Vec<RepairItem> {
    let settings = store.load().unwrap_or_default();
    let recognition = &settings.recognition;
    let instant_ready = matches!(
        phorminx_vosk::validate_asset_layout(
            &store.resolve_asset_path(&recognition.instant_runtime_path),
            &store.resolve_asset_path(&recognition.instant_model_path),
            &recognition.language,
        ),
        phorminx_vosk::AssetLayout::Present { .. }
    );
    let accurate_ready = store.resolve_model_path(&recognition.model_path).is_file();
    let vulkan_ready = std::env::var_os("WINDIR").is_some_and(|windows| {
        Path::new(&windows)
            .join("System32")
            .join("vulkan-1.dll")
            .is_file()
    });
    let ollama_models = OllamaClient::new(
        OllamaEndpoint::default(),
        ClientTimeouts {
            connect: Duration::from_millis(500),
            response_headers: Duration::from_secs(2),
            response_body: Duration::from_secs(2),
            overall: Duration::from_secs(3),
        },
    )
    .ok()
    .and_then(|client| client.discover(&CancellationToken::new()).ok())
    .map(|catalog| {
        catalog
            .models()
            .iter()
            .map(|model| model.name.as_str().to_owned())
            .collect()
    });
    let directory = store
        .path()
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default();
    let assistant_model = ConfigStore::new(directory.join("assistant.json"))
        .load()
        .ok()
        .filter(|config| {
            config.provider.kind == ProviderKind::Ollama && !config.provider.model.trim().is_empty()
        })
        .map(|config| config.provider.model);
    evaluate(&RepairInputs {
        instant_selected: recognition.mode == RecognitionMode::Instant,
        instant_ready,
        accurate_selected: recognition.mode == RecognitionMode::Accurate,
        accurate_ready,
        vulkan_ready,
        ollama_models,
        formatting_model: settings.formatting.ollama_model.clone(),
        embedding_model: settings.search.embedding_model.clone(),
        assistant_model,
        ocr_ready: verify_installed(&directory.join(OCR_MODEL_DIRECTORY)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn healthy() -> RepairInputs {
        RepairInputs {
            instant_selected: true,
            instant_ready: true,
            accurate_selected: false,
            accurate_ready: true,
            vulkan_ready: true,
            ollama_models: Some(vec![
                "qwen2.5:1.5b".into(),
                "nomic-embed-text:latest".into(),
                "gemma3:4b".into(),
            ]),
            formatting_model: Some("qwen2.5:1.5b".into()),
            embedding_model: Some("nomic-embed-text".into()),
            assistant_model: Some("gemma3:4b".into()),
            ocr_ready: true,
        }
    }

    fn state<'a>(items: &'a [RepairItem], id: &str) -> (&'a str, Option<&'a str>) {
        let item = items.iter().find(|item| item.id == id).unwrap();
        (item.state, item.fix)
    }

    #[test]
    fn a_healthy_setup_has_nothing_to_fix() {
        let items = evaluate(&healthy());
        assert!(items.iter().all(|item| item.state == "ok"), "{items:?}");
        assert!(items.iter().all(|item| item.fix.is_none()));
    }

    #[test]
    fn deleted_models_point_to_the_right_fix() {
        let mut inputs = healthy();
        inputs.instant_ready = false;
        inputs.ollama_models = Some(vec!["gemma3:4b".into()]);
        inputs.assistant_model = Some("llama3:8b".into());
        inputs.vulkan_ready = false;
        let items = evaluate(&inputs);
        assert_eq!(state(&items, "instant"), ("missing", Some("setup")));
        assert_eq!(state(&items, "formatting"), ("missing", Some("setup")));
        assert_eq!(state(&items, "embedding"), ("missing", Some("setup")));
        assert_eq!(state(&items, "assistant"), ("missing", Some("models")));
        assert_eq!(state(&items, "vulkan"), ("missing", Some("setup")));
    }

    #[test]
    fn unused_pieces_are_not_errors() {
        let mut inputs = healthy();
        inputs.accurate_ready = false;
        inputs.formatting_model = None;
        inputs.ocr_ready = false;
        let items = evaluate(&inputs);
        assert_eq!(state(&items, "accurate"), ("unused", None));
        assert_eq!(state(&items, "formatting"), ("unused", None));
        assert_eq!(state(&items, "ocr"), ("unused", None));
    }

    #[test]
    fn without_ollama_only_setup_can_help() {
        let mut inputs = healthy();
        inputs.ollama_models = None;
        let items = evaluate(&inputs);
        assert_eq!(state(&items, "ollama"), ("missing", Some("setup")));
        assert!(!items.iter().any(|item| item.id == "formatting"));
        assert!(same_model("nomic-embed-text", "nomic-embed-text:latest"));
        assert!(!same_model("qwen2.5:1.5b", "qwen2.5:7b"));
    }
}
