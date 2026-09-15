//! Literal loopback servers only. No installed model, microphone, or real delivery.
use super::*;
use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    thread,
    time::Duration,
};

fn listener() -> (TcpListener, String) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    (listener, endpoint)
}
fn request(stream: &mut TcpStream) -> String {
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    let mut bytes = Vec::new();
    loop {
        let mut buffer = [0; 4096];
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
    String::from_utf8(bytes).unwrap()
}
fn respond(stream: &mut TcpStream, status: u16, body: &str) {
    write!(
        stream,
        "HTTP/1.1 {status} Fixture\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
}
fn action(endpoint: String) -> ActionConfig {
    ActionConfig {
        id: "fixture".into(),
        name: "Fixture".into(),
        endpoint,
        payload_mode: ActionPayloadMode::AiJson,
        payload_model: "fixture-model".into(),
        payload_prompt: "Create the note JSON.".into(),
        ..Default::default()
    }
}
fn generator(output: String, done_reason: &'static str) -> (String, thread::JoinHandle<()>) {
    let (listener, endpoint) = listener();
    let worker = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        assert!(request(&mut stream).starts_with("POST /api/show "));
        respond(&mut stream, 200, r#"{"capabilities":["completion"]}"#);
        drop(stream);
        let (mut stream, _) = listener.accept().unwrap();
        let wire = request(&mut stream);
        assert!(wire.starts_with("POST /api/chat "));
        assert!(wire.contains("synthetic note"));
        let body =
            serde_json::json!({"message":{"content":output},"done":true,"done_reason":done_reason})
                .to_string()
                + "\n";
        respond(&mut stream, 200, &body);
    });
    (endpoint, worker)
}

#[test]
fn preparation_never_dispatches_and_explicit_retry_keeps_exact_payload() {
    let (delivery, endpoint) = listener();
    delivery.set_nonblocking(true).unwrap();
    let config = action(endpoint);
    let body = r#"{"note":"synthetic note","number":9007199254740993}"#;
    let (ollama, generator) = generator(body.into(), "stop");
    let prepared = prepare_action(
        &config,
        "synthetic note",
        "retry-1",
        &ollama,
        &CancellationToken::default(),
    )
    .unwrap();
    generator.join().unwrap();
    assert_eq!(
        delivery.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
    delivery.set_nonblocking(false).unwrap();
    let receiver = thread::spawn(move || {
        let mut bodies = Vec::new();
        for status in [503, 200] {
            let (mut stream, _) = delivery.accept().unwrap();
            let wire = request(&mut stream);
            assert!(
                wire.to_ascii_lowercase()
                    .contains("idempotency-key: retry-1")
            );
            bodies.push(wire.split_once("\r\n\r\n").unwrap().1.to_owned());
            respond(&mut stream, status, "{}");
        }
        assert_eq!(bodies[0], body);
        assert_eq!(bodies[0], bodies[1]);
    });
    assert_eq!(
        crate::execute_prepared_action(&prepared, &CancellationToken::default()),
        Err(AssistantError::HttpStatus(503))
    );
    assert_eq!(
        crate::execute_prepared_action(&prepared, &CancellationToken::default())
            .unwrap()
            .status,
        200
    );
    receiver.join().unwrap();
}

#[test]
fn malformed_incomplete_and_oversize_generation_never_reaches_delivery() {
    for (output, reason, error) in [
        (
            "```json\n{}\n```".into(),
            "stop",
            AssistantError::InvalidPayload,
        ),
        ("{}".into(), "length", AssistantError::Incomplete),
        (
            format!("\"{}\"", "x".repeat(MAX_GENERATED_BYTES)),
            "stop",
            AssistantError::SizeLimit,
        ),
    ] {
        let (delivery, endpoint) = listener();
        delivery.set_nonblocking(true).unwrap();
        let (ollama, generator) = generator(output, reason);
        assert_eq!(
            prepare_action(
                &action(endpoint),
                "synthetic note",
                "id-1",
                &ollama,
                &CancellationToken::default()
            ),
            Err(error)
        );
        generator.join().unwrap();
        assert_eq!(
            delivery.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
    }
}

#[test]
fn hidden_cloud_model_rejected_before_transcript_and_destination_are_sent() {
    let (model, ollama) = listener();
    let (delivery, endpoint) = listener();
    delivery.set_nonblocking(true).unwrap();
    let server = thread::spawn(move || {
        let (mut stream, _) = model.accept().unwrap();
        let wire = request(&mut stream);
        assert!(!wire.contains("synthetic note"));
        respond(
            &mut stream,
            200,
            r#"{"capabilities":["completion"],"remote_host":"cloud.example.test"}"#,
        );
        model.set_nonblocking(true).unwrap();
        assert_eq!(
            model.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
    });
    assert_eq!(
        prepare_action(
            &action(endpoint),
            "synthetic note",
            "id-1",
            &ollama,
            &CancellationToken::default()
        ),
        Err(AssistantError::InvalidConfig)
    );
    server.join().unwrap();
    assert_eq!(
        delivery.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
}
