//! Bounded daemon recovery. Model trust remains a separate runtime check.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use phorminx_ollama::{
    CancellationToken, ClientError, ClientTimeouts, OllamaClient, OllamaEndpoint,
};

static LAST_ATTEMPT: Mutex<Option<Instant>> = Mutex::new(None);

/// Called only on background setup/readiness or worker startup threads. No
/// downloads, model substitutions, or external endpoints are involved.
pub fn recover_local_ollama(cancel: &AtomicBool) -> bool {
    if cancel.load(Ordering::Acquire) {
        return false;
    }
    let client = OllamaClient::new(
        OllamaEndpoint::default(),
        ClientTimeouts {
            connect: Duration::from_millis(300),
            response_headers: Duration::from_millis(500),
            response_body: Duration::from_millis(500),
            overall: Duration::from_secs(1),
        },
    )
    .expect("fixed recovery timeouts are valid");
    match client.discover(&CancellationToken::new()) {
        Ok(_) => return true,
        Err(error) if should_start_after(&error) => {}
        Err(_) => return false,
    }
    if cancel.load(Ordering::Acquire) {
        return false;
    }
    let Ok(mut attempt) = LAST_ATTEMPT.try_lock() else {
        return false;
    };
    if attempt.is_some_and(|last| last.elapsed() < Duration::from_secs(30)) {
        return false;
    }
    *attempt = Some(Instant::now());
    // Keep the lock until the probe finishes: concurrent UI/worker checks must
    // not create a herd of daemon processes.
    if let Err(error) = phorminx_windows::start_verified_ollama(cancel) {
        // These variants contain no file paths, credentials, or transcript.
        eprintln!("event=ollama_start_unavailable reason={error}");
        return false;
    }
    let deadline = Instant::now() + Duration::from_secs(8);
    while !cancel.load(Ordering::Acquire) && Instant::now() < deadline {
        if client.discover(&CancellationToken::new()).is_ok() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    false
}

fn should_start_after(error: &ClientError) -> bool {
    // A timeout, invalid JSON or HTTP failure could be a running but unhealthy
    // service. Do not respond by launching another daemon repeatedly.
    matches!(error, ClientError::Transport(_))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_transport_failure_requests_a_start() {
        assert!(should_start_after(&ClientError::Transport(
            "refused".into()
        )));
        assert!(!should_start_after(&ClientError::Timeout));
        assert!(!should_start_after(&ClientError::HttpStatus(500)));
        assert!(!should_start_after(&ClientError::Cancelled));
        assert!(!should_start_after(&ClientError::Api(
            "model missing".into()
        )));
    }
}
