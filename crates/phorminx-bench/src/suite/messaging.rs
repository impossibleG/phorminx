//! Messaging: two throwaway messengers on this machine add each other with an
//! invite and exchange messages. The loopback run uses the in-memory
//! transport and measures the engine (handshake, encryption, storage); the Tor
//! run uses the real Tor network, cold (empty Tor state) and warm (restarted
//! with its saved state).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use phorminx_core::benchmark_report::Measure;
use phorminx_messaging::{
    Config, KeyVault, MemoryNetwork, MemoryTransport, MessageState, MessagingError, Messenger,
    NetworkState, SecretBytes, TorTransport, Transport,
};
use serde_json::json;

use super::{FeatureResult, run};

pub const FEATURE: &str = "messaging";

/// Throwaway profiles need no Windows key protection.
struct PlainVault;
impl KeyVault for PlainVault {
    fn protect(&self, secret: &[u8]) -> Result<Vec<u8>, MessagingError> {
        Ok(secret.to_vec())
    }
    fn expose(&self, protected: &[u8]) -> Result<SecretBytes, MessagingError> {
        Ok(SecretBytes::new(protected.to_vec()))
    }
}

fn start(transport: impl Transport, directory: &Path) -> Result<Messenger> {
    std::fs::create_dir_all(directory)?;
    Messenger::start(
        transport,
        &directory.join("phorminx.db"),
        Arc::new(PlainVault),
        Config::default(),
        |_| {},
    )
    .map_err(|error| anyhow::anyhow!("messaging failed to start: {error}"))
}

