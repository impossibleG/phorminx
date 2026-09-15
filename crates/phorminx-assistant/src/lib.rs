//! User-triggered chat and HTTP delivery. No automatic tool use or cloud fallback.
mod action;
mod action_payload;
mod config;
#[cfg(test)]
mod network_tests;
mod provider;
mod secret;
mod transport;

pub use action::{
    ActionConfig, ActionHeader, ActionMethod, ActionPayloadMode, DeliveryReceipt, execute_action,
    execute_prepared_action,
};
pub use action_payload::{
    PreparedAction, prepare_action, prepare_generated_action, prepare_template_action,
};
pub use config::{AssistantConfig, ConfigStore};
pub use provider::{ChatMessage, ChatRole, ProviderConfig, ProviderKind, stream_chat};
pub use secret::{ExposedSecret, ProtectedSecret};

use std::net::{Shutdown, TcpStream};
use std::sync::{
    Arc, Mutex, Weak,
    atomic::{AtomicBool, Ordering},
};

/// Cancellation shuts down connected sockets and suppresses subsequent deltas.
/// DNS/connect are bounded by their timeouts. Hosts must also reject obsolete generation IDs.
#[derive(Clone, Debug, Default)]
pub struct CancellationToken(Arc<CancellationState>);
#[derive(Debug, Default)]
struct CancellationState {
    cancelled: AtomicBool,
    sockets: Mutex<Vec<Weak<TcpStream>>>,
}
impl CancellationToken {
    pub fn cancel(&self) {
        self.0.cancelled.store(true, Ordering::Release);
        let sockets = self.0.sockets.lock().unwrap_or_else(|e| e.into_inner());
        for socket in sockets.iter().filter_map(Weak::upgrade) {
            let _ = socket.shutdown(Shutdown::Both);
        }
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.cancelled.load(Ordering::Acquire)
    }
    pub(crate) fn register(&self, stream: &Arc<TcpStream>) {
        let mut sockets = self.0.sockets.lock().unwrap_or_else(|e| e.into_inner());
        sockets.retain(|s| s.strong_count() > 0);
        sockets.push(Arc::downgrade(stream));
        if self.is_cancelled() {
            let _ = stream.shutdown(Shutdown::Both);
        }
    }
    pub(crate) fn check(&self) -> Result<(), AssistantError> {
        if self.is_cancelled() {
            Err(AssistantError::Cancelled)
        } else {
            Ok(())
        }
    }
}

/// Errors deliberately exclude response bodies, credentials, prompts and URLs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AssistantError {
    #[error("Choose a supported endpoint, explicit model and valid configuration.")]
    InvalidConfig,
    #[error("The configured credential could not be protected or opened on this account.")]
    Credential,
    #[error("A credential is required for this provider.")]
    MissingCredential,
    #[error("The request was cancelled.")]
    Cancelled,
    #[error("The service could not be reached or the request timed out.")]
    Transport,
    #[error("The service rejected the request (HTTP {0}).")]
    HttpStatus(u16),
    #[error("The service returned an invalid stream.")]
    Protocol,
    #[error(
        "The model did not return valid JSON. Nothing was sent; adjust the payload prompt and try again."
    )]
    InvalidPayload,
    #[error("The answer ended before completion. The partial answer is not complete.")]
    Incomplete,
    #[error(
        "The request or response exceeds the supported size. Start a new conversation or send less text."
    )]
    SizeLimit,
    #[error("Configuration could not be read or saved.")]
    Storage,
    #[error(
        "Delivery could not be confirmed. Check the receiving service before retrying to avoid duplicates."
    )]
    DeliveryUncertain,
}

pub(crate) fn agent(token: &CancellationToken) -> ureq::Agent {
    use std::time::Duration;
    let config = ureq::Agent::config_builder()
        .proxy(None)
        .max_redirects(0)
        .max_redirects_will_error(false)
        .http_status_as_error(false)
        .max_idle_connections(0)
        .timeout_resolve(Some(Duration::from_secs(10)))
        .timeout_connect(Some(Duration::from_secs(10)))
        .timeout_send_request(Some(Duration::from_secs(15)))
        .timeout_send_body(Some(Duration::from_secs(15)))
        .timeout_recv_response(Some(Duration::from_secs(120)))
        .timeout_recv_body(Some(Duration::from_secs(300)))
        .timeout_global(Some(Duration::from_secs(420)))
        .build();
    transport::cancellable_agent(config, token.clone())
}

pub(crate) fn endpoint_uri(
    endpoint: &str,
    local_only: bool,
) -> Result<ureq::http::Uri, AssistantError> {
    if endpoint.len() > 4096
        || endpoint
            .chars()
            .any(|c| c.is_control() || c.is_whitespace())
        || endpoint.contains(['#', '@', '\\'])
    {
        return Err(AssistantError::InvalidConfig);
    }
    let uri: ureq::http::Uri = endpoint
        .parse()
        .map_err(|_| AssistantError::InvalidConfig)?;
    let host = uri.host().ok_or(AssistantError::InvalidConfig)?;
    let ip = host
        .trim_start_matches('[')
        .trim_end_matches(']')
        .parse::<std::net::IpAddr>();
    let loopback = ip.is_ok_and(|ip| ip.is_loopback());
    if !(uri.scheme_str() == Some("https") || uri.scheme_str() == Some("http") && loopback)
        || local_only && !loopback
        || uri.port_u16() == Some(0)
    {
        return Err(AssistantError::InvalidConfig);
    }
    Ok(uri)
}

pub(crate) fn secret_header(value: &str) -> Result<ureq::http::HeaderValue, AssistantError> {
    let mut header =
        ureq::http::HeaderValue::from_str(value).map_err(|_| AssistantError::Credential)?;
    header.set_sensitive(true);
    Ok(header)
}
