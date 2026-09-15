//! Adversarial action lifecycle checks. Capture events are synthetic, data is
//! temporary, and HTTP listeners bind literal loopback ephemeral ports only.
use super::*;
use crate::settings::Settings;
use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
};

fn fixture() -> (Host, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let store = SettingsStore::new(dir.path().join("settings.toml")).unwrap();
    store.save(&Settings::default()).unwrap();
    (Host::new(store).unwrap(), dir)
}
fn listener() -> (TcpListener, String) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let endpoint = format!("http://{}/notes", listener.local_addr().unwrap());
    (listener, endpoint)
}
fn action(endpoint: String) -> ActionConfig {
    ActionConfig {
        id: "qa-note".into(),
        name: "Synthetic action".into(),
        endpoint,
        launcher_slot: Some(7),
        ..Default::default()
    }
}
fn arm(host: &mut Host, config: ActionConfig, text: &str) {
    // Do not start an audio device: inject the state just before its Stop event.
    host.note_capture = true;
    host.view.capture = CaptureState::Listening;
    host.view.note_text = text.into();
    host.view.capture_action = Some(config.name.clone());
    host.action_capture = Some(config);
}
fn request(listener: &TcpListener) -> (TcpStream, String) {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut stream = loop {
        match listener.accept() {
            Ok((stream, _)) => break stream,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                assert!(Instant::now() < deadline, "Expected one synthetic delivery");
                thread::sleep(Duration::from_millis(5));
            }
            Err(error) => panic!("fixture accept failed: {error}"),
        }
    };
    // Windows may inherit FIONBIO from the nonblocking listening socket.
    // Accept is polled with a deadline; packet reads use bounded blocking I/O.
    stream.set_nonblocking(false).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut bytes = Vec::new();
    loop {
        let mut buffer = [0u8; 4096];
        let count = stream.read(&mut buffer).unwrap();
        assert!(count > 0);
        bytes.extend_from_slice(&buffer[..count]);
        assert!(bytes.len() < 2_000_000);
        if let Some(end) = bytes.windows(4).position(|b| b == b"\r\n\r\n") {
            let headers = String::from_utf8_lossy(&bytes[..end]);
            let length = headers
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .map(|v| v.trim().parse::<usize>().unwrap())
                })
                .unwrap_or(0);
            if bytes.len() >= end + 4 + length {
                break;
            }
        }
    }
    (stream, String::from_utf8(bytes).unwrap())
}
fn respond(stream: &mut TcpStream, status: u16) {
    write!(
        stream,
        "HTTP/1.1 {status} Fixture\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{{}}"
    )
    .unwrap();
}
fn finish_delivery(host: &mut Host) {
    let event = host
        .job_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("delivery worker completion");
    assert!(matches!(event, JobEvent::Delivered(_)));
    host.job_event(event);
}

#[test]
fn fixture_reader_handles_delayed_fragmented_headers_and_body() {
    let (listener, _) = listener();
    let address = listener.local_addr().unwrap();
    let writer = thread::spawn(move || {
        let mut stream = TcpStream::connect(address).unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        // Accept/read can happen before even the first packet arrives. A
        // nonblocking inherited socket must not turn this normal delay into a failure.
        thread::sleep(Duration::from_millis(20));
        for fragment in [
            "POST /notes HTTP/1.1\r\nContent-Len",
            "gth: 16\r\n\r\n{\"note\":",
            "\"split\"}",
        ] {
            stream.write_all(fragment.as_bytes()).unwrap();
            thread::sleep(Duration::from_millis(5));
        }
    });
    let (_, wire) = request(&listener);
    assert_eq!(
        wire.split_once("\r\n\r\n").unwrap().1,
        r#"{"note":"split"}"#
    );
    writer.join().unwrap();
}

