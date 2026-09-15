use crate::{AssistantError, CancellationToken, ProtectedSecret, agent, endpoint_uri};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Read};

const MAX_INPUT: usize = 512 * 1024;
const MAX_OUTPUT: usize = 512 * 1024;
const MAX_EVENT: usize = 256 * 1024;
const MAX_WIRE: usize = 16 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    #[default]
    Ollama,
    OpenAi,
    Anthropic,
}
impl std::fmt::Display for ProviderKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Ollama => "Local Ollama",
            Self::OpenAi => "OpenAI",
            Self::Anthropic => "Anthropic",
        })
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ProviderConfig {
    pub kind: ProviderKind,
    /// Explicit user-selected model. Empty is allowed in saved configuration, not at execution.
    pub model: String,
    /// Base URL only, e.g. http://127.0.0.1:11434. Cloud endpoints are never configurable.
    pub ollama_endpoint: String,
    pub credential: Option<ProtectedSecret>,
    pub max_output_tokens: u32,
}
impl Default for ProviderConfig {
    fn default() -> Self {
        Self {
            kind: ProviderKind::Ollama,
            model: String::new(),
            ollama_endpoint: "http://127.0.0.1:11434".into(),
            credential: None,
            max_output_tokens: 2048,
        }
    }
}
impl ProviderConfig {
    pub fn validate(&self) -> Result<(), AssistantError> {
        if self.model.len() > 256
            || self
                .model
                .chars()
                .any(|c| c.is_control() || c.is_whitespace())
            || !(1..=32_768).contains(&self.max_output_tokens)
        {
            return Err(AssistantError::InvalidConfig);
        }
        let uri = endpoint_uri(&self.ollama_endpoint, true)?;
        if uri.path() != "/" || uri.query().is_some() {
            return Err(AssistantError::InvalidConfig);
        }
        // Ollama can proxy cloud-tagged models. Local mode must not knowingly opt into it.
        let name = self.model.to_ascii_lowercase();
        if self.kind == ProviderKind::Ollama
            && (name.ends_with(":cloud") || name.ends_with("-cloud"))
        {
            return Err(AssistantError::InvalidConfig);
        }
        Ok(())
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChatRole {
    System,
    User,
    Assistant,
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: ChatRole,
    pub content: String,
}
impl std::fmt::Debug for ChatMessage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChatMessage")
            .field("role", &self.role)
            .field("content", &"[redacted]")
            .finish()
    }
}

/// Runs on a host worker, never the audio/UI thread. All deltas belong to this request.
/// No automatic retries, tools, hidden retrieval, provider fallback or prompt logging.
pub fn stream_chat(
    config: &ProviderConfig,
    messages: &[ChatMessage],
    cancellation: &CancellationToken,
    mut on_delta: impl FnMut(&str),
) -> Result<(), AssistantError> {
    cancellation.check()?;
    let (endpoint, body) = build_request(config, messages)?;
    let client = agent(cancellation);
    if config.kind == ProviderKind::Ollama {
        validate_local_model(&client, config, cancellation)?;
    }
    let mut request = client
        .post(&endpoint)
        .header("Content-Type", "application/json")
        .header(
            "Accept",
            if config.kind == ProviderKind::Ollama {
                "application/x-ndjson"
            } else {
                "text/event-stream"
            },
        );
    let credential = if config.kind == ProviderKind::Ollama {
        None
    } else {
        Some(
            config
                .credential
                .as_ref()
                .ok_or(AssistantError::MissingCredential)?
                .expose()?,
        )
    };
    if let Some(secret) = &credential {
        request = match config.kind {
            ProviderKind::OpenAi => request.header(
                "Authorization",
                crate::secret_header(&format!("Bearer {}", secret.as_str()))?,
            ),
            ProviderKind::Anthropic => request
                .header("x-api-key", crate::secret_header(secret.as_str())?)
                .header("anthropic-version", "2023-06-01"),
            ProviderKind::Ollama => request,
        };
    }
    cancellation.check()?;
    let response = request.send(body.as_bytes());
    cancellation.check()?;
    let mut response = response.map_err(|_| AssistantError::Transport)?;
    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        return Err(AssistantError::HttpStatus(status));
    }
    let reader = BufReader::new(response.body_mut().as_reader());
    parse_stream(reader, config.kind, cancellation, &mut on_delta)
}

