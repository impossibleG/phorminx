//! A deliberately narrow, serializable boundary for the replacement desktop UI.
//!
//! The existing egui shell remains operational during migration. Do not send
//! `WorkspaceSnapshot` itself to a webview: it contains provider and action
//! drafts whose secret fields must never be included in generic state events.

use phorminx_ui::workspace::{
    CaptureState, ChatLine, CompanionView, MeetingLine, SavedMeeting, TranslationLine,
    TranslationView, WorkspaceSnapshot,
};
use serde::Serialize;

use crate::ui_bridge::{UiReadinessState, UiSnapshot};

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppView {
    pub history_enabled: bool,
    pub history: Vec<HistoryItemView>,
    pub lexicon: Vec<LexiconItemView>,
    pub profiles: Vec<ProfileItemView>,
    pub settings: AppSettingsView,
    pub systems: Vec<SystemView>,
    pub ollama_models: Vec<OllamaModelView>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OllamaModelView {
    pub name: String,
    pub size_bytes: Option<u64>,
    pub selected: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetupView {
    pub stage: String,
    pub summary: String,
    pub can_start: bool,
    pub actions: Vec<SetupActionView>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetupActionView {
    pub id: String,
    pub title: String,
    pub detail: String,
    pub progress_percent: Option<u8>,
    pub consent: Vec<String>,
    pub running: bool,
    pub can_retry: bool,
    pub complete: bool,
    pub available: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LibrarySearchView {
    pub generation: u64,
    pub query: String,
    pub status: String,
    pub failed: bool,
    pub semantic_available: bool,
    pub hits: Vec<LibraryHitView>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LibraryHitView {
    pub history_id: i64,
    pub created_at_ms: i64,
    pub passage: String,
    pub score: f32,
    pub semantic: bool,
}

impl From<crate::library_search::LibrarySearchUpdate> for LibrarySearchView {
    fn from(value: crate::library_search::LibrarySearchUpdate) -> Self {
        Self {
            generation: value.generation,
            query: value.query,
            status: value.status,
            failed: value.failed,
            semantic_available: value.semantic_available,
            hits: value
                .hits
                .into_iter()
                .map(|hit| LibraryHitView {
                    history_id: hit.history_id,
                    created_at_ms: hit.created_at_ms,
                    passage: hit.passage,
                    score: hit.score,
                    semantic: hit.semantic,
                })
                .collect(),
        }
    }
}

impl From<&phorminx_ui::SetupSnapshot> for SetupView {
    fn from(value: &phorminx_ui::SetupSnapshot) -> Self {
        Self {
            stage: value.stage.label().to_owned(),
            summary: value.summary.clone(),
            can_start: false,
            actions: value
                .actions
                .iter()
                .map(|action| SetupActionView {
                    id: action.id.clone(),
                    title: action.title.clone(),
                    detail: action.detail.clone(),
                    progress_percent: action.progress_percent,
                    consent: action.consent.clone(),
                    running: action.running,
                    can_retry: action.can_retry,
                    complete: action.complete,
                    available: action.available,
                })
                .collect(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryItemView {
    pub id: i64,
    pub created_at_ms: i64,
    pub preview: String,
    pub truncated: bool,
    pub variants: HistoryVariantsView,
    pub language: Option<String>,
    pub target_app: Option<String>,
    pub duration_ms: Option<u64>,
    /// Why AI formatting did not apply, when it did not.
    pub formatting_note: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryVariantsView {
    pub output: bool,
    pub raw: bool,
    pub normalized: bool,
    pub cleaned: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LexiconItemView {
    pub id: i64,
    pub canonical: String,
    pub alias: String,
    pub language: Option<String>,
    pub app_executable: Option<String>,
    pub enabled: bool,
    pub case_policy: &'static str,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileItemView {
    pub executable: String,
    pub summary: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileDetailView {
    pub executable: String,
    pub formatting_style: &'static str,
    pub custom_instructions: Option<String>,
    pub language: Option<String>,
    pub insertion_preference: &'static str,
    pub deny: bool,
}

impl From<&crate::ui_bridge::UiProfileItem> for ProfileDetailView {
    fn from(value: &crate::ui_bridge::UiProfileItem) -> Self {
        Self {
            executable: value.executable.clone(),
            formatting_style: match value.formatting_style {
                phorminx_persistence::FormattingStyle::Raw => "raw",
                phorminx_persistence::FormattingStyle::Light => "light",
                phorminx_persistence::FormattingStyle::Balanced => "balanced",
                phorminx_persistence::FormattingStyle::Strong => "strong",
                phorminx_persistence::FormattingStyle::Structured => "structured",
                phorminx_persistence::FormattingStyle::Custom => "custom",
            },
            custom_instructions: value.custom_instructions.clone(),
            language: value.language.clone(),
            insertion_preference: match value.insertion_preference {
                phorminx_persistence::InsertionPreference::Automatic => "automatic",
                phorminx_persistence::InsertionPreference::Direct => "direct",
                phorminx_persistence::InsertionPreference::Clipboard => "clipboard",
            },
            deny: value.deny,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppSettingsView {
    pub recognition_mode: String,
    pub accurate_model: String,
    pub accurate_backend: String,
    pub language: String,
    pub minimum_rms: String,
    pub microphone: Option<String>,
    pub formatting_strength: String,
    pub formatting_model: Option<String>,
    pub model_residency: String,
    pub formatting_passes: u8,
    pub recording_mode: String,
    pub launcher_shortcut: String,
    pub show_live_text: bool,
    pub direct_shortcut: Option<String>,
    pub theme: String,
    pub history_retention: String,
    pub launch_at_login: bool,
    pub embedding_model: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SystemView {
    pub name: &'static str,
    pub state: &'static str,
    pub message: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActionItemView {
    pub id: String,
    pub name: String,
    pub launcher_slot: Option<u8>,
    pub method: String,
    pub endpoint: String,
    pub has_credential: bool,
    pub header_names: Vec<String>,
    pub payload_template: String,
    pub payload_mode: &'static str,
    pub payload_model: String,
    pub payload_prompt: String,
}

impl From<&phorminx_ui::workspace::ActionDraft> for ActionItemView {
    fn from(value: &phorminx_ui::workspace::ActionDraft) -> Self {
        Self {
            id: value.id.clone(),
            name: value.name.clone(),
            launcher_slot: value.launcher_slot,
            method: value.method.clone(),
            endpoint: value.endpoint.clone(),
            has_credential: value.has_credential,
            header_names: value.header_names.clone(),
            payload_template: value.payload_template.clone(),
            payload_mode: match value.payload_mode {
                phorminx_ui::workspace::ActionPayloadMode::Template => "template",
                phorminx_ui::workspace::ActionPayloadMode::AiJson => "ai-json",
            },
            payload_model: value.payload_model.clone(),
            payload_prompt: value.payload_prompt.clone(),
        }
    }
}

fn readiness_state(state: UiReadinessState) -> &'static str {
    match state {
        UiReadinessState::Checking => "checking",
        UiReadinessState::Ready => "ready",
        UiReadinessState::NeedsAttention => "needs-attention",
    }
}

impl From<&UiSnapshot> for AppView {
    fn from(value: &UiSnapshot) -> Self {
        let settings = &value.settings.values;
        Self {
            history_enabled: value.history_enabled,
            history: value
                .history
                .iter()
                .map(|item| HistoryItemView {
                    id: item.id,
                    created_at_ms: item.created_at_ms,
                    preview: item.selected_output_preview.clone(),
                    truncated: item.preview_truncated,
                    variants: HistoryVariantsView {
                        output: item.variants.output,
                        raw: item.variants.raw,
                        normalized: item.variants.normalized,
                        cleaned: item.variants.cleaned,
                    },
                    language: item.language.clone(),
                    target_app: item.target_executable.clone(),
                    duration_ms: item.audio_duration_ms,
                    formatting_note: crate::review::formatting_summary(&item.warnings),
                })
                .collect(),
            lexicon: value
                .lexicon
                .iter()
                .map(|item| LexiconItemView {
                    id: item.id,
                    canonical: item.canonical.clone(),
                    alias: item.alias.clone(),
                    language: item.language.clone(),
                    app_executable: item.app_executable.clone(),
                    enabled: item.enabled,
                    case_policy: match item.case_policy {
                        phorminx_persistence::CasePolicy::PreserveInput => "preserve-input",
                        phorminx_persistence::CasePolicy::UseCanonical => "canonical",
                        phorminx_persistence::CasePolicy::Lowercase => "lowercase",
                        phorminx_persistence::CasePolicy::Uppercase => "uppercase",
                    },
                })
                .collect(),
            profiles: value
                .profiles
                .iter()
                .map(|item| ProfileItemView {
                    executable: item.executable.clone(),
                    summary: item.summary(),
                })
                .collect(),
            settings: AppSettingsView {
                recognition_mode: format!("{:?}", settings.recognition.mode).to_lowercase(),
                accurate_model: format!("{:?}", settings.recognition.accurate_model),
                accurate_backend: format!("{:?}", settings.recognition.accurate_backend)
                    .to_lowercase(),
                language: settings.recognition.language.clone(),
                minimum_rms: settings.recognition.minimum_rms.to_string(),
                microphone: settings.recognition.microphone.clone(),
                formatting_strength: settings.formatting.strength.to_string(),
                formatting_model: settings.formatting.ollama_model.clone(),
                model_residency: format!("{:?}", settings.formatting.ollama_lifecycle)
                    .to_lowercase(),
                formatting_passes: settings.formatting.passes,
                recording_mode: format!("{:?}", settings.interaction.recording_mode).to_lowercase(),
                launcher_shortcut: settings.interaction.launcher_shortcut.clone(),
                show_live_text: settings.interaction.show_live_text,
                direct_shortcut: settings.interaction.direct_dictation_shortcut.clone(),
                theme: format!("{:?}", settings.appearance.theme).to_lowercase(),
                history_retention: match settings.privacy.history_retention {
                    crate::settings::HistoryRetention::Disabled => "disabled",
                    crate::settings::HistoryRetention::OneDay => "one-day",
                    crate::settings::HistoryRetention::SevenDays => "seven-days",
                    crate::settings::HistoryRetention::ThirtyDays => "thirty-days",
                    crate::settings::HistoryRetention::Indefinite => "indefinite",
                }
                .into(),
                launch_at_login: settings.startup.launch_at_login,
                embedding_model: settings.search.embedding_model.clone(),
            },
            systems: vec![
                SystemView {
                    name: "Microphone",
                    state: readiness_state(value.readiness.microphone.state),
                    message: value.readiness.microphone.message.clone(),
                },
                SystemView {
                    name: "Whisper",
                    state: readiness_state(value.readiness.whisper.state),
                    message: value.readiness.whisper.message.clone(),
                },
                SystemView {
                    name: "Vosk",
                    state: readiness_state(value.readiness.vosk.state),
                    message: value.readiness.vosk.message.clone(),
                },
                SystemView {
                    name: "Ollama",
                    state: readiness_state(value.readiness.ollama.state),
                    message: value.readiness.ollama.message.clone(),
                },
            ],
            ollama_models: value
                .readiness
                .ollama
                .models
                .iter()
                .map(|model| OllamaModelView {
                    name: model.name.clone(),
                    size_bytes: model.size_bytes,
                    selected: value.readiness.ollama.selected.as_ref() == Some(&model.name),
                })
                .collect(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeetingView {
    pub revision: u64,
    pub action_revision: u64,
    pub provider_revision: u64,
    pub session_id: Option<i64>,
    pub saved_session: bool,
    pub history_enabled: bool,
    pub sessions: Vec<SessionItemView>,
    pub title: String,
    pub capture: &'static str,
    pub captured_samples: u64,
    pub committed_samples: u64,
    pub microphone_muted: bool,
    pub computer_audio_muted: bool,
    pub auto_commentary_enabled: bool,
    pub companion_view: &'static str,
    pub companion_translation_view: &'static str,
    pub companion_split_fraction: u16,
    pub translation_language: String,
    pub translation_model: String,
    pub translation_notice: String,
    pub input_devices: Vec<String>,
    pub output_devices: Vec<String>,
    pub available_models: Vec<String>,
    pub selected_input_device: Option<String>,
    pub selected_output_device: Option<String>,
    pub provisional_text: String,
    pub transcript: Vec<TranscriptLineView>,
    pub translations: Vec<TranslationLineView>,
    pub messages: Vec<ChatLineView>,
    pub assistant_busy: bool,
    pub active_answer_model: String,
    pub notice: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderView {
    pub kind: &'static str,
    pub model: String,
    pub fallback_model: String,
    pub endpoint: String,
    pub has_credential: bool,
    pub preset: String,
    pub revision: u64,
}

impl ProviderView {
    pub fn from_workspace(value: &WorkspaceSnapshot) -> Self {
        Self {
            kind: match value.provider.kind {
                phorminx_ui::workspace::ProviderChoice::Ollama => "ollama",
                phorminx_ui::workspace::ProviderChoice::OpenAi => "openai",
                phorminx_ui::workspace::ProviderChoice::Anthropic => "anthropic",
            },
            model: value.provider.model.clone(),
            fallback_model: value.provider.fallback_model.clone(),
            endpoint: value.provider.endpoint.clone(),
            has_credential: value.provider.has_credential,
            preset: value.provider.preset.clone(),
            revision: value.provider_revision,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptLineView {
    pub start_sample: u64,
    pub end_sample: u64,
    pub source: &'static str,
    pub text: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionItemView {
    pub id: i64,
    pub title: String,
    pub source: String,
}

impl From<&SavedMeeting> for SessionItemView {
    fn from(value: &SavedMeeting) -> Self {
        Self {
            id: value.id,
            title: value.title.clone(),
            source: value.source.clone(),
        }
    }
}

impl From<&MeetingLine> for TranscriptLineView {
    fn from(value: &MeetingLine) -> Self {
        let (source, text) = if let Some((prefix, content)) = value.text.split_once("] ") {
            if prefix.starts_with('[') && prefix.ends_with(" · Computer audio") {
                ("computer", content)
            } else if prefix.starts_with('[') && prefix.ends_with(" · You / microphone") {
                ("microphone", content)
            } else {
                ("conversation", value.text.as_str())
            }
        } else {
            ("conversation", value.text.as_str())
        };
        Self {
            start_sample: value.start_sample,
            end_sample: value.end_sample,
            source,
            text: text.to_owned(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TranslationLineView {
    pub start_sample: u64,
    pub end_sample: u64,
    pub language: String,
    pub text: String,
}

impl From<&TranslationLine> for TranslationLineView {
    fn from(value: &TranslationLine) -> Self {
        Self {
            start_sample: value.start_sample,
            end_sample: value.end_sample,
            language: value.language.clone(),
            text: value.text.clone(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatLineView {
    pub role: String,
    pub text: String,
    pub status: String,
}

impl From<&ChatLine> for ChatLineView {
    fn from(value: &ChatLine) -> Self {
        Self {
            role: value.role.clone(),
            text: value.text.clone(),
            status: value.status.clone(),
        }
    }
}

impl From<&WorkspaceSnapshot> for MeetingView {
    fn from(value: &WorkspaceSnapshot) -> Self {
        Self {
            revision: value.revision,
            action_revision: value.action_revision,
            provider_revision: value.provider_revision,
            session_id: value.session_id,
            saved_session: value.saved_session,
            history_enabled: value.history_enabled,
            sessions: value.sessions.iter().map(Into::into).collect(),
            title: value.title.clone(),
            capture: match value.capture {
                CaptureState::Idle => "idle",
                CaptureState::Starting => "starting",
                CaptureState::Listening => "listening",
                CaptureState::Stopping => "stopping",
            },
            captured_samples: value.captured_samples,
            committed_samples: value.committed_samples,
            microphone_muted: value.microphone_muted,
            computer_audio_muted: value.computer_audio_muted,
            auto_commentary_enabled: value.auto_commentary_enabled,
            companion_view: match value.companion_view {
                CompanionView::Transcript => "transcript",
                CompanionView::Assistant => "assistant",
                CompanionView::Split => "together",
            },
            companion_translation_view: match value.companion_translation_view {
                TranslationView::Original => "original",
                TranslationView::Translation => "translation",
                TranslationView::Both => "both",
            },
            companion_split_fraction: value.companion_split_fraction,
            translation_language: value.translation_language.clone(),
            translation_model: value.translation_model.clone(),
            translation_notice: value.translation_notice.clone(),
            input_devices: value.input_devices.clone(),
            output_devices: value.output_devices.clone(),
            available_models: value.available_models.clone(),
            selected_input_device: value.meeting_microphone_device.clone(),
            selected_output_device: value.meeting_device.clone(),
            provisional_text: value.provisional_text.clone(),
            transcript: value
                .transcript
                .iter()
                .filter(|line| !line.text.trim().is_empty())
                .map(Into::into)
                .collect(),
            translations: value.translations.iter().map(Into::into).collect(),
            messages: value.messages.iter().map(Into::into).collect(),
            assistant_busy: value.ai_busy,
            active_answer_model: value.active_answer_model.clone(),
            notice: value.notice.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_view_omits_private_settings_and_local_paths() {
        let directory = tempfile::tempdir().unwrap();
        let store =
            crate::settings::SettingsStore::new(directory.path().join("settings.toml")).unwrap();
        let mut settings = crate::settings::Settings::default();
        settings.formatting.custom_instructions = Some("secret-prompt-sentinel".into());
        let snapshot = UiSnapshot {
            runtime_status: crate::ui_bridge::UiRuntimeStatus::Ready,
            settings: crate::ui_bridge::UiSettingsSnapshot {
                values: settings.clone(),
                resolved_model_path: directory.path().join("private-model-sentinel"),
            },
            readiness: crate::ui_bridge::UiReadinessSnapshot::checking(&settings, &store),
            history_enabled: true,
            interrupted: Vec::new(),
            history: Vec::new(),
            lexicon: Vec::new(),
            profiles: Vec::new(),
        };
        let value = serde_json::to_string(&AppView::from(&snapshot)).unwrap();
        assert!(!value.contains("secret-prompt-sentinel"));
        assert!(!value.contains("private-model-sentinel"));
    }

    #[test]
    fn meeting_view_never_serializes_provider_or_action_secrets() {
        let mut source = WorkspaceSnapshot::default();
        source.provider.api_key = "provider-secret-sentinel".into();
        source.actions.push(phorminx_ui::workspace::ActionDraft {
            authorization: "action-secret-sentinel".into(),
            ..Default::default()
        });
        source.transcript.push(MeetingLine {
            start_sample: 1,
            end_sample: 2,
            text: "Synthetic meeting text".into(),
        });

        let value = serde_json::to_string(&MeetingView::from(&source)).unwrap();
        assert!(value.contains("Synthetic meeting text"));
        assert!(!value.contains("provider-secret-sentinel"));
        assert!(!value.contains("action-secret-sentinel"));
    }

    #[test]
    fn selected_provider_view_never_serializes_api_key() {
        let mut source = WorkspaceSnapshot::default();
        source.provider.api_key = "provider-secret-sentinel".into();
        source.provider.has_credential = true;
        source.provider.model = "local-test-model".into();
        let value = serde_json::to_string(&ProviderView::from_workspace(&source)).unwrap();
        assert!(value.contains("local-test-model"));
        assert!(!value.contains("provider-secret-sentinel"));
    }

    #[test]
    fn action_list_omits_authorization_and_headers() {
        let action = phorminx_ui::workspace::ActionDraft {
            id: "sample".into(),
            name: "Sample".into(),
            authorization: "action-secret-sentinel".into(),
            headers_json: "header-secret-sentinel".into(),
            ..Default::default()
        };
        let value = serde_json::to_string(&ActionItemView::from(&action)).unwrap();
        assert!(value.contains("Sample"));
        assert!(!value.contains("action-secret-sentinel"));
        assert!(!value.contains("header-secret-sentinel"));
    }

    #[test]
    fn known_source_metadata_is_not_shown_as_spoken_words() {
        let computer = TranscriptLineView::from(&MeetingLine {
            start_sample: 1,
            end_sample: 2,
            text: "[12:00.000 · Computer audio] A client question".into(),
        });
        assert_eq!(computer.source, "computer");
        assert_eq!(computer.text, "A client question");

        let microphone = TranscriptLineView::from(&MeetingLine {
            start_sample: 3,
            end_sample: 4,
            text: "[12:01.120 · You / microphone] My answer".into(),
        });
        assert_eq!(microphone.source, "microphone");
        assert_eq!(microphone.text, "My answer");

        let literal = TranscriptLineView::from(&MeetingLine {
            start_sample: 5,
            end_sample: 6,
            text: "[Reminder] Bring the design file".into(),
        });
        assert_eq!(literal.source, "conversation");
        assert_eq!(literal.text, "[Reminder] Bring the design file");
    }

    #[test]
    fn product_and_preview_use_distinct_tauri_windows() {
        let product: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        let preview: serde_json::Value =
            serde_json::from_str(include_str!("bin/tauri-preview-config/tauri.conf.json")).unwrap();
        assert_eq!(product["identifier"], "net.phorminx.app");
        assert_eq!(
            product["app"]["windows"][0]["url"],
            "tauri-preview/product.html"
        );
        assert_eq!(preview["identifier"], "net.phorminx.preview");
        assert_eq!(
            preview["app"]["windows"][0]["url"],
            "tauri-preview/index.html"
        );
    }
}
