use crate::{AssistantError, CancellationToken, ProtectedSecret, agent, endpoint_uri};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionPayloadMode {
    #[default]
    Template,
    AiJson,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum ActionMethod {
    Get,
    #[default]
    Post,
    Put,
    Patch,
    Delete,
}
impl ActionMethod {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Post => "POST",
            Self::Put => "PUT",
            Self::Patch => "PATCH",
            Self::Delete => "DELETE",
        }
    }
}
impl std::fmt::Display for ActionMethod {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionHeader {
    pub name: String,
    pub value: ProtectedSecret,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ActionConfig {
    pub id: String,
    pub name: String,
    pub endpoint: String,
    pub method: ActionMethod,
    /// All custom header values are encrypted, not merely recognized authentication headers.
    pub headers: Vec<ActionHeader>,
    /// JSON template. {{text}} and {{delivery_id}} expand inside string values only.
    /// GET sends percent-encoded text and delivery_id query parameters instead.
    pub payload_template: String,
    /// Optional launcher key. Keys 1 and 2 belong to dictation and meetings.
    pub launcher_slot: Option<u8>,
    pub payload_mode: ActionPayloadMode,
    /// Explicit local Ollama model; never inherits a paid assistant provider.
    pub payload_model: String,
    pub payload_prompt: String,
    /// Entire Authorization value, e.g. "Bearer ...". Never put credentials in endpoint URLs.
    pub authorization: Option<ProtectedSecret>,
}
impl std::fmt::Debug for ActionConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ActionConfig")
            .field("method", &self.method)
            .field("configuration", &"[redacted]")
            .finish()
    }
}
impl Default for ActionConfig {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            endpoint: String::new(),
            method: ActionMethod::Post,
            headers: Vec::new(),
            payload_template: r#"{"text":"{{text}}","delivery_id":"{{delivery_id}}"}"#.into(),
            launcher_slot: None,
            payload_mode: ActionPayloadMode::Template,
            payload_model: String::new(),
            payload_prompt: String::new(),
            authorization: None,
        }
    }
}
impl ActionConfig {
    pub fn validate(&self) -> Result<(), AssistantError> {
        if !valid_id(&self.id)
            || self.name.trim().is_empty()
            || self.name.len() > 120
            || self.name.chars().any(char::is_control)
            || self.headers.len() > 24
            || self
                .launcher_slot
                .is_some_and(|slot| !(3..=9).contains(&slot))
            || self.payload_prompt.len() > 65_536
            || self.payload_model.len() > 256
            || self
                .payload_model
                .chars()
                .any(|c| c.is_control() || c.is_whitespace())
        {
            return Err(AssistantError::InvalidConfig);
        }
        endpoint_uri(&self.endpoint, false)?;
        let mut names = std::collections::HashSet::new();
        for header in &self.headers {
            let name = header.name.to_ascii_lowercase();
            if header.name.len() > 128
                || !names.insert(name.clone())
                || header.name.parse::<ureq::http::HeaderName>().is_err()
                || matches!(
                    name.as_str(),
                    "authorization"
                        | "host"
                        | "content-length"
                        | "transfer-encoding"
                        | "connection"
                        | "upgrade"
                        | "expect"
                        | "proxy-authorization"
                        | "proxy-connection"
                        | "content-type"
                        | "idempotency-key"
                )
            {
                return Err(AssistantError::InvalidConfig);
            }
        }
        if self.payload_mode == ActionPayloadMode::AiJson {
            if self.method == ActionMethod::Get
                || self.payload_model.is_empty()
                || self.payload_prompt.trim().is_empty()
            {
                return Err(AssistantError::InvalidConfig);
            }
            crate::ProviderConfig {
                model: self.payload_model.clone(),
                ..Default::default()
            }
            .validate()?;
        }
        if self.payload_template.len() > 65_536 {
            return Err(AssistantError::SizeLimit);
        }
        if self.payload_mode == ActionPayloadMode::Template {
            let template: Value = serde_json::from_str(&self.payload_template)
                .map_err(|_| AssistantError::InvalidConfig)?;
            validate_template(&template)?;
        }
        Ok(())
    }
}
fn validate_template(value: &Value) -> Result<(), AssistantError> {
    match value {
        Value::String(s) => {
            let remainder = s.replace("{{text}}", "").replace("{{delivery_id}}", "");
            if remainder.contains("{{") || remainder.contains("}}") {
                return Err(AssistantError::InvalidConfig);
            }
        }
        Value::Array(items) => {
            for item in items {
                validate_template(item)?;
            }
        }
        Value::Object(items) => {
            for (key, value) in items {
                if key.contains("{{") {
                    return Err(AssistantError::InvalidConfig);
                }
                validate_template(value)?;
            }
        }
        _ => {}
    }
    Ok(())
}
pub(crate) fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeliveryReceipt {
    pub delivery_id: String,
    pub status: u16,
}

