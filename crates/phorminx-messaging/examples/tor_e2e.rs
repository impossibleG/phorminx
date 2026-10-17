//! Real-network end-to-end check: two messengers with separate data folders
//! add each other with an invite and exchange messages over Tor.
//!
//! cargo run --release -p phorminx-messaging --features tor --example tor_e2e -- <work-dir>
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use phorminx_messaging::{
    Config, KeyVault, MessageState, MessagingError, Messenger, NetworkState, OnionAddress,
    SecretBytes, TorTransport, Transport,
};

struct XorVault;
impl KeyVault for XorVault {
    fn protect(&self, secret: &[u8]) -> Result<Vec<u8>, MessagingError> {
        Ok(secret.iter().map(|b| b ^ 0x77).collect())
    }
    fn expose(&self, protected: &[u8]) -> Result<SecretBytes, MessagingError> {
        Ok(SecretBytes::new(
            protected.iter().map(|b| b ^ 0x77).collect(),
        ))
    }
}

fn start(root: &Path, name: &str, t0: Instant) -> Messenger {
    let dir = root.join(name);
    std::fs::create_dir_all(&dir).unwrap();
    let label = name.to_string();
    Messenger::start(
        TorTransport::new(dir.join("tor")),
        &dir.join("phorminx.db"),
        Arc::new(XorVault),
        Config::default(),
        move |event| println!("[{:>7.1}s] {label}: {event:?}", t0.elapsed().as_secs_f64()),
    )
    .unwrap()
}

fn wait_for(what: &str, limit: Duration, mut done: impl FnMut() -> bool) -> Duration {
    let start = Instant::now();
    while !done() {
        assert!(start.elapsed() < limit, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(100));
    }
    let took = start.elapsed();
    println!("== {what}: {:.1}s", took.as_secs_f64());
    took
}

fn state(m: &Messenger, contact: i64, index: usize) -> Option<MessageState> {
    m.conversation(contact, 100)
        .ok()?
        .get(index)
        .map(|message| message.state)
}

fn contacts_address(db: &Path) -> OnionAddress {
    let connection = rusqlite::Connection::open(db).unwrap();
    let text: String = connection
        .query_row("SELECT contacts_address FROM messaging_identity", [], |r| {
            r.get(0)
        })
        .unwrap();
    OnionAddress::parse(&text).unwrap()
}

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_ansi(false)
        .init();
    let root = PathBuf::from(std::env::args().nth(1).unwrap_or_else(|| "tor-e2e".into()));
    let t0 = Instant::now();
    let ana = start(&root, "ana", t0);
    let mut bob = Some(start(&root, "bob", t0));
    macro_rules! b {
        () => {
            bob.as_ref().unwrap()
        };
    }

    ana.enable(Some("Ana")).unwrap();
    b!().enable(Some("Bob")).unwrap();
    let limit = Duration::from_secs(300);
    wait_for("Ana online", limit, || {
        ana.status().unwrap().network == NetworkState::Online
    });
    wait_for("Bob online", limit, || {
        b!().status().unwrap().network == NetworkState::Online
    });

    let invite = ana.create_invite().unwrap();
    println!(
        "== invite length {} chars, starts {:?}",
        invite.len(),
        &invite[..4]
    );
    // The invite service needs a few seconds to publish its descriptor.
    let redeem_start = Instant::now();
    let mut attempt = 0;
    let ana_for_bob = loop {
        attempt += 1;
        match b!().redeem_invite(&invite) {
            Ok(contact) => break contact,
            Err(MessagingError::InviteUnreachable) if attempt < 6 => {
                println!("   redeem attempt {attempt} could not reach the invite yet, retrying");
            }
            Err(e) => panic!("redeem failed: {e}"),
        }
    };
    println!(
        "== invite redeemed in {:.1}s ({attempt} attempt(s)); Bob sees {:?}",
        redeem_start.elapsed().as_secs_f64(),
        ana_for_bob.name
    );
    wait_for("Ana sees Bob", limit, || ana.contacts().unwrap().len() == 1);
    let bob_for_ana = ana.contacts().unwrap()[0].clone();
    assert_eq!(bob_for_ana.name, "Bob");

    b!().send(ana_for_bob.id, "hello Ana, over Tor").unwrap();
    wait_for("first message delivered (cold dial)", limit, || {
        state(b!(), ana_for_bob.id, 0) == Some(MessageState::Delivered)
    });
    ana.send(bob_for_ana.id, "hi Bob!").unwrap();
    wait_for("reply delivered (session reuse)", limit, || {
        state(&ana, bob_for_ana.id, 1) == Some(MessageState::Delivered)
    });
    let mut latencies = Vec::new();
    for i in 0..5 {
        b!().send(ana_for_bob.id, &format!("ping {i}")).unwrap();
        latencies.push(wait_for("warm message", limit, || {
            state(b!(), ana_for_bob.id, 2 + i) == Some(MessageState::Delivered)
        }));
    }
    let mean = latencies.iter().sum::<Duration>() / latencies.len() as u32;
    println!("== warm send-to-delivered mean {:.2}s", mean.as_secs_f64());

    // A stranger with no discovery key cannot even find Ana's contacts service.
    let stranger_dir = root.join("stranger");
    let ana_address = contacts_address(&root.join("ana").join("phorminx.db"));
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        let stranger = TorTransport::new(stranger_dir.join("tor"));
        stranger.start().await.unwrap();
        let attempt =
            tokio::time::timeout(Duration::from_secs(120), stranger.connect(&ana_address)).await;
        match attempt {
            Ok(Ok(_)) => panic!("restricted discovery let a stranger connect"),
            Ok(Err(e)) => println!("== stranger refused as expected: {e}"),
            Err(_) => println!("== stranger timed out as expected"),
        }
        stranger.stop();
    });

    // Bob goes away; Ana's message waits, then arrives when Bob is back.
    drop(bob.take());
    let away = ana.send(bob_for_ana.id, "while you were away").unwrap();
    wait_for("message marked waiting", limit, || {
        ana.conversation(bob_for_ana.id, 100)
            .unwrap()
            .iter()
            .any(|m| m.id == away.id && m.state == MessageState::Waiting)
    });
    let restart = Instant::now();
    bob = Some(start(&root, "bob", t0));
    wait_for("Bob back online (warm)", limit, || {
        b!().status().unwrap().network == NetworkState::Online
    });
    wait_for(
        "queued message delivered after Bob returns",
        Duration::from_secs(900),
        || {
            ana.conversation(bob_for_ana.id, 100)
                .unwrap()
                .iter()
                .any(|m| m.id == away.id && m.state == MessageState::Delivered)
        },
    );
    println!(
        "== restart-to-delivery {:.1}s",
        restart.elapsed().as_secs_f64()
    );

    // Turning messaging off leaves Tor; turning it on again reconnects.
    ana.disable().unwrap();
    assert_eq!(ana.status().unwrap().network, NetworkState::Disabled);
    let reenable = Instant::now();
    ana.enable(None).unwrap();
    wait_for("Ana online again after off/on", limit, || {
        ana.status().unwrap().network == NetworkState::Online
    });
    let after = b!().send(ana_for_bob.id, "still there?").unwrap();
    wait_for("message delivered after off/on", limit, || {
        b!().conversation(ana_for_bob.id, 100)
            .unwrap()
            .iter()
            .any(|m| m.id == after.id && m.state == MessageState::Delivered)
    });
    println!(
        "== off/on to delivery {:.1}s",
        reenable.elapsed().as_secs_f64()
    );
    println!("ALL OK in {:.1}s", t0.elapsed().as_secs_f64());
}