#[test]
fn cancelled_error_and_empty_recordings_never_dispatch_even_after_late_stop() {
    for case in ["cancel", "error", "empty"] {
        let (mut host, _dir) = fixture();
        let (receiver, endpoint) = listener();
        arm(
            &mut host,
            action(endpoint),
            if case == "empty" {
                "   "
            } else {
                "synthetic retained text"
            },
        );
        match case {
            "cancel" => host.handle(WorkspaceEvent::CancelActionCapture).unwrap(),
            "error" => host
                .audio_event(MeetingAudioEvent::Error {
                    message: "Synthetic device fault".into(),
                })
                .unwrap(),
            _ => (),
        }
        host.audio_event(MeetingAudioEvent::Stopped { sample: 100 })
            .unwrap();
        host.audio_event(MeetingAudioEvent::Stopped { sample: 100 })
            .unwrap();
        assert!(host.action_capture.is_none());
        assert_eq!(host.view.capture, CaptureState::Idle);
        assert!(!host.view.delivery_busy);
        assert!(host.delivery_cancel.is_none());
        assert_eq!(
            receiver.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
        if case == "error" {
            assert_eq!(host.view.note_text, "synthetic retained text");
        }
    }
}

#[test]
fn stop_dispatches_frozen_action_once_and_does_not_follow_later_configuration_edits() {
    let (mut host, _dir) = fixture();
    let (original_receiver, original_endpoint) = listener();
    let (edited_receiver, edited_endpoint) = listener();
    let captured = action(original_endpoint);
    arm(
        &mut host,
        captured.clone(),
        "quote\"\n{{delivery_id}} synthetic",
    );
    host.config.actions = vec![ActionConfig {
        endpoint: edited_endpoint,
        ..captured
    }];
    host.audio_event(MeetingAudioEvent::Stopped { sample: 200 })
        .unwrap();
    assert!(host.view.delivery_busy);
    let (mut stream, wire) = request(&original_receiver);
    assert!(wire.starts_with("POST /notes "));
    let body: serde_json::Value =
        serde_json::from_str(wire.split_once("\r\n\r\n").unwrap().1).unwrap();
    assert_eq!(body["text"], "quote\"\n{{delivery_id}} synthetic");
    // A duplicated terminal event cannot create a second worker while the first is pending.
    host.audio_event(MeetingAudioEvent::Stopped { sample: 200 })
        .unwrap();
    respond(&mut stream, 200);
    finish_delivery(&mut host);
    host.audio_event(MeetingAudioEvent::Stopped { sample: 200 })
        .unwrap();
    assert_eq!(
        original_receiver.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
    assert_eq!(
        edited_receiver.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
    assert_eq!(host.view.notice, "Note delivered.");
}

#[test]
fn coordinator_retry_reuses_completed_ai_json_without_running_any_model() {
    let (mut host, _dir) = fixture();
    let (receiver, endpoint) = listener();
    let config = ActionConfig {
        payload_mode: phorminx_assistant::ActionPayloadMode::AiJson,
        payload_model: "not-an-installed-model".into(),
        payload_prompt: "Synthetic fixture".into(),
        ..action(endpoint)
    };
    // Deliberately unreachable local model endpoint. Cached retry must never contact it.
    host.config.provider.ollama_endpoint = "http://127.0.0.1:1".into();
    let body = r#"{"note":"synthetic note","n":9007199254740993}"#;
    let prepared = phorminx_assistant::prepare_generated_action(
        &config,
        "synthetic note",
        "frozen-retry",
        body,
    )
    .unwrap();
    host.last_delivery = Some(DeliveryAttempt {
        action: config.clone(),
        text: "synthetic note".into(),
        id: "frozen-retry".into(),
        prepared: Arc::new(Mutex::new(Some(prepared))),
    });
    for status in [503, 200] {
        host.deliver_config(config.clone(), "synthetic note".into())
            .unwrap();
        let (mut stream, wire) = request(&receiver);
        assert!(
            wire.to_ascii_lowercase()
                .contains("idempotency-key: frozen-retry")
        );
        assert_eq!(wire.split_once("\r\n\r\n").unwrap().1, body);
        respond(&mut stream, status);
        finish_delivery(&mut host);
        assert!(!host.view.delivery_busy);
        assert_eq!(host.last_delivery.is_none(), status == 200);
    }
}

#[test]
fn missing_launcher_slot_and_invalid_action_fail_before_capture_or_network() {
    let (mut host, _dir) = fixture();
    assert!(host.handle(WorkspaceEvent::TriggerActionSlot(8)).is_err());
    let mut config = action("http://not-loopback.test/notes".into());
    config.launcher_slot = Some(7);
    assert!(host.trigger_config(config).is_err());
    assert!(host.audio.is_none());
    assert!(host.action_capture.is_none());
    assert!(!host.view.delivery_busy);
}