/// Exactly one attempt. Preserve delivery_id for an explicit retry; the receiving
/// server must implement Idempotency-Key for duplicate prevention to be guaranteed.
pub fn execute_action(
    action: &ActionConfig,
    text: &str,
    delivery_id: &str,
    cancellation: &CancellationToken,
) -> Result<DeliveryReceipt, AssistantError> {
    cancellation.check()?;
    let prepared = crate::prepare_template_action(action, text, delivery_id)?;
    execute_prepared_action(&prepared, cancellation)
}

/// Executes the exact frozen payload again on an explicit retry. No generation,
/// mutation of request configuration, or automatic retry occurs here.
pub fn execute_prepared_action(
    prepared: &crate::PreparedAction,
    cancellation: &CancellationToken,
) -> Result<DeliveryReceipt, AssistantError> {
    cancellation.check()?;
    let action = &prepared.action;
    let delivery_id = &prepared.delivery_id;
    let mut builder = ureq::http::Request::builder()
        .method(action.method.as_str())
        .uri(&prepared.url)
        .header("Idempotency-Key", delivery_id)
        .header("Content-Type", "application/json");
    for header in &action.headers {
        let value = header.value.expose()?;
        builder = builder.header(&header.name, crate::secret_header(value.as_str())?);
    }
    if let Some(auth) = &action.authorization {
        let value = auth.expose()?;
        builder = builder.header("Authorization", crate::secret_header(value.as_str())?);
    }
    let request = builder
        .body(prepared.body.clone())
        .map_err(|_| AssistantError::InvalidConfig)?;
    cancellation.check()?;
    let response = agent(cancellation)
        .run(request)
        .map_err(|_| AssistantError::DeliveryUncertain)?;
    // A cancellation after dispatch cannot revoke delivery. Report the actual status
    // rather than inviting a retry under a misleading "cancelled before send" result.
    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        return Err(AssistantError::HttpStatus(status));
    }
    Ok(DeliveryReceipt {
        delivery_id: delivery_id.into(),
        status,
    })
}