fn wait(what: &str, limit: Duration, mut done: impl FnMut() -> bool) -> Result<f64> {
    let started = Instant::now();
    while !done() {
        if started.elapsed() > limit {
            bail!("timed out waiting for {what}");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    Ok(started.elapsed().as_secs_f64())
}

fn online(messenger: &Messenger) -> bool {
    messenger
        .status()
        .is_ok_and(|status| status.network == NetworkState::Online)
}

fn delivered(messenger: &Messenger, contact: i64, id: i64) -> bool {
    messenger.conversation(contact, 200).is_ok_and(|messages| {
        messages
            .iter()
            .any(|m| m.id == id && m.state == MessageState::Delivered)
    })
}

struct Timings {
    bootstrap: Vec<f64>,
    redeem: f64,
    first: f64,
    rtt: Vec<f64>,
}

fn exchange(ana: &Messenger, bob: &Messenger, messages: u32, limit: Duration) -> Result<Timings> {
    let started = Instant::now();
    ana.enable(Some("Ana"))
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    bob.enable(Some("Bob"))
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let mut bootstrap = Vec::new();
    wait("Ana online", limit, || online(ana))?;
    bootstrap.push(started.elapsed().as_secs_f64());
    wait("Bob online", limit, || online(bob))?;
    bootstrap.push(started.elapsed().as_secs_f64());

    let invite = ana.create_invite().map_err(|e| anyhow::anyhow!("{e}"))?;
    let started = Instant::now();
    let mut attempt = 0;
    let ana_for_bob = loop {
        attempt += 1;
        match bob.redeem_invite(&invite) {
            Ok(contact) => break contact,
            Err(MessagingError::InviteUnreachable) if attempt < 6 => continue,
            Err(error) => bail!("redeem failed: {error}"),
        }
    };
    wait("Ana sees Bob", limit, || {
        ana.contacts().is_ok_and(|contacts| contacts.len() == 1)
    })?;
    let redeem = started.elapsed().as_secs_f64();

    let sent = bob
        .send(ana_for_bob.id, "hello")
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let first = wait("first message", limit, || {
        delivered(bob, ana_for_bob.id, sent.id)
    })?;
    let mut rtt = Vec::new();
    for index in 0..messages {
        let sent = bob
            .send(ana_for_bob.id, &format!("ping {index}"))
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        rtt.push(wait("message", limit, || {
            delivered(bob, ana_for_bob.id, sent.id)
        })?);
    }
    Ok(Timings {
        bootstrap,
        redeem,
        first,
        rtt,
    })
}

fn push(result: &mut FeatureResult, backend: &str, start: &str, timings: &Timings) {
    let built = run(
        FEATURE,
        None,
        backend,
        &[("transport", json!(backend)), ("start", json!(start))],
        timings.rtt.len(),
        vec![
            ("bootstrap", Measure::of(&timings.bootstrap, "s")),
            ("redeem", Measure::new(timings.redeem, "s")),
            ("firstMessage", Measure::new(timings.first, "s")),
            ("rtt", Measure::of(&timings.rtt, "s")),
        ],
    );
    println!(
        "{backend} {start}: bootstrap {:.2}s redeem {:.2}s first {:.2}s rtt {:.3}s",
        built.metrics["bootstrap"].value,
        built.metrics["redeem"].value,
        built.metrics["firstMessage"].value,
        built.metrics["rtt"].value
    );
    result.runs.push(built);
}

fn workspace(name: &str) -> PathBuf {
    let directory = std::env::temp_dir().join(format!(
        "phorminx-messaging-bench-{}-{name}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&directory);
    directory
}

pub fn measure(tor: bool, messages: u32) -> Result<FeatureResult> {
    let mut result = FeatureResult {
        note: "Two throwaway profiles on one machine. Loopback measures the messaging engine without a network; Tor uses the public Tor network, so its times vary with the circuits it builds.".into(),
        ..Default::default()
    };
    let limit = Duration::from_secs(600);

    let root = workspace("loopback");
    let network = MemoryNetwork::new();
    let ana = start(
        MemoryTransport::new(network.clone(), [1; 16]),
        &root.join("ana"),
    )?;
    let bob = start(
        MemoryTransport::new(network.clone(), [2; 16]),
        &root.join("bob"),
    )?;
    let timings = exchange(&ana, &bob, messages, limit)?;
    push(&mut result, "loopback", "warm", &timings);
    drop((ana, bob));
    let _ = std::fs::remove_dir_all(&root);

    if !tor {
        result.skip(
            FEATURE,
            "Tor bootstrap, invite and messages",
            "not run: pass --tor to use the public Tor network",
        );
        return Ok(result);
    }
    let root = workspace("tor");
    let ana_dir = root.join("ana");
    let bob_dir = root.join("bob");
    let ana = start(TorTransport::new(ana_dir.join("tor")), &ana_dir)?;
    let bob = start(TorTransport::new(bob_dir.join("tor")), &bob_dir)?;
    let cold = exchange(&ana, &bob, messages, limit).context("the Tor run failed")?;
    push(&mut result, "tor", "cold", &cold);

    // Warm: both restart with their saved Tor state and contact.
    drop((ana, bob));
    std::thread::sleep(Duration::from_secs(3));
    let started = Instant::now();
    let ana = start(TorTransport::new(ana_dir.join("tor")), &ana_dir)?;
    let bob = start(TorTransport::new(bob_dir.join("tor")), &bob_dir)?;
    let mut bootstrap = Vec::new();
    wait("Ana online (warm)", limit, || online(&ana))?;
    bootstrap.push(started.elapsed().as_secs_f64());
    wait("Bob online (warm)", limit, || online(&bob))?;
    bootstrap.push(started.elapsed().as_secs_f64());
    let contact = bob
        .contacts()
        .map_err(|e| anyhow::anyhow!("{e}"))?
        .first()
        .context("Bob lost the contact")?
        .id;
    let sent = bob
        .send(contact, "back")
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let first = wait("first message (warm)", limit, || {
        delivered(&bob, contact, sent.id)
    })?;
    let mut rtt = Vec::new();
    for index in 0..messages {
        let sent = bob
            .send(contact, &format!("again {index}"))
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        rtt.push(wait("message (warm)", limit, || {
            delivered(&bob, contact, sent.id)
        })?);
    }
    let built = run(
        FEATURE,
        None,
        "tor",
        &[("transport", json!("tor")), ("start", json!("warm"))],
        rtt.len(),
        vec![
            ("bootstrap", Measure::of(&bootstrap, "s")),
            ("firstMessage", Measure::new(first, "s")),
            ("rtt", Measure::of(&rtt, "s")),
        ],
    );
    println!(
        "tor warm: bootstrap {:.2}s first {:.2}s rtt {:.3}s",
        built.metrics["bootstrap"].value,
        built.metrics["firstMessage"].value,
        built.metrics["rtt"].value
    );
    result.runs.push(built);
    drop((ana, bob));
    std::thread::sleep(Duration::from_secs(2));
    let _ = std::fs::remove_dir_all(&root);
    Ok(result)
}
