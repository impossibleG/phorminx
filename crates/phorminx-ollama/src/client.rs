use std::fmt;
use std::io::Read;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::model::{ModelCatalog, ModelName, ModelSelectionError, TagsResponse};
use crate::prompt::{FormatProfile, PromptError, PromptPlan, build_prompt};
use crate::validation::{OutputValidator, ValidationError};

const DEFAULT_PORT: u16 = 11_434;
const MAX_DISCOVERY_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
const MAX_GENERATE_RESPONSE_BYTES: usize = 1024 * 1024;

/// An Ollama base address guaranteed to resolve to an IP loopback literal.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OllamaEndpoint {
    base_url: String,
}

impl OllamaEndpoint {
    pub fn ipv4(port: u16) -> Self {
        Self {
            base_url: format!("http://127.0.0.1:{port}"),
        }
    }

    pub fn ipv6(port: u16) -> Self {
        Self {
            base_url: format!("http://[::1]:{port}"),
        }
    }

    /// Parses only `http://127.0.0.1`, `http://[::1]`, and `http://localhost`.
    /// `localhost` is canonicalized to `127.0.0.1`, avoiding hosts-file or DNS routing.
    pub fn parse(value: &str) -> Result<Self, ClientError> {
        let value = value.trim();
        let authority = value.strip_prefix("http://").ok_or_else(|| {
            ClientError::InvalidEndpoint("only http loopback URLs are allowed".to_owned())
        })?;
        if authority.is_empty()
            || authority.contains(['/', '?', '#', '@'])
            || authority.chars().any(char::is_whitespace)
        {
            return Err(ClientError::InvalidEndpoint(
                "endpoint must contain only a loopback host and optional port".to_owned(),
            ));
        }

        if let Some(port) = authority.strip_prefix("[::1]:") {
            return parse_port(port).map(Self::ipv6);
        }
        if authority == "[::1]" {
            return Ok(Self::ipv6(DEFAULT_PORT));
        }

        let (host, port) = match authority.split_once(':') {
            Some((host, port)) => (host, parse_port(port)?),
            None => (authority, DEFAULT_PORT),
        };
        match host.to_ascii_lowercase().as_str() {
            "127.0.0.1" | "localhost" => Ok(Self::ipv4(port)),
            _ => Err(ClientError::InvalidEndpoint(
                "endpoint host must be 127.0.0.1, [::1], or localhost".to_owned(),
            )),
        }
    }

    pub fn as_str(&self) -> &str {
        &self.base_url
    }

    fn api_url(&self, path: &str) -> String {
        format!("{}{path}", self.base_url)
    }
}

impl Default for OllamaEndpoint {
    fn default() -> Self {
        Self::ipv4(DEFAULT_PORT)
    }
}

impl fmt::Display for OllamaEndpoint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.base_url)
    }
}

fn parse_port(value: &str) -> Result<u16, ClientError> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(ClientError::InvalidEndpoint("invalid TCP port".to_owned()));
    }
    let port = value
        .parse::<u16>()
        .map_err(|_| ClientError::InvalidEndpoint("TCP port is out of range".to_owned()))?;
    if port == 0 {
        return Err(ClientError::InvalidEndpoint(
            "TCP port cannot be zero".to_owned(),
        ));
    }
    Ok(port)
}

/// Finite deadlines for every blocking part of a local API call.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClientTimeouts {
    pub connect: Duration,
    pub response_headers: Duration,
    pub response_body: Duration,
    pub overall: Duration,
}

impl Default for ClientTimeouts {
    fn default() -> Self {
        Self {
            connect: Duration::from_secs(2),
            response_headers: Duration::from_secs(120),
            response_body: Duration::from_secs(30),
            overall: Duration::from_secs(180),
        }
    }
}

impl ClientTimeouts {
    fn validate(&self) -> Result<(), ClientError> {
        if [
            self.connect,
            self.response_headers,
            self.response_body,
            self.overall,
        ]
        .contains(&Duration::ZERO)
        {
            return Err(ClientError::InvalidTimeout);
        }
        Ok(())
    }
}

/// Ollama model residency after a request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum KeepAlive {
    ServerDefault,
    UnloadAfterRequest,
    For(Duration),
    Indefinite,
}

