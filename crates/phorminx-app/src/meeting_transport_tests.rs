//! Synthetic coordinator QA; no network, microphone, model inference or personal data.
use super::*;
use crate::{performance_runtime::WorkloadCoordinator, settings::Settings};

fn host() -> (Host, tempfile::TempDir) {
    let directory = tempfile::tempdir().unwrap();
    let store = SettingsStore::new(directory.path().join("settings.toml")).unwrap();
    store.save(&Settings::default()).unwrap();
    (Host::new(store).unwrap(), directory)
}
fn action() -> ActionConfig {
    ActionConfig {
        id: "synthetic-action".into(),
        name: "Synthetic notes".into(),
        endpoint: "https://example.test/notes".into(),
        ..Default::default()
    }
}
fn draft(endpoint: &str) -> ActionDraft {
    ActionDraft {
        id: "synthetic-action".into(),
        name: "Synthetic notes".into(),
        endpoint: endpoint.into(),
        method: "POST".into(),
        payload_template: r#"{"text":"{{text}}"}"#.into(),
        ..Default::default()
    }
}

#[test]
fn failed_delivery_keeps_exact_attempt_identity_but_success_releases_it() {
    let (mut host, _directory) = host();
    let config = action();
    let first = delivery_identity(None, &config, "synthetic note");
    host.last_delivery = Some(DeliveryAttempt {
        action: config.clone(),
        text: "synthetic note".into(),
        id: first.clone(),
        prepared: Default::default(),
    });
    host.delivery_cancel = Some(CancellationToken::default());
    host.view.delivery_busy = true;
    host.job_event(JobEvent::Delivered(Err(
        "Unconfirmed synthetic delivery".into()
    )));
    assert!(!host.view.delivery_busy);
    assert!(host.delivery_cancel.is_none());
    assert_eq!(
        delivery_identity(host.last_delivery.as_ref(), &config, "synthetic note"),
        first
    );
    host.job_event(JobEvent::Delivered(Ok(())));
    assert!(host.last_delivery.is_none());
    assert_ne!(
        delivery_identity(host.last_delivery.as_ref(), &config, "synthetic note"),
        first
    );
}

#[test]
fn retry_identity_is_scoped_to_exact_payload_destination_headers_and_auth() {
    let config = action();
    let previous = DeliveryAttempt {
        action: config.clone(),
        text: "synthetic note".into(),
        id: "old-attempt".into(),
        prepared: Default::default(),
    };
    assert_eq!(
        delivery_identity(Some(&previous), &config, "synthetic note"),
        "old-attempt"
    );
    assert_ne!(
        delivery_identity(Some(&previous), &config, "edited note"),
        "old-attempt"
    );
    let mut variants = Vec::new();
    let mut changed = config.clone();
    changed.endpoint = "https://another.example.test/notes".into();
    variants.push(changed);
    let mut changed = config.clone();
    changed.method = ActionMethod::Put;
    variants.push(changed);
    let mut changed = config.clone();
    changed.payload_template = r#"{"message":"{{text}}"}"#.into();
    variants.push(changed);
    let mut changed = config.clone();
    changed.authorization = Some(ProtectedSecret::protect("Bearer synthetic-key").unwrap());
    variants.push(changed);
    let mut changed = config.clone();
    changed.headers = vec![phorminx_assistant::ActionHeader {
        name: "X-Synthetic".into(),
        value: ProtectedSecret::protect("synthetic-header").unwrap(),
    }];
    variants.push(changed);
    for changed in variants {
        assert_ne!(
            delivery_identity(Some(&previous), &changed, "synthetic note"),
            "old-attempt"
        );
    }
}

#[test]
fn fresh_identifiers_are_distinct_even_without_a_clock_tick() {
    let values: std::collections::HashSet<_> =
        (0..1000).map(|_| fresh_workspace_id("phorminx")).collect();
    assert_eq!(values.len(), 1000);
    assert!(values.iter().all(|id|id.len()<=128&&id.bytes().all(|b|b.is_ascii_alphanumeric()||b==b'-')));
}