fn validate_local_model(
    client: &ureq::Agent,
    config: &ProviderConfig,
    token: &CancellationToken,
) -> Result<(), AssistantError> {
    token.check()?;
    let endpoint = format!("{}/api/show", config.ollama_endpoint.trim_end_matches('/'));
    // Only a model identifier is sent during this check, never conversation content.
    let body = json!({"model":config.model,"verbose":false}).to_string();
    let result = client
        .post(endpoint)
        .header("Content-Type", "application/json")
        .send(body.as_bytes());
    token.check()?;
    let mut response = result.map_err(|_| AssistantError::Transport)?;
    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        return Err(AssistantError::HttpStatus(status));
    }
    let mut bytes = Vec::new();
    let read = response
        .body_mut()
        .as_reader()
        .take(2 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes);
    token.check()?;
    read.map_err(|_| AssistantError::Transport)?;
    if bytes.len() > 2 * 1024 * 1024 {
        return Err(AssistantError::SizeLimit);
    }
    let value: Value = serde_json::from_slice(&bytes).map_err(|_| AssistantError::Protocol)?;
    let remote = ["remote_host", "remote_model", "remote_url"]
        .iter()
        .any(|key| {
            value
                .get(*key)
                .is_some_and(|v| !v.is_null() && v.as_str() != Some(""))
        });
    let capable = value
        .get("capabilities")
        .and_then(Value::as_array)
        .is_some_and(|v| v.iter().any(|c| c.as_str() == Some("completion")));
    if remote || !capable {
        return Err(AssistantError::InvalidConfig);
    }
    Ok(())
}

fn build_request(
    config: &ProviderConfig,
    messages: &[ChatMessage],
) -> Result<(String, String), AssistantError> {
    config.validate()?;
    if config.model.is_empty()
        || messages.is_empty()
        || !messages.iter().any(|m| m.role == ChatRole::User)
    {
        return Err(AssistantError::InvalidConfig);
    }
    if messages.len() > 1024
        || messages
            .iter()
            .try_fold(0usize, |n, m| n.checked_add(m.content.len()))
            .is_none_or(|n| n > MAX_INPUT)
    {
        return Err(AssistantError::SizeLimit);
    }
    let (endpoint, body) = match config.kind {
        ProviderKind::Ollama => (
            format!("{}/api/chat", config.ollama_endpoint.trim_end_matches('/')),
            json!({
                "model":config.model, "messages":messages, "stream":true,
                "options":{"num_predict":config.max_output_tokens}, "keep_alive":"5m"
            }),
        ),
        ProviderKind::OpenAi => (
            "https://api.openai.com/v1/responses".into(),
            json!({
                "model":config.model, "input":messages, "stream":true,
                "store":false, "max_output_tokens":config.max_output_tokens
            }),
        ),
        ProviderKind::Anthropic => {
            let system = messages
                .iter()
                .filter(|m| m.role == ChatRole::System)
                .map(|m| m.content.as_str())
                .collect::<Vec<_>>()
                .join("\n\n");
            let turns: Vec<_> = messages
                .iter()
                .filter(|m| m.role != ChatRole::System)
                .collect();
            (
                "https://api.anthropic.com/v1/messages".into(),
                json!({
                    "model":config.model, "system":system, "messages":turns,
                    "stream":true, "max_tokens":config.max_output_tokens
                }),
            )
        }
    };
    let encoded = serde_json::to_string(&body).map_err(|_| AssistantError::Protocol)?;
    if encoded.len() > MAX_INPUT * 6 + 16_384 {
        return Err(AssistantError::SizeLimit);
    }
    Ok((endpoint, encoded))
}

fn read_bounded_line(
    reader: &mut impl BufRead,
    total: &mut usize,
) -> Result<Option<String>, AssistantError> {
    let mut bytes = Vec::new();
    loop {
        let available = reader.fill_buf().map_err(|_| AssistantError::Transport)?;
        if available.is_empty() {
            break;
        }
        let end = available.iter().position(|b| *b == b'\n').map(|p| p + 1);
        let count = end.unwrap_or(available.len());
        if bytes.len() + count > MAX_EVENT || *total + count > MAX_WIRE {
            return Err(AssistantError::SizeLimit);
        }
        bytes.extend_from_slice(&available[..count]);
        *total += count;
        reader.consume(count);
        if end.is_some() {
            break;
        }
    }
    if bytes.is_empty() {
        return Ok(None);
    }
    let line = String::from_utf8(bytes).map_err(|_| AssistantError::Protocol)?;
    Ok(Some(line.trim_end_matches(['\r', '\n']).to_owned()))
}