impl KeepAlive {
    fn api_value(&self) -> Option<String> {
        match self {
            Self::ServerDefault => None,
            Self::UnloadAfterRequest => Some("0s".to_owned()),
            Self::For(duration) => Some(format!("{}s", duration.as_secs().max(1))),
            Self::Indefinite => Some("-1".to_owned()),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct CancellationToken(Arc<AtomicBool>);

impl CancellationToken {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }

    fn check(&self) -> Result<(), ClientError> {
        if self.is_cancelled() {
            Err(ClientError::Cancelled)
        } else {
            Ok(())
        }
    }
}

/// Synchronous Ollama client. Clone it freely; ureq's connection pool is shared.
#[derive(Clone, Debug)]
pub struct OllamaClient {
    endpoint: OllamaEndpoint,
    agent: ureq::Agent,
    validator: OutputValidator,
}

impl OllamaClient {
    pub fn new(endpoint: OllamaEndpoint, timeouts: ClientTimeouts) -> Result<Self, ClientError> {
        timeouts.validate()?;
        let config = ureq::Agent::config_builder()
            .proxy(None)
            .max_redirects(0)
            .max_redirects_will_error(false)
            .http_status_as_error(false)
            .timeout_connect(Some(timeouts.connect))
            .timeout_recv_response(Some(timeouts.response_headers))
            .timeout_recv_body(Some(timeouts.response_body))
            .timeout_global(Some(timeouts.overall))
            .build();
        Ok(Self {
            endpoint,
            agent: config.into(),
            validator: OutputValidator::default(),
        })
    }

    pub fn with_validator(mut self, validator: OutputValidator) -> Self {
        self.validator = validator;
        self
    }

    pub fn endpoint(&self) -> &OllamaEndpoint {
        &self.endpoint
    }

    /// Lists only models installed in the user's already-running local Ollama service.
    pub fn discover(&self, cancel: &CancellationToken) -> Result<ModelCatalog, ClientError> {
        cancel.check()?;
        let response = self
            .agent
            .get(&self.endpoint.api_url("/api/tags"))
            .call()
            .map_err(map_http_error)?;
        let body = read_response(response, MAX_DISCOVERY_RESPONSE_BYTES, cancel)?;
        let response: TagsResponse =
            serde_json::from_slice(&body).map_err(ClientError::InvalidJson)?;
        ModelCatalog::from_api(response.models).map_err(ClientError::InvalidCatalog)
    }

    /// Loads a model with an empty generation request and sets its residency policy.
    pub fn warm_up(
        &self,
        model: &ModelName,
        keep_alive: KeepAlive,
        cancel: &CancellationToken,
    ) -> Result<(), ClientError> {
        self.generate_request(model, "", None, keep_alive, cancel)
            .map(|_| ())
    }

    /// Asks Ollama to unload one model immediately.
    pub fn unload(&self, model: &ModelName, cancel: &CancellationToken) -> Result<(), ClientError> {
        self.warm_up(model, KeepAlive::UnloadAfterRequest, cancel)
    }

    /// Performs one validated formatting operation, falling back to the exact input.
    pub fn format(
        &self,
        model: &ModelName,
        transcript: &str,
        profile: &FormatProfile,
        keep_alive: KeepAlive,
        cancel: &CancellationToken,
    ) -> FormatResult {
        let plan = match build_prompt(transcript, profile) {
            Ok(plan) => plan,
            Err(error) => {
                return FormatResult::Fallback {
                    text: transcript.to_owned(),
                    reason: FallbackReason::PromptRejected(error),
                };
            }
        };
        let PromptPlan::Generate(prompt) = plan else {
            return FormatResult::Fallback {
                text: transcript.to_owned(),
                reason: FallbackReason::RawProfile,
            };
        };

        let generated = match self.generate_request(
            model,
            &prompt.user,
            Some(&prompt.system),
            keep_alive,
            cancel,
        ) {
            Ok(output) => output,
            Err(ClientError::Cancelled) => {
                return FormatResult::Fallback {
                    text: transcript.to_owned(),
                    reason: FallbackReason::Cancelled,
                };
            }
            Err(error) => {
                return FormatResult::Fallback {
                    text: transcript.to_owned(),
                    reason: FallbackReason::ServiceUnavailable(error.to_string()),
                };
            }
        };
        match self
            .validator
            .validate(transcript, &generated, &prompt.protected_tokens)
        {
            Ok(text) => FormatResult::Formatted {
                text,
                model: model.clone(),
            },
            Err(error) => FormatResult::Fallback {
                text: transcript.to_owned(),
                reason: FallbackReason::OutputRejected(error),
            },
        }
    }

    /// Low-level generation for integrations that need a prebuilt prompt.
    pub fn generate(
        &self,
        model: &ModelName,
        prompt: &str,
        system: Option<&str>,
        keep_alive: KeepAlive,
        cancel: &CancellationToken,
    ) -> Result<String, ClientError> {
        self.generate_request(model, prompt, system, keep_alive, cancel)
    }

    fn generate_request(
        &self,
        model: &ModelName,
        prompt: &str,
        system: Option<&str>,
        keep_alive: KeepAlive,
        cancel: &CancellationToken,
    ) -> Result<String, ClientError> {
        cancel.check()?;
        let request = GenerateRequest {
            model: model.as_str(),
            prompt,
            system,
            stream: false,
            keep_alive: keep_alive.api_value(),
            options: GenerateOptions { temperature: 0.0 },
        };
        let request_body = serde_json::to_vec(&request).map_err(ClientError::SerializeRequest)?;
        let response = self
            .agent
            .post(&self.endpoint.api_url("/api/generate"))
            .content_type("application/json")
            .send(request_body.as_slice())
            .map_err(map_http_error)?;
        let body = read_response(response, MAX_GENERATE_RESPONSE_BYTES, cancel)?;
        let response: GenerateResponse =
            serde_json::from_slice(&body).map_err(ClientError::InvalidJson)?;
        if let Some(error) = response.error {
            return Err(ClientError::Api(error));
        }
        if !response.done {
            return Err(ClientError::IncompleteResponse);
        }
        Ok(response.response)
    }
}

impl Default for OllamaClient {
    fn default() -> Self {
        Self::new(OllamaEndpoint::default(), ClientTimeouts::default())
            .expect("default Ollama client configuration is valid")
    }
}

fn read_response(
    response: ureq::http::Response<ureq::Body>,
    maximum_bytes: usize,
    cancel: &CancellationToken,
) -> Result<Vec<u8>, ClientError> {
    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        return Err(ClientError::HttpStatus(status));
    }
    let mut reader = response.into_parts().1.into_reader();
    let mut body = Vec::new();
    let mut buffer = [0_u8; 8 * 1024];
    loop {
        cancel.check()?;
        let count = reader
            .read(&mut buffer)
            .map_err(ClientError::ReadResponse)?;
        if count == 0 {
            break;
        }
        if body.len().saturating_add(count) > maximum_bytes {
            return Err(ClientError::ResponseTooLarge(maximum_bytes));
        }
        body.extend_from_slice(&buffer[..count]);
    }
    cancel.check()?;
    Ok(body)
}

fn map_http_error(error: ureq::Error) -> ClientError {
    match error {
        ureq::Error::Timeout(_) => ClientError::Timeout,
        other => ClientError::Transport(other.to_string()),
    }
}

#[derive(Debug, Serialize)]
struct GenerateRequest<'a> {
    model: &'a str,
    prompt: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<&'a str>,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    keep_alive: Option<String>,
    options: GenerateOptions,
}