pub(crate) fn action_payload(
    action: &ActionConfig,
    text: &str,
    delivery_id: &str,
) -> Result<(String, String), AssistantError> {
    if action.method == ActionMethod::Get {
        let separator = if action.endpoint.contains('?') {
            '&'
        } else {
            '?'
        };
        let url = format!(
            "{}{separator}text={}&delivery_id={}",
            action.endpoint,
            percent_encode(text),
            percent_encode(delivery_id)
        );
        if url.len() > 8192 {
            return Err(AssistantError::SizeLimit);
        }
        return Ok((url, String::new()));
    }
    let mut value: Value = serde_json::from_str(&action.payload_template)
        .map_err(|_| AssistantError::InvalidConfig)?;
    substitute(&mut value, text, delivery_id, &mut 0)?;
    let body = serde_json::to_string(&value).map_err(|_| AssistantError::InvalidConfig)?;
    if body.len() > 4 * 1024 * 1024 {
        return Err(AssistantError::SizeLimit);
    }
    Ok((action.endpoint.clone(), body))
}
fn substitute(
    value: &mut Value,
    text: &str,
    delivery_id: &str,
    allocated: &mut usize,
) -> Result<(), AssistantError> {
    match value {
        Value::String(s) => {
            // Single pass prevents tokens in user text from becoming executable template syntax.
            let text_count = s.matches("{{text}}").count();
            let id_count = s.matches("{{delivery_id}}").count();
            let projected = s
                .len()
                .saturating_add(text_count.saturating_mul(text.len()))
                .saturating_add(id_count.saturating_mul(delivery_id.len()));
            *allocated = allocated.saturating_add(projected);
            if *allocated > 4 * 1024 * 1024 {
                return Err(AssistantError::SizeLimit);
            }
            let mut rendered = String::new();
            let mut rest = s.as_str();
            while let Some(index) = rest.find("{{") {
                rendered.push_str(&rest[..index]);
                rest = &rest[index..];
                if let Some(next) = rest.strip_prefix("{{text}}") {
                    rendered.push_str(text);
                    rest = next;
                } else if let Some(next) = rest.strip_prefix("{{delivery_id}}") {
                    rendered.push_str(delivery_id);
                    rest = next;
                } else {
                    rendered.push_str("{{");
                    rest = &rest[2..];
                }
            }
            rendered.push_str(rest);
            *s = rendered;
        }
        Value::Array(items) => {
            for item in items {
                substitute(item, text, delivery_id, allocated)?;
            }
        }
        Value::Object(items) => {
            for item in items.values_mut() {
                substitute(item, text, delivery_id, allocated)?;
            }
        }
        _ => {}
    }
    Ok(())
}
fn percent_encode(input: &str) -> String {
    let mut output = String::new();
    for byte in input.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            output.push(byte as char);
        } else {
            use std::fmt::Write;
            let _ = write!(output, "%{byte:02X}");
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    fn action() -> ActionConfig {
        ActionConfig {
            id: "test-action".into(),
            name: "Test".into(),
            endpoint: "http://127.0.0.1:12345/notes".into(),
            ..Default::default()
        }
    }
    #[test]
    fn endpoint_policy_and_model_independence() {
        assert!(action().validate().is_ok());
        for endpoint in [
            "http://example.test/notes",
            "file:///tmp/file",
            "https://user:pass@example.test/",
            "https://example.test/#fragment",
        ] {
            assert!(
                ActionConfig {
                    endpoint: endpoint.into(),
                    ..action()
                }
                .validate()
                .is_err()
            );
        }
    }
    #[test]
    fn json_escaping_and_user_text_are_not_template_programs() {
        let text = "quote\"\n{{delivery_id}} 💬";
        let (_, body) = action_payload(&action(), text, "id-1").unwrap();
        let value: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(value["text"], text);
        assert_eq!(value["delivery_id"], "id-1");
    }
    #[test]
    fn get_percent_encodes_and_limits_urls() {
        let action = ActionConfig {
            method: ActionMethod::Get,
            ..action()
        };
        let (url, body) = action_payload(&action, "a&secret=é", "id-1").unwrap();
        assert!(url.ends_with("?text=a%26secret%3D%C3%A9&delivery_id=id-1"));
        assert!(body.is_empty());
        assert_eq!(
            action_payload(&action, &"a".repeat(9000), "id-1"),
            Err(AssistantError::SizeLimit)
        );
    }
    #[test]
    fn unknown_template_tokens_and_invalid_ids_are_rejected() {
        assert!(
            ActionConfig {
                payload_template: r#"{"text":"{{secret}}"}"#.into(),
                ..action()
            }
            .validate()
            .is_err()
        );
        assert!(
            ActionConfig {
                id: "../../file".into(),
                ..action()
            }
            .validate()
            .is_err()
        );
        assert!(
            ActionConfig {
                payload_template: "not json".into(),
                ..action()
            }
            .validate()
            .is_err()
        );
    }
    #[test]
    fn cancelled_action_does_not_attempt_network() {
        let token = CancellationToken::default();
        token.cancel();
        assert_eq!(
            execute_action(&action(), "text", "id-1", &token),
            Err(AssistantError::Cancelled)
        );
    }
    #[test]
    fn repeated_placeholders_cannot_allocate_unbounded_text() {
        let config = ActionConfig {
            payload_template: serde_json::json!({"text":"{{text}}".repeat(7000)}).to_string(),
            ..action()
        };
        assert!(config.validate().is_ok());
        assert_eq!(
            action_payload(&config, &"x".repeat(512 * 1024), "id-1"),
            Err(AssistantError::SizeLimit)
        );
    }
    #[test]
    #[cfg(windows)]
    fn unsafe_reserved_and_duplicate_headers_are_rejected() {
        let value = ProtectedSecret::protect("synthetic").unwrap();
        for name in [
            "Host",
            "Content-Length",
            "Transfer-Encoding",
            "Authorization",
            "Proxy-Authorization",
            "Idempotency-Key",
            "bad\r\nname",
        ] {
            let config = ActionConfig {
                headers: vec![ActionHeader {
                    name: name.into(),
                    value: value.clone(),
                }],
                ..action()
            };
            assert!(config.validate().is_err(), "{name}");
        }
        let config = ActionConfig {
            headers: vec![
                ActionHeader {
                    name: "X-Custom".into(),
                    value: value.clone(),
                },
                ActionHeader {
                    name: "x-custom".into(),
                    value,
                },
            ],
            ..action()
        };
        assert!(config.validate().is_err());
    }
}