#[test]
fn custom_credentials_are_retained_only_for_identical_destination_or_reentry() {
    let (mut host, _directory) = host();
    let mut first = draft("https://example.test/notes");
    first.authorization = "Bearer synthetic-key".into();
    first.headers_json = r#"{"X-Token":"synthetic-header"}"#.into();
    host.save_action(first).unwrap();
    let config = host.config.actions[0].clone();
    host.save_action(draft("https://example.test/notes"))
        .unwrap();
    assert_eq!(host.config.actions[0].authorization, config.authorization);
    assert_eq!(host.config.actions[0].headers, config.headers);
    assert!(host.view.actions[0].authorization.is_empty());
    assert!(host.view.actions[0].headers_json.is_empty());
    assert_eq!(host.view.actions[0].header_names, vec!["X-Token"]);
    host.save_action(draft("https://another.example.test/notes"))
        .unwrap();
    assert!(host.config.actions[0].authorization.is_none());
    assert!(host.config.actions[0].headers.is_empty());
    let mut explicit = draft("https://third.example.test/notes");
    explicit.authorization = "Bearer replacement".into();
    explicit.headers_json = r#"{"X-New":"replacement"}"#.into();
    host.save_action(explicit).unwrap();
    assert_eq!(
        host.config.actions[0]
            .authorization
            .as_ref()
            .unwrap()
            .expose()
            .unwrap()
            .as_str(),
        "Bearer replacement"
    );
    assert_eq!(
        host.config.actions[0].headers[0]
            .value
            .expose()
            .unwrap()
            .as_str(),
        "replacement"
    );
    let mut clear = draft("https://third.example.test/notes");
    clear.clear_credential = true;
    clear.clear_headers = true;
    host.save_action(clear).unwrap();
    assert!(host.config.actions[0].authorization.is_none());
    assert!(host.config.actions[0].headers.is_empty());
}

#[test]
fn invalid_action_saves_preserve_config_and_ack_revision_and_delete_is_supported() {
    let (mut host, _directory) = host();
    host.save_action(draft("https://example.test/notes"))
        .unwrap();
    let previous = host.config.clone();
    let revision = host.view.action_revision;
    for method in ["", "INVALID", "CONNECT"] {
        let mut invalid = draft("https://example.test/notes");
        invalid.method = method.into();
        assert!(host.save_action(invalid).is_err());
        assert_eq!(host.config, previous);
        assert_eq!(host.view.action_revision, revision);
    }
    for headers in [
        r#"{"Authorization":"oops"}"#,
        r#"{"Host":"evil.test"}"#,
        r#"{"X":123}"#,
        r#"{"X":"one","x":"two"}"#,
    ] {
        let mut invalid = draft("https://example.test/notes");
        invalid.headers_json = headers.into();
        assert!(host.save_action(invalid).is_err());
        assert_eq!(host.config, previous);
        assert_eq!(host.view.action_revision, revision);
    }
    let mut delete = draft("https://example.test/notes");
    delete.method = "DELETE".into();
    host.save_action(delete).unwrap();
    assert_eq!(host.config.actions[0].method, ActionMethod::Delete);
}

#[test]
fn local_chat_compute_lease_excludes_formatter_but_not_recording() {
    let coordinator = WorkloadCoordinator::default();
    let capture = coordinator
        .try_begin(RuntimeActivityKind::Dictation)
        .unwrap();
    let chat = acquire_chat_compute(ProviderKind::Ollama, &coordinator)
        .unwrap()
        .unwrap();
    assert!(coordinator.try_begin(RuntimeActivityKind::Ollama).is_err());
    assert!(coordinator.try_begin(RuntimeActivityKind::Whisper).is_ok());
    assert!(
        acquire_chat_compute(ProviderKind::OpenAi, &coordinator)
            .unwrap()
            .is_none()
    );
    assert!(
        acquire_chat_compute(ProviderKind::Anthropic, &coordinator)
            .unwrap()
            .is_none()
    );
    drop(chat);
    let formatter = coordinator.try_begin(RuntimeActivityKind::Ollama).unwrap();
    assert!(acquire_chat_compute(ProviderKind::Ollama, &coordinator).is_err());
    drop(formatter);
    drop(capture);
    let chat = acquire_chat_compute(ProviderKind::Ollama, &coordinator)
        .unwrap()
        .unwrap();
    assert!(
        coordinator
            .try_begin(RuntimeActivityKind::Dictation)
            .is_ok()
    );
    drop(chat);
    assert!(!coordinator.is_busy());
}

#[test]
fn invalid_or_busy_deliveries_do_not_start_workers() {
    let (mut host, _directory) = host();
    host.config.actions.push(action());
    for text in [String::new(), " ".into(), "x".repeat(512 * 1024 + 1)] {
        assert!(host.deliver("synthetic-action".into(), text).is_err());
        assert!(host.delivery_cancel.is_none());
        assert!(host.last_delivery.is_none());
    }
    host.delivery_cancel = Some(CancellationToken::default());
    assert!(
        host.deliver("synthetic-action".into(), "synthetic note".into())
            .is_err()
    );
    assert!(host.last_delivery.is_none());
}