#[derive(Debug, Serialize)]
struct GenerateOptions {
    temperature: f32,
}

#[derive(Debug, Deserialize)]
struct GenerateResponse {
    #[serde(default)]
    response: String,
    #[serde(default)]
    done: bool,
    #[serde(default)]
    error: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FormatResult {
    Formatted {
        text: String,
        model: ModelName,
    },
    /// The exact recognizer transcript is returned whenever local cleanup is unsafe.
    Fallback {
        text: String,
        reason: FallbackReason,
    },
}

impl FormatResult {
    pub fn text(&self) -> &str {
        match self {
            Self::Formatted { text, .. } | Self::Fallback { text, .. } => text,
        }
    }

    pub fn is_fallback(&self) -> bool {
        matches!(self, Self::Fallback { .. })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FallbackReason {
    RawProfile,
    PromptRejected(PromptError),
    Cancelled,
    ServiceUnavailable(String),
    OutputRejected(ValidationError),
}

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("invalid Ollama endpoint: {0}")]
    InvalidEndpoint(String),
    #[error("all Ollama timeouts must be greater than zero")]
    InvalidTimeout,
    #[error("Ollama operation was cancelled")]
    Cancelled,
    #[error("Ollama request timed out")]
    Timeout,
    #[error("Ollama transport failed: {0}")]
    Transport(String),
    #[error("Ollama returned HTTP status {0}")]
    HttpStatus(u16),
    #[error("Ollama response exceeded the {0}-byte safety limit")]
    ResponseTooLarge(usize),
    #[error("failed while reading the Ollama response: {0}")]
    ReadResponse(std::io::Error),
    #[error("failed to serialize the Ollama request: {0}")]
    SerializeRequest(serde_json::Error),
    #[error("Ollama returned invalid JSON: {0}")]
    InvalidJson(serde_json::Error),
    #[error("Ollama reported an error: {0}")]
    Api(String),
    #[error("Ollama generation ended without a completed response")]
    IncompleteResponse,
    #[error("Ollama returned an invalid model catalog: {0}")]
    InvalidCatalog(ModelSelectionError),
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::net::{TcpListener, TcpStream};
    use std::sync::mpsc::{self, Receiver};
    use std::thread::{self, JoinHandle};

