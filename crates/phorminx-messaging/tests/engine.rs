//! End-to-end engine tests over the in-memory transport: invites, delivery,
//! offline outbox, restarts, blocking and limits.
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use phorminx_messaging::{
    Config, ContactView, Event, KeyVault, MemoryNetwork, MemoryTransport, MessageState,
    MessagingError, Messenger, NetworkState, SecretBytes,
};

/// A reversible stand-in for DPAPI that still never stores the raw seed.
struct XorVault;
impl KeyVault for XorVault {
    fn protect(&self, secret: &[u8]) -> Result<Vec<u8>, MessagingError> {
        Ok(secret.iter().map(|b| b ^ 0xa5).collect())
    }
    fn expose(&self, protected: &[u8]) -> Result<SecretBytes, MessagingError> {
        Ok(SecretBytes::new(
            protected.iter().map(|b| b ^ 0xa5).collect(),
        ))
    }
}

fn fast() -> Config {
    Config {
        invite_ttl: Duration::from_secs(60),
        retry_delays: vec![Duration::from_millis(60), Duration::from_millis(120)],
        tick: Duration::from_millis(20),
        handshake_timeout: Duration::from_secs(3),
        connect_timeout: Duration::from_secs(3),
        idle_timeout: Duration::from_secs(30),
        restart_delay: Duration::from_millis(100),
        rate_burst: 30,
        rate_refill: Duration::from_millis(10),
        announce_limit: 20,
    }
}

struct Peer {
    messenger: Option<Messenger>,
    transport: Arc<MemoryTransport>,
    events: Arc<Mutex<Vec<Event>>>,
    dir: tempfile::TempDir,
    config: Config,
}

impl Peer {
    fn new(network: &Arc<MemoryNetwork>, seed: u8, config: Config) -> Self {
        let transport = Arc::new(MemoryTransport::new(network.clone(), [seed; 16]));
        let dir = tempfile::tempdir().unwrap();
        let mut peer = Self {
            messenger: None,
            transport,
            events: Arc::default(),
            dir,
            config,
        };
        peer.start();
        peer
    }

    fn start(&mut self) {
        let events = self.events.clone();
        self.messenger = Some(
            Messenger::start(
                self.transport.clone(),
                &self.dir.path().join("phorminx.db"),
                Arc::new(XorVault),
                self.config.clone(),
                move |e| events.lock().unwrap().push(e),
            )
            .unwrap(),
        );
    }

    fn stop(&mut self) {
        self.messenger = None;
    }

    fn m(&self) -> &Messenger {
        self.messenger.as_ref().unwrap()
    }

    fn enable(&self, name: &str) {
        self.m().enable(Some(name)).unwrap();
        wait(|| self.m().status().unwrap().network == NetworkState::Online);
    }

    fn contact(&self, name: &str) -> ContactView {
        self.m()
            .contacts()
            .unwrap()
            .into_iter()
            .find(|c| c.name == name)
            .unwrap_or_else(|| panic!("no contact {name}"))
    }

    fn texts(&self, contact_id: i64) -> Vec<(bool, String, MessageState)> {
        self.m()
            .conversation(contact_id, 100)
            .unwrap()
            .into_iter()
            .map(|m| (m.outgoing, m.text, m.state))
            .collect()
    }

    fn received_events(&self) -> usize {
        self.events
            .lock()
            .unwrap()
            .iter()
            .filter(|e| matches!(e, Event::MessageReceived { .. }))
            .count()
    }
}