fn parse_stream(
    mut reader: impl BufRead,
    kind: ProviderKind,
    cancellation: &CancellationToken,
    on_delta: &mut impl FnMut(&str),
) -> Result<(), AssistantError> {
    let mut wire = 0;
    let mut output = 0;
    let mut data = String::new();
    let mut anthropic_stop = None;
    loop {
        cancellation.check()?;
        let line = read_bounded_line(&mut reader, &mut wire);
        cancellation.check()?;
        let Some(line) = line? else {
            return Err(AssistantError::Incomplete);
        };
        let event = if kind == ProviderKind::Ollama {
            if line.is_empty() {
                continue;
            }
            line
        } else if line.is_empty() {
            if data.is_empty() {
                continue;
            }
            std::mem::take(&mut data)
        } else {
            if let Some(value) = line.strip_prefix("data:") {
                let value = value.strip_prefix(' ').unwrap_or(value);
                if data.len() + value.len() + 1 > MAX_EVENT {
                    return Err(AssistantError::SizeLimit);
                }
                if !data.is_empty() {
                    data.push('\n');
                }
                data.push_str(value);
            }
            continue;
        };
        if event == "[DONE]" {
            return Err(AssistantError::Incomplete);
        }
        let value: Value = serde_json::from_str(&event).map_err(|_| AssistantError::Protocol)?;
        if !value.is_object() || value.get("error").is_some() {
            return Err(AssistantError::Protocol);
        }
        let mut complete = false;
        let mut incomplete = false;
        let text = match kind {
            ProviderKind::Ollama => {
                let done = value
                    .get("done")
                    .and_then(Value::as_bool)
                    .ok_or(AssistantError::Protocol)?;
                if done {
                    match value.get("done_reason").and_then(Value::as_str) {
                        Some("stop") => complete = true,
                        Some("length") => incomplete = true,
                        _ => return Err(AssistantError::Incomplete),
                    }
                }
                value.pointer("/message/content").and_then(Value::as_str)
            }
            ProviderKind::OpenAi => match value
                .get("type")
                .and_then(Value::as_str)
                .ok_or(AssistantError::Protocol)?
            {
                "response.output_text.delta" | "response.refusal.delta" => Some(
                    value
                        .get("delta")
                        .and_then(Value::as_str)
                        .ok_or(AssistantError::Protocol)?,
                ),
                "response.completed" => {
                    complete = value.pointer("/response/status").and_then(Value::as_str)
                        == Some("completed");
                    if !complete {
                        return Err(AssistantError::Incomplete);
                    }
                    None
                }
                "response.failed" | "response.incomplete" | "error" => {
                    return Err(AssistantError::Incomplete);
                }
                _ => None,
            },
            ProviderKind::Anthropic => match value
                .get("type")
                .and_then(Value::as_str)
                .ok_or(AssistantError::Protocol)?
            {
                "content_block_delta"
                    if value.pointer("/delta/type").and_then(Value::as_str)
                        == Some("text_delta") =>
                {
                    Some(
                        value
                            .pointer("/delta/text")
                            .and_then(Value::as_str)
                            .ok_or(AssistantError::Protocol)?,
                    )
                }
                "message_delta" => {
                    if let Some(reason) =
                        value.pointer("/delta/stop_reason").and_then(Value::as_str)
                    {
                        anthropic_stop = Some(reason.to_owned());
                    }
                    None
                }
                "message_stop" => {
                    match anthropic_stop.as_deref() {
                        Some("end_turn" | "stop_sequence" | "refusal") => complete = true,
                        _ => return Err(AssistantError::Incomplete),
                    }
                    None
                }
                "error" => return Err(AssistantError::Protocol),
                _ => None,
            },
        };
        if let Some(text) = text.filter(|s| !s.is_empty()) {
            output += text.len();
            if output > MAX_OUTPUT {
                return Err(AssistantError::SizeLimit);
            }
            cancellation.check()?;
            on_delta(text);
        }
        cancellation.check()?;
        if incomplete {
            return Err(AssistantError::Incomplete);
        }
        if complete {
            return if output == 0 {
                Err(AssistantError::Incomplete)
            } else {
                Ok(())
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn parse(kind: ProviderKind, stream: &str) -> (Result<(), AssistantError>, String) {
        let mut output = String::new();
        let result = parse_stream(
            stream.as_bytes(),
            kind,
            &CancellationToken::default(),
            &mut |text| output.push_str(text),
        );
        (result, output)
    }
    #[test]
    fn local_defaults_and_endpoint_policy() {
        assert!(ProviderConfig::default().validate().is_ok());
        for endpoint in [
            "http://localhost:11434",
            "https://evil.test",
            "http://127.0.0.1@evil.test",
            "http://127.0.0.1/a",
            "http://127.0.0.1?key=x",
            "http://127.0.0.1#x",
            "http://127.0.0.1:0",
        ] {
            assert!(
                ProviderConfig {
                    ollama_endpoint: endpoint.into(),
                    ..Default::default()
                }
                .validate()
                .is_err(),
                "{endpoint}"
            );
        }
        for endpoint in [
            "http://127.0.0.1:11434",
            "http://[::1]:11434/",
            "https://127.0.0.1:443",
        ] {
            assert!(
                ProviderConfig {
                    ollama_endpoint: endpoint.into(),
                    ..Default::default()
                }
                .validate()
                .is_ok(),
                "{endpoint}"
            );
        }
    }
    #[test]
    fn local_cloud_tags_and_empty_run_rejected() {
        for model in ["a:cloud", "a-cloud", "a b", "a\n"] {
            assert!(
                ProviderConfig {
                    model: model.into(),
                    ..Default::default()
                }
                .validate()
                .is_err()
            );
        }
        assert!(
            build_request(
                &ProviderConfig::default(),
                &[ChatMessage {
                    role: ChatRole::User,
                    content: "hi".into()
                }]
            )
            .is_err()
        );
    }
    #[test]
    fn explicit_models_and_stateless_openai_payload() {
        let config = ProviderConfig {
            kind: ProviderKind::OpenAi,
            model: "user-model-id".into(),
            ..Default::default()
        };
        let (url, body) = build_request(
            &config,
            &[ChatMessage {
                role: ChatRole::User,
                content: "synthetic".into(),
            }],
        )
        .unwrap();
        let json: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(url, "https://api.openai.com/v1/responses");
        assert_eq!(json["store"], false);
        assert_eq!(json["model"], "user-model-id");
        assert!(json.get("tools").is_none());
        assert!(json.get("previous_response_id").is_none());
    }
    #[test]
    fn ollama_deltas_and_terminal_required() {
        let stream = "{\"message\":{\"content\":\"Olá\"},\"done\":false}\n{\"message\":{\"content\":\"!\"},\"done\":true,\"done_reason\":\"stop\"}\n";
        assert_eq!(parse(ProviderKind::Ollama, stream), (Ok(()), "Olá!".into()));
        assert_eq!(
            parse(
                ProviderKind::Ollama,
                "{\"message\":{\"content\":\"part\"},\"done\":false}\n"
            )
            .0,
            Err(AssistantError::Incomplete)
        );
        assert_eq!(
            parse(ProviderKind::Ollama, &stream.replace("stop", "length")).0,
            Err(AssistantError::Incomplete)
        );
    }
    #[test]
    fn openai_sse_crlf_comments_and_multiline() {
        let stream = ": keepalive\r\nevent: response.output_text.delta\r\ndata: {\"type\":\"response.output_text.delta\",\r\ndata: \"delta\":\"hello\"}\r\n\r\ndata: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\"}}\r\n\r\n";
        assert_eq!(
            parse(ProviderKind::OpenAi, stream),
            (Ok(()), "hello".into())
        );
        assert_eq!(
            parse(ProviderKind::OpenAi, "data: [DONE]\n\n").0,
            Err(AssistantError::Incomplete)
        );
        assert_eq!(
            parse(
                ProviderKind::OpenAi,
                "data: {\"type\":\"response.incomplete\"}\n\n"
            )
            .0,
            Err(AssistantError::Incomplete)
        );
    }
    #[test]
    fn anthropic_requires_valid_stop_reason_and_stop_event() {
        let stream = "data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"answer\"}}\n\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"}}\n\ndata: {\"type\":\"message_stop\"}\n\n";
        assert_eq!(
            parse(ProviderKind::Anthropic, stream),
            (Ok(()), "answer".into())
        );
        assert_eq!(
            parse(
                ProviderKind::Anthropic,
                &stream.replace("end_turn", "max_tokens")
            )
            .0,
            Err(AssistantError::Incomplete)
        );
        assert_eq!(
            parse(
                ProviderKind::Anthropic,
                "data: {\"type\":\"message_stop\"}\n\n"
            )
            .0,
            Err(AssistantError::Incomplete)
        );
    }
    #[test]
    fn cancellation_between_deltas_never_delivers_late_text() {
        let token = CancellationToken::default();
        let mut output = String::new();
        let stream = "{\"message\":{\"content\":\"first\"},\"done\":false}\n{\"message\":{\"content\":\"late\"},\"done\":true,\"done_reason\":\"stop\"}\n";
        let result = parse_stream(stream.as_bytes(), ProviderKind::Ollama, &token, &mut |s| {
            output.push_str(s);
            token.cancel();
        });
        assert_eq!(result, Err(AssistantError::Cancelled));
        assert_eq!(output, "first");
    }
    #[test]
    fn bounded_unterminated_event_and_invalid_utf8() {
        assert_eq!(
            parse(ProviderKind::Ollama, &"a".repeat(MAX_EVENT + 1)).0,
            Err(AssistantError::SizeLimit)
        );
        assert_eq!(
            parse_stream(
                &b"\xff\n"[..],
                ProviderKind::Ollama,
                &CancellationToken::default(),
                &mut |_| {}
            ),
            Err(AssistantError::Protocol)
        );
    }
    #[test]
    fn context_limit_is_explicit_not_silent_truncation() {
        let config = ProviderConfig {
            model: "synthetic".into(),
            ..Default::default()
        };
        assert_eq!(
            build_request(
                &config,
                &[ChatMessage {
                    role: ChatRole::User,
                    content: "a".repeat(MAX_INPUT + 1)
                }]
            ),
            Err(AssistantError::SizeLimit)
        );
    }
    #[test]
    fn unknown_events_do_not_finish_and_error_content_is_not_returned() {
        let stream = "data: {\"type\":\"new_future_event\",\"secret\":\"secret-response\"}\n\ndata: {\"type\":\"error\",\"error\":{\"message\":\"secret-response\"}}\n\n";
        let (result, output) = parse(ProviderKind::OpenAi, stream);
        assert_eq!(result, Err(AssistantError::Protocol));
        assert!(output.is_empty());
        assert!(!format!("{}", result.unwrap_err()).contains("secret-response"));
    }
    #[test]
    fn malformed_terminal_and_empty_answer_are_not_success() {
        for stream in [
            "data: {\"type\":\"response.completed\"}\n\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\"}}\n\n",
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"part\"}\n\ndata: {\"type\":\"response.completed\",\"response\":{\"status\":\"failed\"}}\n\n",
        ] {
            assert_eq!(
                parse(ProviderKind::OpenAi, stream).0,
                Err(AssistantError::Incomplete)
            );
        }
        assert_eq!(
            parse(
                ProviderKind::Ollama,
                "{\"done\":true,\"message\":{\"content\":\"part\"}}\n"
            )
            .0,
            Err(AssistantError::Incomplete)
        );
    }
    #[test]
    fn pre_cancelled_cloud_request_never_needs_credentials_or_network() {
        let config = ProviderConfig {
            kind: ProviderKind::OpenAi,
            model: "synthetic-model".into(),
            ..Default::default()
        };
        let token = CancellationToken::default();
        token.cancel();
        assert_eq!(
            stream_chat(
                &config,
                &[ChatMessage {
                    role: ChatRole::User,
                    content: "synthetic".into()
                }],
                &token,
                |_| {}
            ),
            Err(AssistantError::Cancelled)
        );
    }
    #[test]
    fn cloud_requires_credentials_before_network() {
        for kind in [ProviderKind::OpenAi, ProviderKind::Anthropic] {
            let config = ProviderConfig {
                kind,
                model: "synthetic-model".into(),
                ..Default::default()
            };
            assert_eq!(
                stream_chat(
                    &config,
                    &[ChatMessage {
                        role: ChatRole::User,
                        content: "synthetic".into()
                    }],
                    &CancellationToken::default(),
                    |_| {}
                ),
                Err(AssistantError::MissingCredential)
            );
        }
    }
    #[test]
    fn anthropic_system_instructions_are_separate_from_turns() {
        let config = ProviderConfig {
            kind: ProviderKind::Anthropic,
            model: "user-model-id".into(),
            ..Default::default()
        };
        let (endpoint, body) = build_request(
            &config,
            &[
                ChatMessage {
                    role: ChatRole::System,
                    content: "preset".into(),
                },
                ChatMessage {
                    role: ChatRole::User,
                    content: "question".into(),
                },
                ChatMessage {
                    role: ChatRole::Assistant,
                    content: "previous".into(),
                },
            ],
        )
        .unwrap();
        assert_eq!(endpoint, "https://api.anthropic.com/v1/messages");
        let body: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(body["system"], "preset");
        assert_eq!(body["messages"].as_array().unwrap().len(), 2);
        assert_eq!(body["messages"][0]["role"], "user");
        assert!(body.get("tools").is_none());
    }
}