    use super::*;
    use crate::SelectionPolicy;

    #[derive(Debug)]
    struct CapturedRequest {
        method: String,
        path: String,
        body: Vec<u8>,
    }

    struct FakeResponse {
        status: u16,
        body: Vec<u8>,
        delay: Duration,
    }

    impl FakeResponse {
        fn json(body: &str) -> Self {
            Self {
                status: 200,
                body: body.as_bytes().to_vec(),
                delay: Duration::ZERO,
            }
        }
    }

    struct FakeServer {
        endpoint: OllamaEndpoint,
        requests: Receiver<CapturedRequest>,
        thread: Option<JoinHandle<()>>,
    }

    impl FakeServer {
        fn start(responses: Vec<FakeResponse>) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            let (send, requests) = mpsc::channel();
            let thread = thread::spawn(move || {
                for response in responses {
                    let (mut stream, _) = listener.accept().unwrap();
                    let request = read_request(&mut stream);
                    send.send(request).unwrap();
                    if !response.delay.is_zero() {
                        thread::sleep(response.delay);
                    }
                    let reason = if response.status == 200 {
                        "OK"
                    } else {
                        "Error"
                    };
                    let headers = format!(
                        "HTTP/1.1 {} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        response.status,
                        reason,
                        response.body.len()
                    );
                    if stream.write_all(headers.as_bytes()).is_ok() {
                        let _ = stream.write_all(&response.body);
                    }
                }
            });
            Self {
                endpoint: OllamaEndpoint::ipv4(port),
                requests,
                thread: Some(thread),
            }
        }

        fn request(&self) -> CapturedRequest {
            self.requests.recv_timeout(Duration::from_secs(2)).unwrap()
        }
    }

    impl Drop for FakeServer {
        fn drop(&mut self) {
            if let Some(thread) = self.thread.take() {
                thread.join().unwrap();
            }
        }
    }

