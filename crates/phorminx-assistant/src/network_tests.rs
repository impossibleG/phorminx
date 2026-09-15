//! All endpoints and credentials here are synthetic, literal-loopback fixtures.
use crate::*;
use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
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
    let mut buf = [0u8; 4096];
    loop {
        let n = stream.read(&mut buf).unwrap();
        assert!(n > 0);
        bytes.extend_from_slice(&buf[..n]);
        assert!(bytes.len() < 2_000_000);
        if let Some(end) = bytes.windows(4).position(|s| s == b"\r\n\r\n") {
            let headers = String::from_utf8_lossy(&bytes[..end]);
            let length = headers
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .map(|s| s.trim().parse::<usize>().unwrap())
                })
                .unwrap_or(0);
            if bytes.len() >= end + 4 + length {
                break;
            }
        }
    }
    String::from_utf8(bytes).unwrap()
}
fn respond(stream: &mut TcpStream, body: &str) {
    write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
}
fn config(endpoint: String) -> ProviderConfig {
    ProviderConfig {
        model: "synthetic-local-model".into(),
        ollama_endpoint: endpoint,
        ..Default::default()
    }
}
fn messages() -> Vec<ChatMessage> {
    vec![ChatMessage {
        role: ChatRole::User,
        content: "private synthetic transcript".into(),
    }]
}

#[test]
fn ollama_preflight_and_stream_roundtrip() {
    let (listener, endpoint) = listener();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let show = request(&mut stream);
        assert!(show.starts_with("POST /api/show "));
        assert!(!show.contains("private synthetic transcript"));
        respond(&mut stream, r#"{"capabilities":["completion"]}"#);
        drop(stream);
        let (mut stream, _) = listener.accept().unwrap();
        let chat = request(&mut stream);
        assert!(chat.starts_with("POST /api/chat "));
        assert!(chat.contains("private synthetic transcript"));
        assert!(!chat.to_ascii_lowercase().contains("authorization"));
        respond(
            &mut stream,
            "{\"message\":{\"content\":\"Olá\"},\"done\":false}\n{\"message\":{\"content\":\"!\"},\"done\":true,\"done_reason\":\"stop\"}\n",
        );
    });
    let mut output = String::new();
    assert_eq!(
        stream_chat(
            &config(endpoint),
            &messages(),
            &CancellationToken::default(),
            |s| output.push_str(s)
        ),
        Ok(())
    );
    assert_eq!(output, "Olá!");
    server.join().unwrap();
}

#[test]
fn hidden_cloud_alias_rejected_before_private_input() {
    let (listener, endpoint) = listener();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let show = request(&mut stream);
        assert!(!show.contains("private synthetic transcript"));
        respond(
            &mut stream,
            r#"{"capabilities":["completion"],"remote_model":"secret-cloud-alias"}"#,
        );
        listener.set_nonblocking(true).unwrap();
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
    });
    assert_eq!(
        stream_chat(
            &config(endpoint),
            &messages(),
            &CancellationToken::default(),
            |_| panic!("no delta expected")
        ),
        Err(AssistantError::InvalidConfig)
    );
    server.join().unwrap();
}

#[test]
fn stalled_response_headers_cancel_by_socket_shutdown() {
    let (listener, endpoint) = listener();
    let (ready_tx, ready_rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        request(&mut stream);
        ready_tx.send(()).unwrap();
        // Deliberately provide no HTTP headers. Cancellation must wake the blocked read.
        let mut buf = [0];
        let _ = stream.read(&mut buf);
    });
    let token = CancellationToken::default();
    let worker_token = token.clone();
    let worker = thread::spawn(move || {
        done_tx
            .send(stream_chat(
                &config(endpoint),
                &messages(),
                &worker_token,
                |_| {},
            ))
            .unwrap();
    });
    ready_rx.recv_timeout(Duration::from_secs(3)).unwrap();
    let before = Instant::now();
    token.cancel();
    assert_eq!(
        done_rx.recv_timeout(Duration::from_secs(2)).unwrap(),
        Err(AssistantError::Cancelled)
    );
    assert!(before.elapsed() < Duration::from_secs(2));
    worker.join().unwrap();
    server.join().unwrap();
}

#[test]
fn stalled_response_body_cancel_by_socket_shutdown() {
    let (listener, endpoint) = listener();
    let (ready_tx, ready_rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        request(&mut stream);
        respond(&mut stream, r#"{"capabilities":["completion"]}"#);
        drop(stream);
        let (mut stream, _) = listener.accept().unwrap();
        request(&mut stream);
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 99999\r\n\r\n")
            .unwrap();
        ready_tx.send(()).unwrap();
        let mut buf = [0];
        let _ = stream.read(&mut buf);
    });
    let token = CancellationToken::default();
    let worker_token = token.clone();
    let worker = thread::spawn(move || {
        done_tx
            .send(stream_chat(
                &config(endpoint),
                &messages(),
                &worker_token,
                |_| {},
            ))
            .unwrap();
    });
    ready_rx.recv_timeout(Duration::from_secs(3)).unwrap();
    token.cancel();
    assert_eq!(
        done_rx.recv_timeout(Duration::from_secs(2)).unwrap(),
        Err(AssistantError::Cancelled)
    );
    worker.join().unwrap();
    server.join().unwrap();
}

#[test]
#[cfg(windows)]
fn http_action_protected_headers_and_stable_delivery_id() {
    let (listener, endpoint) = listener();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let req = request(&mut stream);
        let lower = req.to_ascii_lowercase();
        assert!(req.starts_with("POST /notes "));
        assert!(lower.contains("idempotency-key: stable-id"));
        assert!(lower.contains("authorization: bearer synthetic-key"));
        assert!(lower.contains("x-private-token: synthetic-header"));
        let body: serde_json::Value =
            serde_json::from_str(req.split_once("\r\n\r\n").unwrap().1).unwrap();
        assert_eq!(body["text"], "quote\"\n{{delivery_id}}");
        respond(&mut stream, "server response is intentionally not surfaced");
    });
    let action = ActionConfig {
        id: "notes".into(),
        name: "Notes".into(),
        endpoint: format!("{endpoint}/notes"),
        authorization: Some(ProtectedSecret::protect("Bearer synthetic-key").unwrap()),
        headers: vec![ActionHeader {
            name: "x-private-token".into(),
            value: ProtectedSecret::protect("synthetic-header").unwrap(),
        }],
        ..Default::default()
    };
    let receipt = execute_action(
        &action,
        "quote\"\n{{delivery_id}}",
        "stable-id",
        &CancellationToken::default(),
    )
    .unwrap();
    assert_eq!(
        receipt,
        DeliveryReceipt {
            delivery_id: "stable-id".into(),
            status: 200
        }
    );
    server.join().unwrap();
}

#[test]
fn redirects_are_not_followed_and_lost_responses_are_not_retried() {
    for redirect in [true, false] {
        let (listener, endpoint) = listener();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            request(&mut stream);
            if redirect {
                stream.write_all(b"HTTP/1.1 307 Temporary Redirect\r\nLocation: https://example.test/leak\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
            }
            // With no response this simulates a server that committed the note then disconnected.
        });
        let action = ActionConfig {
            id: "notes".into(),
            name: "Notes".into(),
            endpoint,
            ..Default::default()
        };
        let result = execute_action(
            &action,
            "synthetic text",
            "stable-id",
            &CancellationToken::default(),
        );
        assert_eq!(
            result,
            if redirect {
                Err(AssistantError::HttpStatus(307))
            } else {
                Err(AssistantError::DeliveryUncertain)
            }
        );
        server.join().unwrap();
    }
}