fn wait(mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !condition() {
        assert!(Instant::now() < deadline, "condition not met in time");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Two enabled peers who are contacts of each other.
fn pair(config: Config) -> (Arc<MemoryNetwork>, Peer, Peer) {
    let network = MemoryNetwork::new();
    let ana = Peer::new(&network, 1, config.clone());
    let bob = Peer::new(&network, 2, config);
    ana.enable("Ana");
    bob.enable("Bob");
    let invite = ana.m().create_invite().unwrap();
    assert!(invite.starts_with("Ana#"));
    let added = bob.m().redeem_invite(&invite).unwrap();
    assert_eq!(added.name, "Ana");
    wait(|| ana.m().contacts().unwrap().len() == 1);
    (network, ana, bob)
}

#[test]
fn nothing_runs_until_enabled() {
    let network = MemoryNetwork::new();
    let peer = Peer::new(&network, 9, fast());
    let status = peer.m().status().unwrap();
    assert!(!status.enabled && !status.has_identity);
    assert_eq!(status.network, NetworkState::Disabled);
    assert!(matches!(
        peer.m().create_invite(),
        Err(MessagingError::NotOnline)
    ));
    assert!(matches!(
        peer.m().enable(None),
        Err(MessagingError::InvalidName)
    ));
    assert!(matches!(
        peer.m().enable(Some(" \n")),
        Err(MessagingError::InvalidName)
    ));
    peer.enable("Ana");
    let status = peer.m().status().unwrap();
    assert!(status.enabled && status.has_identity);
    assert_eq!(status.display_name.as_deref(), Some("Ana"));
}

#[test]
fn invite_exchange_then_two_way_messages() {
    let (_net, ana, bob) = pair(fast());
    let ana_view_of_bob = ana.contact("Bob");
    let bob_view_of_ana = bob.contact("Ana");

    let sent = bob.m().send(bob_view_of_ana.id, "hi Ana").unwrap();
    assert!(sent.outgoing);
    wait(|| bob.texts(bob_view_of_ana.id)[0].2 == MessageState::Delivered);
    wait(|| ana.texts(ana_view_of_bob.id).len() == 1);
    assert_eq!(
        ana.texts(ana_view_of_bob.id),
        vec![(false, "hi Ana".to_string(), MessageState::Received)]
    );
    assert_eq!(ana.contact("Bob").unread, 1);
    ana.m().mark_read(ana_view_of_bob.id).unwrap();
    assert_eq!(ana.contact("Bob").unread, 0);

    ana.m().send(ana_view_of_bob.id, "hey Bob").unwrap();
    wait(|| bob.texts(bob_view_of_ana.id).len() == 2);
    wait(|| ana.texts(ana_view_of_bob.id)[1].2 == MessageState::Delivered);
    assert_eq!(bob.received_events(), 1);
    assert!(ana.events.lock().unwrap().iter().any(|e| matches!(
        e,
        Event::MessageReceived { contact_name, .. } if contact_name == "Bob"
    )));
}

#[test]
fn invites_are_single_use_and_destroyed() {
    let network = MemoryNetwork::new();
    let ana = Peer::new(&network, 1, fast());
    let bob = Peer::new(&network, 2, fast());
    let eve = Peer::new(&network, 3, fast());
    ana.enable("Ana");
    bob.enable("Bob");
    eve.enable("Eve");
    let invite = ana.m().create_invite().unwrap();
    bob.m().redeem_invite(&invite).unwrap();
    assert!(eve.m().redeem_invite(&invite).is_err());
    assert_eq!(ana.m().contacts().unwrap().len(), 1);
    wait(|| ana.transport.destroyed().len() == 1);
    let destroyed = ana.transport.destroyed();
    assert_eq!(destroyed.len(), 1);
    assert!(destroyed[0].starts_with("invite-"));
    // A fresh invite gets a fresh address.
    let second = ana.m().create_invite().unwrap();
    assert_ne!(second, invite);
    eve.m().redeem_invite(&second).unwrap();
    assert_eq!(ana.m().contacts().unwrap().len(), 2);
}

#[test]
fn own_and_broken_invites_are_refused() {
    let network = MemoryNetwork::new();
    let ana = Peer::new(&network, 1, fast());
    ana.enable("Ana");
    let invite = ana.m().create_invite().unwrap();
    assert!(matches!(
        ana.m().redeem_invite(&invite),
        Err(MessagingError::OwnInvite)
    ));
    assert!(matches!(
        ana.m().redeem_invite("Ana#abc"),
        Err(MessagingError::InvalidInvite)
    ));
}

#[test]
fn expired_invites_are_destroyed() {
    let mut config = fast();
    config.invite_ttl = Duration::from_millis(200);
    let network = MemoryNetwork::new();
    let ana = Peer::new(&network, 1, config.clone());
    let bob = Peer::new(&network, 2, config);
    ana.enable("Ana");
    bob.enable("Bob");
    let invite = ana.m().create_invite().unwrap();
    wait(|| ana.transport.destroyed().len() == 1);
    let error = bob.m().redeem_invite(&invite).unwrap_err();
    assert!(matches!(
        error,
        MessagingError::InviteExpired | MessagingError::InviteUnreachable
    ));
    assert!(ana.m().contacts().unwrap().is_empty());
}

#[test]
fn outbox_waits_while_contact_is_offline_then_delivers() {
    let (_net, ana, bob) = pair(fast());
    let bob_id = ana.contact("Bob").id;
    let ana_id = bob.contact("Ana").id;
    bob.transport.set_online(false);
    wait(|| bob.m().status().unwrap().network == NetworkState::Offline);
    ana.m().send(bob_id, "are you there?").unwrap();
    ana.m().send(bob_id, "second").unwrap();
    wait(|| {
        ana.texts(bob_id)
            .iter()
            .all(|(_, _, state)| *state == MessageState::Waiting)
    });
    assert!(ana.events.lock().unwrap().iter().any(|e| matches!(
        e,
        Event::MessageUpdated {
            state: MessageState::Waiting,
            ..
        }
    )));
    bob.transport.set_online(true);
    wait(|| bob.texts(ana_id).len() == 2);
    wait(|| {
        ana.texts(bob_id)
            .iter()
            .all(|(_, _, state)| *state == MessageState::Delivered)
    });
    let order: Vec<_> = bob.texts(ana_id).into_iter().map(|t| t.1).collect();
    assert_eq!(order, ["are you there?", "second"]);
}

#[test]
fn queued_messages_survive_a_restart() {
    let (_net, mut ana, mut bob) = pair(fast());
    let bob_id = ana.contact("Bob").id;
    let ana_id = bob.contact("Ana").id;
    bob.stop();
    ana.m().send(bob_id, "while you were away").unwrap();
    wait(|| ana.texts(bob_id)[0].2 == MessageState::Waiting);
    ana.stop();
    bob.start();
    wait(|| bob.m().status().unwrap().network == NetworkState::Online);
    ana.start();
    // Ana's restart retries the outbox without any user action.
    wait(|| bob.texts(ana_id).len() == 1);
    wait(|| ana.texts(bob_id)[0].2 == MessageState::Delivered);
    assert_eq!(ana.contact("Bob").name, "Bob");
}

#[test]
fn blocking_stops_delivery_until_unblocked() {
    let (_net, ana, bob) = pair(fast());
    let bob_id = ana.contact("Bob").id;
    let ana_id = bob.contact("Ana").id;
    ana.m().set_blocked(bob_id, true).unwrap();
    assert!(ana.contact("Bob").blocked);
    assert!(matches!(
        ana.m().send(bob_id, "x"),
        Err(MessagingError::Blocked)
    ));
    bob.m().send(ana_id, "let me in").unwrap();
    wait(|| bob.texts(ana_id)[0].2 == MessageState::Waiting);
    std::thread::sleep(Duration::from_millis(300));
    assert!(ana.texts(bob_id).is_empty());
    ana.m().set_blocked(bob_id, false).unwrap();
    wait(|| ana.texts(bob_id).len() == 1);
    wait(|| bob.texts(ana_id)[0].2 == MessageState::Delivered);
}

#[test]
fn remove_and_delete_history() {
    let (_net, ana, bob) = pair(fast());
    let bob_id = ana.contact("Bob").id;
    let ana_id = bob.contact("Ana").id;
    bob.m().send(ana_id, "one").unwrap();
    wait(|| ana.texts(bob_id).len() == 1);
    ana.m().delete_history(bob_id).unwrap();
    assert!(ana.texts(bob_id).is_empty());
    assert_eq!(ana.contact("Bob").unread, 0);
    ana.m().rename(bob_id, Some("Bobby")).unwrap();
    assert_eq!(ana.contact("Bobby").display_name, "Bob");
    ana.m().rename(bob_id, None).unwrap();
    ana.contact("Bob");
    ana.m().remove(bob_id).unwrap();
    assert!(ana.m().contacts().unwrap().is_empty());
    assert!(matches!(
        ana.m().send(bob_id, "x"),
        Err(MessagingError::UnknownContact)
    ));
    // Bob's messages to Ana now wait: Ana no longer knows him.
    bob.m().send(ana_id, "hello?").unwrap();
    wait(|| bob.texts(ana_id)[1].2 == MessageState::Waiting);
}

#[test]
fn disable_leaves_the_network_and_enable_resumes() {
    let (_net, ana, bob) = pair(fast());
    let ana_id = bob.contact("Ana").id;
    ana.m().disable().unwrap();
    assert_eq!(ana.m().status().unwrap().network, NetworkState::Disabled);
    assert!(!ana.m().status().unwrap().enabled);
    bob.m().send(ana_id, "ping").unwrap();
    wait(|| bob.texts(ana_id)[0].2 == MessageState::Waiting);
    ana.m().enable(None).unwrap();
    wait(|| bob.texts(ana_id)[0].2 == MessageState::Delivered);
    assert_eq!(ana.m().contacts().unwrap()[0].name, "Bob");
}

#[test]
fn slots_and_promotion() {
    let (_net, ana, _bob) = pair(fast());
    let bob_id = ana.contact("Bob").id;
    ana.m().set_contacts_slot(bob_id, Some(1)).unwrap();
    ana.m().set_main_slot(bob_id, Some(5)).unwrap();
    let bob = ana.contact("Bob");
    assert_eq!((bob.contacts_slot, bob.main_slot), (Some(1), Some(5)));
    assert!(matches!(
        ana.m().set_main_slot(bob_id, Some(2)),
        Err(MessagingError::InvalidSlot)
    ));
    ana.m().set_main_slot(bob_id, None).unwrap();
    assert_eq!(ana.contact("Bob").main_slot, None);
}

#[test]
fn per_contact_rate_limit_drops_the_flood() {
    let mut config = fast();
    config.rate_burst = 3;
    config.rate_refill = Duration::from_secs(60);
    let (_net, ana, bob) = pair(config);
    let bob_id = ana.contact("Bob").id;
    let ana_id = bob.contact("Ana").id;
    for i in 0..6 {
        bob.m().send(ana_id, &format!("m{i}")).unwrap();
    }
    wait(|| ana.texts(bob_id).len() == 3);
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(ana.texts(bob_id).len(), 3);
    let delivered = bob
        .texts(ana_id)
        .iter()
        .filter(|t| t.2 == MessageState::Delivered)
        .count();
    assert_eq!(delivered, 3);
}

#[test]
fn rejects_invalid_messages() {
    let (_net, ana, _bob) = pair(fast());
    let bob_id = ana.contact("Bob").id;
    assert!(matches!(
        ana.m().send(bob_id, "   "),
        Err(MessagingError::InvalidMessage)
    ));
    let big = "x".repeat(phorminx_messaging::MAX_TEXT_BYTES + 1);
    assert!(matches!(
        ana.m().send(bob_id, &big),
        Err(MessagingError::InvalidMessage)
    ));
}

#[test]
fn works_without_restricted_discovery() {
    let network = MemoryNetwork::new();
    let make = |seed: u8| {
        let transport = Arc::new(
            MemoryTransport::new(network.clone(), [seed; 16]).without_restricted_discovery(),
        );
        let dir = tempfile::tempdir().unwrap();
        let messenger = Messenger::start(
            transport,
            &dir.path().join("p.db"),
            Arc::new(XorVault),
            fast(),
            |_| {},
        )
        .unwrap();
        (dir, messenger)
    };
    let (_a_dir, ana) = make(1);
    let (_b_dir, bob) = make(2);
    ana.enable(Some("Ana")).unwrap();
    bob.enable(Some("Bob")).unwrap();
    wait(|| ana.status().unwrap().network == NetworkState::Online);
    wait(|| bob.status().unwrap().network == NetworkState::Online);
    let invite = ana.create_invite().unwrap();
    let contact = bob.redeem_invite(&invite).unwrap();
    bob.send(contact.id, "open service").unwrap();
    wait(|| {
        ana.contacts()
            .unwrap()
            .first()
            .is_some_and(|c| c.unread == 1)
    });
}

#[test]
fn returning_contact_collects_queued_messages_without_waiting_for_backoff() {
    let mut config = fast();
    // Ana would not retry on her own within this test.
    config.retry_delays = vec![Duration::from_secs(600)];
    let (_net, ana, mut bob) = pair(config);
    let bob_id = ana.contact("Bob").id;
    let ana_id = bob.contact("Ana").id;
    // Make Bob known as recently seen on Ana's side, then take him away.
    bob.m().send(ana_id, "hi").unwrap();
    wait(|| ana.texts(bob_id).len() == 1);
    bob.stop();
    ana.m().send(bob_id, "queued").unwrap();
    wait(|| ana.texts(bob_id)[1].2 == MessageState::Waiting);
    bob.start();
    // Bob's engine dials his recent contacts on start; Ana flushes to him.
    wait(|| ana.texts(bob_id)[1].2 == MessageState::Delivered);
    assert_eq!(bob.texts(ana_id).last().unwrap().1, "queued");
}

#[test]
fn open_invites_are_capped() {
    let network = MemoryNetwork::new();
    let ana = Peer::new(&network, 1, fast());
    ana.enable("Ana");
    for _ in 0..20 {
        ana.m().create_invite().unwrap();
    }
    assert!(matches!(
        ana.m().create_invite(),
        Err(MessagingError::TooManyInvites)
    ));
}