    fn read_request(stream: &mut TcpStream) -> CapturedRequest {
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut data = Vec::new();
        let mut buffer = [0_u8; 1024];
        let header_end = loop {
            let count = stream.read(&mut buffer).unwrap();
            assert!(count > 0, "client disconnected before headers completed");
            data.extend_from_slice(&buffer[..count]);
            if let Some(index) = data.windows(4).position(|window| window == b"\r\n\r\n") {
                break index + 4;
            }
        };
        let headers = String::from_utf8_lossy(&data[..header_end]);
        let first = headers.lines().next().unwrap();
        let mut request_line = first.split_whitespace();
        let method = request_line.next().unwrap().to_owned();
        let path = request_line.next().unwrap().to_owned();
        let content_length = headers
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().unwrap())
            })
            .unwrap_or(0);
        while data.len() - header_end < content_length {
            let count = stream.read(&mut buffer).unwrap();
            assert!(count > 0, "client disconnected before body completed");
            data.extend_from_slice(&buffer[..count]);
        }
        CapturedRequest {
            method,
            path,
            body: data[header_end..header_end + content_length].to_vec(),
        }
    }

    fn client(server: &FakeServer) -> OllamaClient {
        OllamaClient::new(server.endpoint.clone(), ClientTimeouts::default()).unwrap()
    }

    fn model() -> ModelName {
        ModelName::parse("qwen2.5:3b").unwrap()
    }

    #[test]
    fn endpoints_accept_only_canonicalized_loopback_http() {
        assert_eq!(
            OllamaEndpoint::parse("http://localhost:11434")
                .unwrap()
                .as_str(),
            "http://127.0.0.1:11434"
        );
        assert_eq!(
            OllamaEndpoint::parse("http://[::1]").unwrap().as_str(),
            "http://[::1]:11434"
        );
        for rejected in [
            "https://127.0.0.1:11434",
            "http://192.168.1.2:11434",
            "http://example.com",
            "http://127.0.0.1:11434/path",
            "http://user@127.0.0.1:11434",
            "http://127.0.0.1:0",
        ] {
            assert!(
                OllamaEndpoint::parse(rejected).is_err(),
                "accepted {rejected}"
            );
        }
    }

    #[test]
    fn discovers_and_selects_models_from_tags() {
        let server = FakeServer::start(vec![FakeResponse::json(
            r#"{"models":[{"name":"zeta:7b","size":7000,"digest":"abc","details":{"family":"qwen"}},{"name":"alpha:1b"}]}"#,
        )]);
        let catalog = client(&server).discover(&CancellationToken::new()).unwrap();
        assert_eq!(catalog.models().len(), 2);
        assert_eq!(
            catalog
                .select(&SelectionPolicy::FirstAvailable)
                .unwrap()
                .name
                .as_str(),
            "alpha:1b"
        );
        let request = server.request();
        assert_eq!(request.method, "GET");
        assert_eq!(request.path, "/api/tags");
    }

    #[test]
    fn generation_is_non_streaming_deterministic_and_sets_lifecycle() {
        let server = FakeServer::start(vec![FakeResponse::json(
            r#"{"model":"qwen2.5:3b","response":"Hello.","done":true}"#,
        )]);
        let result = client(&server)
            .generate(
                &model(),
                "hello",
                Some("format it"),
                KeepAlive::For(Duration::from_secs(300)),
                &CancellationToken::new(),
            )
            .unwrap();
        assert_eq!(result, "Hello.");
        let request = server.request();
        assert_eq!(request.method, "POST");
        assert_eq!(request.path, "/api/generate");
        let json: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
        assert_eq!(json["stream"], false);
        assert_eq!(json["keep_alive"], "300s");
        assert_eq!(json["options"]["temperature"], 0.0);
        assert_eq!(json["system"], "format it");
    }

    #[test]
    fn warm_up_and_unload_use_empty_generation_requests() {
        let server = FakeServer::start(vec![
            FakeResponse::json(r#"{"response":"","done":true}"#),
            FakeResponse::json(r#"{"response":"","done":true}"#),
        ]);
        let client = client(&server);
        client
            .warm_up(&model(), KeepAlive::Indefinite, &CancellationToken::new())
            .unwrap();
        client.unload(&model(), &CancellationToken::new()).unwrap();
        let warm: serde_json::Value = serde_json::from_slice(&server.request().body).unwrap();
        let unload: serde_json::Value = serde_json::from_slice(&server.request().body).unwrap();
        assert_eq!(warm["prompt"], "");
        assert_eq!(warm["keep_alive"], "-1");
        assert_eq!(unload["prompt"], "");
        assert_eq!(unload["keep_alive"], "0s");
    }

    #[test]
    fn valid_formatting_is_accepted() {
        let server = FakeServer::start(vec![FakeResponse::json(
            r#"{"response":"Visit https://example.com in version v2.","done":true}"#,
        )]);
        let result = client(&server).format(
            &model(),
            "visit https://example.com in version v2",
            &FormatProfile::Light,
            KeepAlive::ServerDefault,
            &CancellationToken::new(),
        );
        assert_eq!(
            result,
            FormatResult::Formatted {
                text: "Visit https://example.com in version v2.".to_owned(),
                model: model(),
            }
        );
    }

    #[test]
    fn raw_cancellation_service_failure_and_bad_output_fall_back_exactly() {
        let transcript = "use API_KEY_2";
        let unused = FakeServer::start(Vec::new());
        let raw = client(&unused).format(
            &model(),
            transcript,
            &FormatProfile::Raw,
            KeepAlive::ServerDefault,
            &CancellationToken::new(),
        );
        assert_eq!(
            raw,
            FormatResult::Fallback {
                text: transcript.to_owned(),
                reason: FallbackReason::RawProfile,
            }
        );

        let cancelled = CancellationToken::new();
        cancelled.cancel();
        let cancelled_result = client(&unused).format(
            &model(),
            transcript,
            &FormatProfile::Light,
            KeepAlive::ServerDefault,
            &cancelled,
        );
        assert_eq!(
            cancelled_result,
            FormatResult::Fallback {
                text: transcript.to_owned(),
                reason: FallbackReason::Cancelled,
            }
        );

        let bad = FakeServer::start(vec![FakeResponse::json(
            r#"{"response":"use API key two","done":true}"#,
        )]);
        let bad_result = client(&bad).format(
            &model(),
            transcript,
            &FormatProfile::Light,
            KeepAlive::ServerDefault,
            &CancellationToken::new(),
        );
        assert_eq!(bad_result.text(), transcript);
        assert!(matches!(
            bad_result,
            FormatResult::Fallback {
                reason: FallbackReason::OutputRejected(_),
                ..
            }
        ));

        let closed_port = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = closed_port.local_addr().unwrap().port();
        drop(closed_port);
        let offline = OllamaClient::new(
            OllamaEndpoint::ipv4(port),
            ClientTimeouts {
                connect: Duration::from_millis(100),
                response_headers: Duration::from_millis(100),
                response_body: Duration::from_millis(100),
                overall: Duration::from_millis(250),
            },
        )
        .unwrap();
        let unavailable = offline.format(
            &model(),
            transcript,
            &FormatProfile::Light,
            KeepAlive::ServerDefault,
            &CancellationToken::new(),
        );
        assert_eq!(unavailable.text(), transcript);
        assert!(matches!(
            unavailable,
            FormatResult::Fallback {
                reason: FallbackReason::ServiceUnavailable(_),
                ..
            }
        ));
    }

    #[test]
    fn status_malformed_incomplete_and_api_errors_are_typed() {
        let server = FakeServer::start(vec![
            FakeResponse {
                status: 500,
                body: b"{}".to_vec(),
                delay: Duration::ZERO,
            },
            FakeResponse::json("not-json"),
            FakeResponse::json(r#"{"response":"partial","done":false}"#),
            FakeResponse::json(r#"{"error":"model not found","done":true}"#),
        ]);
        let client = client(&server);
        assert!(matches!(
            client.discover(&CancellationToken::new()),
            Err(ClientError::HttpStatus(500))
        ));
        assert!(matches!(
            client.discover(&CancellationToken::new()),
            Err(ClientError::InvalidJson(_))
        ));
        assert!(matches!(
            client.generate(
                &model(),
                "x",
                None,
                KeepAlive::ServerDefault,
                &CancellationToken::new()
            ),
            Err(ClientError::IncompleteResponse)
        ));
        assert!(matches!(
            client.generate(
                &model(),
                "x",
                None,
                KeepAlive::ServerDefault,
                &CancellationToken::new()
            ),
            Err(ClientError::Api(message)) if message == "model not found"
        ));
    }

    #[test]
    fn response_wait_is_bounded_by_timeout() {
        let server = FakeServer::start(vec![FakeResponse {
            status: 200,
            body: br#"{"models":[]}"#.to_vec(),
            delay: Duration::from_millis(200),
        }]);
        let client = OllamaClient::new(
            server.endpoint.clone(),
            ClientTimeouts {
                connect: Duration::from_millis(50),
                response_headers: Duration::from_millis(50),
                response_body: Duration::from_millis(50),
                overall: Duration::from_millis(100),
            },
        )
        .unwrap();
        assert!(matches!(
            client.discover(&CancellationToken::new()),
            Err(ClientError::Timeout) | Err(ClientError::ReadResponse(_))
        ));
    }
}
