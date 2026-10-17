//! Speaks the wire protocol by hand against a real engine to check the
//! handshake rules, dedupe and protocol violations.
use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use phorminx_messaging::{
    BoxStream, Config, Frame, Identity, Invite, KeyVault, MemoryNetwork, MemoryTransport,
    MessagingError, Messenger, NetworkState, OnionAddress, ProtocolError, SecretBytes, Transport,
    read_frame, write_frame,
};

struct XorVault;
impl KeyVault for XorVault {
    fn protect(&self, secret: &[u8]) -> Result<Vec<u8>, MessagingError> {
        Ok(secret.iter().map(|b| b ^ 0x3c).collect())
    }
    fn expose(&self, protected: &[u8]) -> Result<SecretBytes, MessagingError> {
        Ok(SecretBytes::new(
            protected.iter().map(|b| b ^ 0x3c).collect(),
        ))
    }
}

const SEED: [u8; 16] = [1; 16];
const ID: &str = "11111111-2222-4333-8444-555555555555";

fn b64(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

fn unb64(text: &str) -> Vec<u8> {
    URL_SAFE_NO_PAD.decode(text).unwrap()
}

struct Host {
    _dir: tempfile::TempDir,
    messenger: Messenger,
    network: Arc<MemoryNetwork>,
}

/// Ana, running a real engine on an open (non-restricted) transport so raw
/// callers can reach her services.
fn host() -> Host {
    let network = MemoryNetwork::new();
    let transport = MemoryTransport::new(network.clone(), SEED).without_restricted_discovery();
    let dir = tempfile::tempdir().unwrap();
    let config = Config {
        tick: Duration::from_millis(20),
        handshake_timeout: Duration::from_secs(3),
        connect_timeout: Duration::from_secs(3),
        ..Config::default()
    };
    let messenger = Messenger::start(
        transport,
        &dir.path().join("p.db"),
        Arc::new(XorVault),
        config,
        |_| {},
    )
    .unwrap();
    messenger.enable(Some("Ana")).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while messenger.status().unwrap().network != NetworkState::Online {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    }
    Host {
        _dir: dir,
        messenger,
        network,
    }
}

fn ana_contacts_address() -> OnionAddress {
    MemoryTransport::new(MemoryNetwork::new(), SEED)
        .service_address("contacts")
        .unwrap()
}

/// A hand-driven peer with its own identity and transport.
struct Raw {
    identity: Identity,
    transport: MemoryTransport,
}

impl Raw {
    async fn new(network: &Arc<MemoryNetwork>, seed: u8) -> Self {
        let transport =
            MemoryTransport::new(network.clone(), [seed; 16]).without_restricted_discovery();
        transport.start().await.unwrap();
        Self {
            identity: Identity::generate().unwrap().0,
            transport,
        }
    }

    async fn connect(&self, address: &OnionAddress) -> BoxStream {
        self.transport.connect(address).await.unwrap()
    }

    fn hello(&self, name: &str) -> Frame {
        Frame::hello(&self.identity.public_key().0, name)
    }

    /// Runs the caller side of the handshake; returns the challenge nonce.
    async fn handshake(&self, stream: &mut BoxStream, prove_for: &OnionAddress) -> Vec<u8> {
        write_frame(stream, &self.hello("Raw")).await.unwrap();
        assert!(
            matches!(read_frame(stream).await.unwrap(), Frame::Hello { display_name, .. } if display_name == "Ana")
        );
        let nonce = match read_frame(stream).await.unwrap() {
            Frame::Challenge { nonce } => unb64(&nonce),
            other => panic!("expected challenge, got {other:?}"),
        };
        let proof = self.identity.prove(&nonce, &prove_for.as_onion());
        write_frame(
            stream,
            &Frame::Proof {
                signature: b64(&proof),
            },
        )
        .await
        .unwrap();
        nonce
    }

    /// Redeems an invite by hand and returns Ana's contacts address.
    async fn redeem(&self, invite: &str, secret: Option<[u8; 16]>) -> Frame {
        let invite = Invite::decode(invite).unwrap();
        let mut stream = self.connect(&invite.address).await;
        self.handshake(&mut stream, &invite.address).await;
        write_frame(
            &mut stream,
            &Frame::RedeemInvite {
                secret: b64(&secret.unwrap_or(invite.secret)),
                contacts_address: self
                    .transport
                    .service_address("contacts")
                    .unwrap()
                    .to_string(),
                discovery_key: None,
            },
        )
        .await
        .unwrap();
        read_frame(&mut stream).await.unwrap()
    }
}

fn closed(result: Result<Frame, ProtocolError>) -> bool {
    matches!(result, Err(ProtocolError::Closed) | Ok(Frame::Goodbye {}))
}

#[tokio::test(flavor = "multi_thread")]
async fn full_protocol_with_duplicate_delivery() {
    let host = host();
    let invite = host.messenger.create_invite().unwrap();
    let raw = Raw::new(&host.network, 50).await;
    let accepted = raw.redeem(&invite, None).await;
    let contacts_address = match accepted {
        Frame::InviteAccepted {
            contacts_address,
            discovery_key,
        } => {
            assert!(
                discovery_key.is_none(),
                "open transport has no discovery keys"
            );
            OnionAddress::parse(&contacts_address).unwrap()
        }
        other => panic!("expected acceptance, got {other:?}"),
    };
    assert_eq!(contacts_address, ana_contacts_address());
    assert_eq!(host.messenger.contacts().unwrap()[0].name, "Raw");

    let mut stream = raw.connect(&contacts_address).await;
    raw.handshake(&mut stream, &contacts_address).await;
    let message = Frame::Message {
        id: ID.into(),
        sent_at: 1_700_000_000_000,
        text: "hello".into(),
    };
    for _ in 0..2 {
        write_frame(&mut stream, &message).await.unwrap();
        assert_eq!(
            read_frame(&mut stream).await.unwrap(),
            Frame::Ack { id: ID.into() }
        );
    }
    let contact = host.messenger.contacts().unwrap()[0].clone();
    assert_eq!(contact.unread, 1);
    let conversation = host.messenger.conversation(contact.id, 10).unwrap();
    assert_eq!(conversation.len(), 1);
    assert_eq!(conversation[0].sent_at_ms, 1_700_000_000_000);

    // Ana's reply arrives over the same session.
    host.messenger.send(contact.id, "hi Raw").unwrap();
    match read_frame(&mut stream).await.unwrap() {
        Frame::Message { id, text, .. } => {
            assert_eq!(text, "hi Raw");
            write_frame(&mut stream, &Frame::Ack { id }).await.unwrap();
        }
        other => panic!("expected message, got {other:?}"),
    }

    // A frame that does not belong in a session ends it.
    write_frame(
        &mut stream,
        &Frame::Challenge {
            nonce: b64(&[0; 32]),
        },
    )
    .await
    .unwrap();
    assert!(closed(read_frame(&mut stream).await));
}

#[tokio::test(flavor = "multi_thread")]
async fn unknown_callers_learn_nothing() {
    let host = host();
    let invite = host.messenger.create_invite().unwrap();
    let friend = Raw::new(&host.network, 50).await;
    assert!(matches!(
        friend.redeem(&invite, None).await,
        Frame::InviteAccepted { .. }
    ));

    let stranger = Raw::new(&host.network, 60).await;
    let mut stream = stranger.connect(&ana_contacts_address()).await;
    write_frame(&mut stream, &stranger.hello("Stranger"))
        .await
        .unwrap();
    // No hello, no challenge: the connection just closes.
    assert!(matches!(
        read_frame(&mut stream).await,
        Err(ProtocolError::Closed)
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn proof_for_another_address_is_rejected() {
    let host = host();
    let invite = host.messenger.create_invite().unwrap();
    let raw = Raw::new(&host.network, 50).await;
    assert!(matches!(
        raw.redeem(&invite, None).await,
        Frame::InviteAccepted { .. }
    ));
    let address = ana_contacts_address();
    let mut stream = raw.connect(&address).await;
    // Sign for a different service, as a relaying attacker would have to.
    let elsewhere = raw.transport.service_address("elsewhere").unwrap();
    raw.handshake(&mut stream, &elsewhere).await;
    let _ = write_frame(
        &mut stream,
        &Frame::Message {
            id: ID.into(),
            sent_at: 1,
            text: "sneaky".into(),
        },
    )
    .await;
    assert!(closed(read_frame(&mut stream).await));
    let contact = host.messenger.contacts().unwrap()[0].clone();
    assert!(
        host.messenger
            .conversation(contact.id, 10)
            .unwrap()
            .is_empty()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn other_protocol_versions_are_refused() {
    let host = host();
    let invite = Invite::decode(&host.messenger.create_invite().unwrap()).unwrap();
    let raw = Raw::new(&host.network, 50).await;
    let mut stream = raw.connect(&invite.address).await;
    write_frame(
        &mut stream,
        &Frame::Hello {
            version: 2,
            identity_key: b64(&raw.identity.public_key().0),
            display_name: "Future".into(),
        },
    )
    .await
    .unwrap();
    assert!(closed(read_frame(&mut stream).await));
}

#[tokio::test(flavor = "multi_thread")]
async fn wrong_invite_secret_is_rejected_and_invite_survives() {
    let host = host();
    let invite = host.messenger.create_invite().unwrap();
    let guesser = Raw::new(&host.network, 50).await;
    assert_eq!(
        guesser.redeem(&invite, Some([0; 16])).await,
        Frame::InviteRejected {}
    );
    assert!(host.messenger.contacts().unwrap().is_empty());
    let friend = Raw::new(&host.network, 51).await;
    assert!(matches!(
        friend.redeem(&invite, None).await,
        Frame::InviteAccepted { .. }
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn garbage_before_hello_is_dropped() {
    let host = host();
    let invite = Invite::decode(&host.messenger.create_invite().unwrap()).unwrap();
    let raw = Raw::new(&host.network, 50).await;
    let mut stream = raw.connect(&invite.address).await;
    use tokio::io::AsyncWriteExt;
    stream
        .write_all(&[0xff, 0xff, 0xff, 0xff, 1, 2, 3])
        .await
        .unwrap();
    assert!(closed(read_frame(&mut stream).await));
    // The invite is still usable by the real friend.
    let friend = Raw::new(&host.network, 51).await;
    assert!(matches!(
        friend.redeem(&invite_text(&invite), None).await,
        Frame::InviteAccepted { .. }
    ));
}

fn invite_text(invite: &Invite) -> String {
    invite.encode()
}
