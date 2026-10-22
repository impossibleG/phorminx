//! The messaging engine: a single actor task that owns the database, the
//! onion services and the session table. Handshakes and sessions run as their
//! own tasks and talk to the actor over channels.
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use phorminx_persistence::{
    MESSAGING_SPARE_INVITE_EXPIRY, MessagingContactRecord, MessagingDirection,
    MessagingIdentityRecord, MessagingInviteRecord, MessagingMessageRecord, MessagingMessageState,
    NewMessagingContact, Persistence,
};
use tokio::sync::{mpsc, oneshot};
use tokio::time::Instant;

use crate::identity::{
    Identity, PublicKey, SecretBytes, new_uuid, random_bytes, same_secret, sha256,
};
use crate::invite::{INVITE_SECRET_LEN, Invite, OnionAddress};
use crate::protocol::{Frame, valid_name, valid_text};
use crate::session::{self, Ctx, RateLimit, SessionCmd};
use crate::transport::{BoxStream, NetStatus, Transport, TransportError};
use crate::{
    Config, ContactView, Event, EventSink, KeyVault, MessageState, MessageView, MessagingError,
    NetworkState, StatusView,
};

const CONTACTS_SERVICE: &str = "contacts";
const INVITE_PREFIX: &str = "invite-";
const OUTBOX_BATCH: usize = 100;
/// Inbound connections served at once; more are dropped unanswered.
const MAX_INBOUND: usize = 64;
/// Live invites (each one is an onion service) at once.
const MAX_INVITES: usize = 20;

pub(crate) type Reply<T> = std::sync::mpsc::SyncSender<Result<T, MessagingError>>;

pub(crate) enum Request {
    Status(Reply<StatusView>),
    Enable {
        display_name: Option<String>,
        reply: Reply<()>,
    },
    Disable(Reply<()>),
    SetDisplayName {
        display_name: String,
        reply: Reply<()>,
    },
    CreateInvite(Reply<String>),
    RedeemInvite {
        invite: String,
        reply: Reply<ContactView>,
    },
    Contacts(Reply<Vec<ContactView>>),
    Conversation {
        contact_id: i64,
        limit: usize,
        reply: Reply<Vec<MessageView>>,
    },
    Send {
        contact_id: i64,
        text: String,
        reply: Reply<MessageView>,
    },
    MarkRead {
        contact_id: i64,
        reply: Reply<()>,
    },
    Rename {
        contact_id: i64,
        nickname: Option<String>,
        reply: Reply<()>,
    },
    SetBlocked {
        contact_id: i64,
        blocked: bool,
        reply: Reply<()>,
    },
    Remove {
        contact_id: i64,
        reply: Reply<()>,
    },
    SetContactsSlot {
        contact_id: i64,
        slot: Option<u8>,
        reply: Reply<()>,
    },
    SetMainSlot {
        contact_id: i64,
        slot: Option<u8>,
        reply: Reply<()>,
    },
    DeleteHistory {
        contact_id: i64,
        reply: Reply<()>,
    },
    Shutdown(std::sync::mpsc::SyncSender<()>),
}

#[derive(Clone, Debug)]
pub(crate) struct PeerHello {
    pub key: PublicKey,
    pub display_name: String,
}

pub(crate) enum ServiceKind {
    Contacts,
    Invite(String),
}

pub(crate) enum Internal {
    Request(Request),
    NetStarted(Result<(), TransportError>),
    NetStatus(NetStatus),
    Inbound {
        kind: ServiceKind,
        stream: BoxStream,
    },
    LookupContact {
        key: PublicKey,
        reply: oneshot::Sender<Option<i64>>,
    },
    PeerName {
        contact_id: i64,
        display_name: String,
    },
    AcceptInvite {
        invite_id: String,
        secret: Vec<u8>,
        peer: PeerHello,
        contacts_address: OnionAddress,
        discovery_secret: Option<SecretBytes>,
        reply: oneshot::Sender<Option<(OnionAddress, Option<SecretBytes>)>>,
    },
    /// An invite connection ended; a used invite can now be torn down.
    InviteFinished {
        invite_id: String,
    },
    RedeemDone {
        result: Result<session::Redeemed, MessagingError>,
        our_discovery_public: Option<[u8; 32]>,
        reply: Reply<ContactView>,
    },
    SessionUp {
        contact_id: i64,
        tx: mpsc::Sender<SessionCmd>,
        id_reply: oneshot::Sender<u64>,
    },
    SessionDown {
        contact_id: i64,
        session_id: u64,
    },
    DialFailed {
        contact_id: i64,
    },
    Incoming {
        contact_id: i64,
        id: String,
        sent_at: i64,
        text: String,
        reply: oneshot::Sender<bool>,
    },
    Acked {
        contact_id: i64,
        id: String,
    },
    Tick,
}

struct Session {
    id: u64,
    tx: mpsc::Sender<SessionCmd>,
    inflight: HashSet<String>,
}

struct Backoff {
    failures: u32,
    next_at: Instant,
}

pub(crate) struct Engine<T: Transport> {
    transport: Arc<T>,
    store: Persistence,
    vault: Arc<dyn KeyVault>,
    config: Arc<Config>,
    events: EventSink,
    actor: mpsc::UnboundedSender<Internal>,
    identity: Option<Arc<Identity>>,
    display_name: String,
    enabled: bool,
    net: NetworkState,
    next_start_at: Option<Instant>,
    contacts_address: Option<OnionAddress>,
    contacts_service: Option<Box<dyn Send + Sync>>,
    invite_services: HashMap<String, (OnionAddress, Box<dyn Send + Sync>)>,
    sessions: HashMap<i64, Session>,
    dialing: HashSet<i64>,
    backoff: HashMap<i64, Backoff>,
    rates: HashMap<i64, RateLimit>,
    inbound_active: Arc<std::sync::atomic::AtomicUsize>,
    next_session_id: u64,
}

pub(crate) fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn store_err(_: phorminx_persistence::PersistenceError) -> MessagingError {
    MessagingError::Storage
}

impl<T: Transport> Engine<T> {
    pub(crate) fn new(
        transport: Arc<T>,
        store: Persistence,
        vault: Arc<dyn KeyVault>,
        config: Arc<Config>,
        events: EventSink,
        actor: mpsc::UnboundedSender<Internal>,
    ) -> Self {
        Self {
            transport,
            store,
            vault,
            config,
            events,
            actor,
            identity: None,
            display_name: String::new(),
            enabled: false,
            net: NetworkState::Disabled,
            next_start_at: None,
            contacts_address: None,
            contacts_service: None,
            invite_services: HashMap::new(),
            sessions: HashMap::new(),
            dialing: HashSet::new(),
            backoff: HashMap::new(),
            rates: HashMap::new(),
            inbound_active: Arc::default(),
            next_session_id: 1,
        }
    }

    pub(crate) async fn run(mut self, mut inbox: mpsc::UnboundedReceiver<Internal>) {
        self.resume();
        let mut ticker = tokio::time::interval(self.config.tick);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            let message = tokio::select! {
                message = inbox.recv() => match message {
                    Some(message) => message,
                    None => break,
                },
                _ = ticker.tick() => Internal::Tick,
            };
            if let Internal::Request(Request::Shutdown(done)) = message {
                self.go_offline();
                let _ = done.send(());
                break;
            }
            self.handle(message);
        }
    }

    /// Restores the previous session: messaging that was enabled starts again.
    fn resume(&mut self) {
        let Ok(Some(record)) = self.store.messaging().identity() else {
            return;
        };
        self.display_name = record.display_name.clone();
        if record.enabled {
            match self.load_identity(&record) {
                Ok(()) => {
                    self.enabled = true;
                    self.start_network();
                }
                Err(_) => {
                    self.net = NetworkState::Offline;
                }
            }
        }
    }

    fn load_identity(&mut self, record: &MessagingIdentityRecord) -> Result<(), MessagingError> {
        let seed = self.vault.expose(&record.protected_secret)?;
        let identity = Identity::from_seed(seed.as_bytes())?;
        if identity.public_key().0[..] != record.public_key[..] {
            return Err(MessagingError::Identity);
        }
        self.identity = Some(Arc::new(identity));
        self.contacts_address = record
            .contacts_address
            .as_deref()
            .and_then(OnionAddress::parse);
        Ok(())
    }

    fn emit(&self, event: Event) {
        (self.events)(event);
    }

    fn set_net(&mut self, state: NetworkState) {
        if self.net != state {
            self.net = state;
            self.emit(Event::Status(state));
        }
    }

    fn ctx(&self) -> Option<Ctx> {
        Some(Ctx {
            identity: self.identity.clone()?,
            display_name: self.display_name.clone(),
            actor: self.actor.clone(),
            config: self.config.clone(),
        })
    }

    fn start_network(&mut self) {
        self.set_net(NetworkState::Starting);
        self.next_start_at = None;
        let transport = self.transport.clone();
        let actor = self.actor.clone();
        tokio::spawn(async move {
            let result = transport.start().await;
            let _ = actor.send(Internal::NetStarted(result));
        });
    }

    fn online(&self) -> bool {
        self.enabled && self.net == NetworkState::Online
    }

    fn handle(&mut self, message: Internal) {
        match message {
            Internal::Request(request) => self.request(request),
            Internal::NetStarted(result) => self.net_started(result),
            Internal::NetStatus(status) => self.net_status(status),
            Internal::Inbound { kind, stream } => self.inbound(kind, stream),
            Internal::LookupContact { key, reply } => {
                let id = self
                    .store
                    .messaging()
                    .contact_by_key(&key.0)
                    .ok()
                    .flatten()
                    .filter(|c| !c.blocked)
                    .map(|c| c.id);
                let _ = reply.send(id);
            }
            Internal::PeerName {
                contact_id,
                display_name,
            } => {
                let repo = self.store.messaging();
                if let Ok(Some(contact)) = repo.contact(contact_id)
                    && contact.display_name != display_name
                    && repo.set_display_name_for(contact_id, &display_name).is_ok()
                {
                    self.emit(Event::ContactsChanged);
                }
            }
            Internal::AcceptInvite {
                invite_id,
                secret,
                peer,
                contacts_address,
                discovery_secret,
                reply,
            } => {
                let result = self.accept_invite(
                    &invite_id,
                    &secret,
                    &peer,
                    &contacts_address,
                    discovery_secret,
                );
                let _ = reply.send(result);
            }
            Internal::InviteFinished { invite_id } => {
                let still_open = self
                    .store
                    .messaging()
                    .invites()
                    .is_ok_and(|all| all.iter().any(|i| i.id == invite_id));
                if !still_open {
                    self.retire_invite(&invite_id);
                }
            }
            Internal::RedeemDone {
                result,
                our_discovery_public,
                reply,
            } => {
                let outcome = result.and_then(|r| self.finish_redeem(r, our_discovery_public));
                let _ = reply.send(outcome);
            }
            Internal::SessionUp {
                contact_id,
                tx,
                id_reply,
            } => self.session_up(contact_id, tx, id_reply),
            Internal::SessionDown {
                contact_id,
                session_id,
            } => self.session_down(contact_id, session_id),
            Internal::DialFailed { contact_id } => {
                self.dialing.remove(&contact_id);
                let pending = self
                    .store
                    .messaging()
                    .outbox(contact_id, 1)
                    .is_ok_and(|o| !o.is_empty());
                if pending {
                    self.attempt_failed(contact_id);
                }
            }
            Internal::Incoming {
                contact_id,
                id,
                sent_at,
                text,
                reply,
            } => {
                let _ = reply.send(self.incoming(contact_id, &id, sent_at, &text));
            }
            Internal::Acked { contact_id, id } => self.acked(contact_id, &id),
            Internal::Tick => self.tick(),
        }
    }

    fn request(&mut self, request: Request) {
        match request {
            Request::Status(reply) => {
                let _ = reply.send(self.status());
            }
            Request::Enable {
                display_name,
                reply,
            } => {
                let _ = reply.send(self.enable(display_name));
            }
            Request::Disable(reply) => {
                let _ = reply.send(self.disable());
            }
            Request::SetDisplayName {
                display_name,
                reply,
            } => {
                let display_name = display_name.trim().to_string();
                let result = if !valid_name(&display_name) {
                    Err(MessagingError::InvalidName)
                } else {
                    self.store
                        .messaging()
                        .set_display_name(&display_name)
                        .map_err(store_err)
                        .map(|_| self.display_name = display_name)
                };
                let _ = reply.send(result);
            }
            Request::CreateInvite(reply) => {
                let _ = reply.send(self.create_invite());
            }
            Request::RedeemInvite { invite, reply } => {
                if let Err(error) = self.start_redeem(&invite, reply.clone()) {
                    let _ = reply.send(Err(error));
                }
            }
            Request::Contacts(reply) => {
                let _ = reply.send(self.contacts());
            }
            Request::Conversation {
                contact_id,
                limit,
                reply,
            } => {
                let result = self
                    .store
                    .messaging()
                    .conversation(contact_id, limit)
                    .map_err(store_err)
                    .map(|rows| rows.into_iter().map(MessageView::from).collect());
                let _ = reply.send(result);
            }
            Request::Send {
                contact_id,
                text,
                reply,
            } => {
                let _ = reply.send(self.send(contact_id, &text));
            }
            Request::MarkRead { contact_id, reply } => {
                let result = self
                    .store
                    .messaging()
                    .mark_read(contact_id)
                    .map_err(store_err);
                if result.as_ref().is_ok_and(|n| *n > 0) {
                    self.emit(Event::ContactsChanged);
                }
                let _ = reply.send(result.map(|_| ()));
            }
            Request::Rename {
                contact_id,
                nickname,
                reply,
            } => {
                let result = self
                    .store
                    .messaging()
                    .rename_contact(contact_id, nickname.as_deref())
                    .map_err(|_| MessagingError::InvalidName);
                self.contacts_changed(&result);
                let _ = reply.send(result);
            }
            Request::SetBlocked {
                contact_id,
                blocked,
                reply,
            } => {
                let result = self
                    .store
                    .messaging()
                    .set_blocked(contact_id, blocked)
                    .map_err(|_| MessagingError::UnknownContact);
                if result.is_ok() {
                    if blocked {
                        self.close_session(contact_id);
                    }
                    self.refresh_contacts_service();
                }
                self.contacts_changed(&result);
                let _ = reply.send(result);
            }
            Request::Remove { contact_id, reply } => {
                let result = self
                    .store
                    .messaging()
                    .remove_contact(contact_id)
                    .map_err(|_| MessagingError::UnknownContact);
                if result.is_ok() {
                    self.close_session(contact_id);
                    self.backoff.remove(&contact_id);
                    self.refresh_contacts_service();
                }
                self.contacts_changed(&result);
                let _ = reply.send(result);
            }
            Request::SetContactsSlot {
                contact_id,
                slot,
                reply,
            } => {
                let result = self
                    .store
                    .messaging()
                    .set_contacts_slot(contact_id, slot)
                    .map_err(|_| MessagingError::InvalidSlot);
                self.contacts_changed(&result);
                let _ = reply.send(result);
            }
            Request::SetMainSlot {
                contact_id,
                slot,
                reply,
            } => {
                let result = self
                    .store
                    .messaging()
                    .set_main_slot(contact_id, slot)
                    .map_err(|_| MessagingError::InvalidSlot);
                self.contacts_changed(&result);
                let _ = reply.send(result);
            }
            Request::DeleteHistory { contact_id, reply } => {
                let result = self
                    .store
                    .messaging()
                    .delete_history(contact_id)
                    .map(|_| ())
                    .map_err(store_err);
                if result.is_ok() {
                    if let Some(session) = self.sessions.get_mut(&contact_id) {
                        session.inflight.clear();
                    }
                    self.emit(Event::ConversationChanged { contact_id });
                }
                self.contacts_changed(&result);
                let _ = reply.send(result);
            }
            Request::Shutdown(done) => {
                let _ = done.send(());
            }
        }
    }

    fn contacts_changed<R>(&self, result: &Result<R, MessagingError>) {
        if result.is_ok() {
            self.emit(Event::ContactsChanged);
        }
    }

    fn status(&self) -> Result<StatusView, MessagingError> {
        let identity = self.store.messaging().identity().map_err(store_err)?;
        Ok(StatusView {
            enabled: self.enabled,
            has_identity: identity.is_some(),
            display_name: identity.map(|i| i.display_name),
            network: self.net,
        })
    }

    fn enable(&mut self, display_name: Option<String>) -> Result<(), MessagingError> {
        let repo = self.store.messaging();
        let record = match repo.identity().map_err(store_err)? {
            Some(record) => {
                if let Some(name) = display_name.as_deref().map(str::trim)
                    && name != record.display_name
                {
                    if !valid_name(name) {
                        return Err(MessagingError::InvalidName);
                    }
                    repo.set_display_name(name).map_err(store_err)?;
                }
                repo.set_enabled(true).map_err(store_err)?;
                repo.identity()
                    .map_err(store_err)?
                    .ok_or(MessagingError::Storage)?
            }
            None => {
                let name = display_name
                    .as_deref()
                    .map(str::trim)
                    .filter(|n| valid_name(n))
                    .ok_or(MessagingError::InvalidName)?;
                let (identity, seed) = Identity::generate()?;
                let record = MessagingIdentityRecord {
                    display_name: name.to_string(),
                    public_key: identity.public_key().0.to_vec(),
                    protected_secret: self.vault.protect(seed.as_bytes())?,
                    enabled: true,
                    contacts_address: None,
                    created_at_ms: now_ms(),
                };
                repo.create_identity(&record).map_err(store_err)?;
                record
            }
        };
        self.display_name = record.display_name.clone();
        if self.identity.is_none() {
            self.load_identity(&record)?;
        }
        if !self.enabled {
            self.enabled = true;
            self.start_network();
        }
        Ok(())
    }

    fn disable(&mut self) -> Result<(), MessagingError> {
        self.store
            .messaging()
            .set_enabled(false)
            .map_err(store_err)?;
        self.enabled = false;
        self.go_offline();
        self.set_net(NetworkState::Disabled);
        Ok(())
    }

    /// Closes every session and service and leaves the Tor network.
    fn go_offline(&mut self) {
        for (_, session) in self.sessions.drain() {
            let _ = session.tx.try_send(SessionCmd::Close);
        }
        self.contacts_service = None;
        self.invite_services.clear();
        self.dialing.clear();
        self.transport.stop();
    }

    fn net_started(&mut self, result: Result<(), TransportError>) {
        if !self.enabled {
            self.transport.stop();
            return;
        }
        match result {
            Ok(()) => {
                self.set_net(NetworkState::Online);
                let mut status = self.transport.status();
                let actor = self.actor.clone();
                tokio::spawn(async move {
                    while status.changed().await.is_ok() {
                        let value = *status.borrow_and_update();
                        if actor.send(Internal::NetStatus(value)).is_err() {
                            break;
                        }
                    }
                });
                self.came_online();
            }
            Err(_) => {
                self.set_net(NetworkState::Offline);
                self.next_start_at = Some(Instant::now() + self.config.restart_delay);
            }
        }
    }

    fn net_status(&mut self, status: NetStatus) {
        if !self.enabled {
            return;
        }
        let state = match status {
            NetStatus::Online => NetworkState::Online,
            NetStatus::Starting => NetworkState::Starting,
            NetStatus::Offline | NetStatus::Stopped => NetworkState::Offline,
        };
        let was_online = self.net == NetworkState::Online;
        self.set_net(state);
        if state == NetworkState::Online && !was_online {
            self.came_online();
        }
    }

    /// Publishes our services and retries the outbox right away.
    fn came_online(&mut self) {
        if let Err(_e) = self.ensure_contacts_address() {
            return;
        }
        self.refresh_contacts_service();
        self.relaunch_invites();
        self.ensure_spare_invite();
        self.backoff.clear();
        self.retry_outbox(true);
        self.announce();
    }

    /// Dials recently seen contacts once after coming online. Their engines
    /// then flush anything they queued for us while we were away, instead of
    /// waiting for their next retry.
    fn announce(&mut self) {
        let Ok(mut contacts) = self.store.messaging().contacts() else {
            return;
        };
        contacts.retain(|c| !c.blocked);
        contacts.sort_by_key(|c| std::cmp::Reverse(c.last_seen_ms.unwrap_or(c.added_at_ms)));
        for contact in contacts.into_iter().take(self.config.announce_limit) {
            self.dial(contact.id);
        }
    }

    fn ensure_contacts_address(&mut self) -> Result<OnionAddress, MessagingError> {
        let address = self
            .transport
            .service_address(CONTACTS_SERVICE)
            .map_err(|_| MessagingError::Network)?;
        if self.contacts_address.as_ref() != Some(&address) {
            self.store
                .messaging()
                .set_contacts_address(&address.to_string())
                .map_err(store_err)?;
            self.contacts_address = Some(address.clone());
        }
        Ok(address)
    }

    /// Runs the contacts service only while someone may call: with restricted
    /// discovery only holders of an authorized key can even find it.
    fn refresh_contacts_service(&mut self) {
        if !self.online() {
            return;
        }
        let Ok(contacts) = self.store.messaging().contacts() else {
            return;
        };
        let active: Vec<&MessagingContactRecord> = contacts.iter().filter(|c| !c.blocked).collect();
        if active.is_empty() {
            self.contacts_service = None;
            return;
        }
        let restricted = self.transport.supports_restricted_discovery();
        let keys: Vec<[u8; 32]> = active
            .iter()
            .filter_map(|c| c.discovery_key.as_deref())
            .filter_map(|k| k.try_into().ok())
            .collect();
        if restricted && keys.is_empty() {
            self.contacts_service = None;
            return;
        }
        let authorized = restricted.then_some(keys);
        if self.contacts_service.is_some() {
            if let Some(keys) = authorized
                && self.transport.reauthorize(CONTACTS_SERVICE, keys).is_err()
            {
                self.contacts_service = None;
                self.refresh_contacts_service();
            }
            return;
        }
        match self.transport.launch(CONTACTS_SERVICE, authorized) {
            Ok(handle) => {
                self.forward_incoming(handle.incoming, ServiceKind::Contacts);
                self.contacts_service = Some(handle._guard);
            }
            Err(_) => self.set_net(NetworkState::Offline),
        }
    }

    fn forward_incoming(&self, mut incoming: mpsc::Receiver<BoxStream>, kind: ServiceKind) {
        let actor = self.actor.clone();
        tokio::spawn(async move {
            while let Some(stream) = incoming.recv().await {
                let kind = match &kind {
                    ServiceKind::Contacts => ServiceKind::Contacts,
                    ServiceKind::Invite(id) => ServiceKind::Invite(id.clone()),
                };
                if actor.send(Internal::Inbound { kind, stream }).is_err() {
                    break;
                }
            }
        });
    }

    fn inbound(&mut self, kind: ServiceKind, stream: BoxStream) {
        use std::sync::atomic::Ordering;
        let Some(ctx) = self.ctx() else {
            return;
        };
        let active = self.inbound_active.clone();
        if active.fetch_add(1, Ordering::AcqRel) >= MAX_INBOUND {
            // A flood of callers: drop the newest without answering.
            active.fetch_sub(1, Ordering::AcqRel);
            return;
        }
        let release = move || {
            active.fetch_sub(1, Ordering::AcqRel);
        };
        match kind {
            ServiceKind::Contacts => {
                if let Some(address) = self.contacts_address.clone() {
                    tokio::spawn(async move {
                        session::accept_contact(stream, ctx, address).await;
                        release();
                    });
                    return;
                }
            }
            ServiceKind::Invite(id) => {
                if let Some((address, _)) = self.invite_services.get(&id) {
                    let address = address.clone();
                    tokio::spawn(async move {
                        session::accept_invite(stream, ctx, id, address).await;
                        release();
                    });
                    return;
                }
            }
        }
        release();
    }

    fn create_invite(&mut self) -> Result<String, MessagingError> {
        if !self.online() {
            return Err(MessagingError::NotOnline);
        }
        let repo = self.store.messaging();
        let invites = repo.invites().map_err(store_err)?;
        let handed_out = invites
            .iter()
            .filter(|i| i.expires_at_ms != MESSAGING_SPARE_INVITE_EXPIRY)
            .count();
        if handed_out >= MAX_INVITES {
            return Err(MessagingError::TooManyInvites);
        }
        let secret = random_bytes::<INVITE_SECRET_LEN>()?;
        let now = now_ms();
        let expires_at_ms = now + self.config.invite_ttl.as_millis() as i64;
        // Prefer the spare: it was published in advance, so it is usually
        // reachable by the time the person receives the invite.
        let spare = invites
            .iter()
            .filter(|i| i.expires_at_ms == MESSAGING_SPARE_INVITE_EXPIRY)
            .find(|i| self.invite_services.contains_key(&i.id))
            .map(|i| i.id.clone());
        let id = match spare {
            Some(id)
                if repo
                    .claim_spare_invite(&id, &sha256(&secret), now, expires_at_ms)
                    .map_err(store_err)? =>
            {
                id
            }
            _ => self.launch_invite(&sha256(&secret), now, expires_at_ms)?,
        };
        let address = self
            .invite_services
            .get(&id)
            .map(|(address, _)| address.clone())
            .ok_or(MessagingError::Network)?;
        self.ensure_spare_invite();
        Ok(Invite {
            name: self.display_name.clone(),
            address,
            secret,
            expires_at: (expires_at_ms / 1000) as u64,
        }
        .encode())
    }

    /// Publishes a new invite service and records it.
    fn launch_invite(
        &mut self,
        secret_hash: &[u8],
        created_at_ms: i64,
        expires_at_ms: i64,
    ) -> Result<String, MessagingError> {
        let id = random_bytes::<8>()?
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        let handle = self
            .transport
            .launch(&format!("{INVITE_PREFIX}{id}"), None)
            .map_err(|_| MessagingError::Network)?;
        self.store
            .messaging()
            .insert_invite(&MessagingInviteRecord {
                id: id.clone(),
                secret_hash: secret_hash.to_vec(),
                address: handle.address.to_string(),
                created_at_ms,
                expires_at_ms,
            })
            .map_err(store_err)?;
        self.forward_incoming(handle.incoming, ServiceKind::Invite(id.clone()));
        self.invite_services
            .insert(id.clone(), (handle.address, handle._guard));
        Ok(id)
    }

    /// Keeps one invite service published ahead of time. Its stored secret
    /// is random and never shared, so nobody can redeem it until it is
    /// handed out with a real secret.
    fn ensure_spare_invite(&mut self) {
        if !self.online() {
            return;
        }
        let has_spare = self.store.messaging().invites().is_ok_and(|all| {
            all.iter()
                .any(|i| i.expires_at_ms == MESSAGING_SPARE_INVITE_EXPIRY)
        });
        if has_spare {
            return;
        }
        if let Ok(placeholder) = random_bytes::<32>() {
            let _ = self.launch_invite(&placeholder, now_ms(), MESSAGING_SPARE_INVITE_EXPIRY);
        }
    }

    fn relaunch_invites(&mut self) {
        let Ok(invites) = self.store.messaging().invites() else {
            return;
        };
        let now = now_ms();
        for invite in invites {
            if invite.expires_at_ms <= now {
                self.retire_invite(&invite.id);
                continue;
            }
            if self.invite_services.contains_key(&invite.id) {
                continue;
            }
            if let Ok(handle) = self
                .transport
                .launch(&format!("{INVITE_PREFIX}{}", invite.id), None)
            {
                self.forward_incoming(handle.incoming, ServiceKind::Invite(invite.id.clone()));
                self.invite_services
                    .insert(invite.id, (handle.address, handle._guard));
            }
        }
    }

    /// Stops an invite's service, deletes its keys and forgets it.
    fn retire_invite(&mut self, id: &str) {
        self.invite_services.remove(id);
        let _ = self.transport.destroy(&format!("{INVITE_PREFIX}{id}"));
        let _ = self.store.messaging().delete_invite(id);
    }

    fn accept_invite(
        &mut self,
        invite_id: &str,
        secret: &[u8],
        peer: &PeerHello,
        contacts_address: &OnionAddress,
        discovery_secret: Option<SecretBytes>,
    ) -> Option<(OnionAddress, Option<SecretBytes>)> {
        let our_key = self.identity.as_ref()?.public_key();
        let invite = self
            .store
            .messaging()
            .invites()
            .ok()?
            .into_iter()
            .find(|i| i.id == invite_id)?;
        if invite.expires_at_ms <= now_ms()
            || invite.expires_at_ms == MESSAGING_SPARE_INVITE_EXPIRY
            || !same_secret(&sha256(secret), &invite.secret_hash)
            || peer.key == our_key
            || !valid_name(&peer.display_name)
        {
            return None;
        }
        let repo = self.store.messaging();
        if repo
            .contact_by_key(&peer.key.0)
            .ok()
            .flatten()
            .is_some_and(|c| c.blocked)
        {
            return None;
        }
        // Deleting the row first makes the invite single-use even if two
        // redeemers race.
        if !repo.delete_invite(invite_id).ok()? {
            return None;
        }
        let our_address = self.ensure_contacts_address().ok()?;
        let keypair = if self.transport.supports_restricted_discovery() {
            Some(self.transport.new_discovery_keypair().ok()?)
        } else {
            None
        };
        let contact_id = self
            .store
            .messaging()
            .upsert_contact(
                &NewMessagingContact {
                    identity_key: &peer.key.0,
                    contacts_address: &contacts_address.to_string(),
                    display_name: &peer.display_name,
                    discovery_key: keypair.as_ref().map(|k| &k.public[..]),
                },
                now_ms(),
            )
            .ok()?;
        if let Some(secret) = discovery_secret {
            let _ = self
                .transport
                .add_discovery_secret(contacts_address, secret.as_bytes());
        }
        // The service itself is retired once this connection has replied
        // (see `InviteFinished`); the deleted row already makes it single-use.
        self.refresh_contacts_service();
        self.emit(Event::ContactsChanged);
        self.emit(Event::ContactAdded { contact_id });
        Some((our_address, keypair.map(|k| k.secret)))
    }

    fn start_redeem(
        &mut self,
        text: &str,
        reply: Reply<ContactView>,
    ) -> Result<(), MessagingError> {
        let invite = Invite::decode(text)?;
        if invite.expires_at <= (now_ms() / 1000) as u64 {
            return Err(MessagingError::InviteExpired);
        }
        if self
            .invite_services
            .values()
            .any(|(address, _)| *address == invite.address)
        {
            return Err(MessagingError::OwnInvite);
        }
        if !self.online() {
            return Err(MessagingError::NotOnline);
        }
        let ctx = self.ctx().ok_or(MessagingError::NotOnline)?;
        let our_address = self.ensure_contacts_address()?;
        let keypair = if self.transport.supports_restricted_discovery() {
            Some(
                self.transport
                    .new_discovery_keypair()
                    .map_err(|_| MessagingError::Network)?,
            )
        } else {
            None
        };
        let our_discovery_public = keypair.as_ref().map(|k| k.public);
        let transport = self.transport.clone();
        let actor = self.actor.clone();
        tokio::spawn(async move {
            let address = invite.address.clone();
            let connect = move || {
                let transport = transport.clone();
                let address = address.clone();
                async move { transport.connect(&address).await }
            };
            let result =
                session::redeem(ctx, connect, invite, our_address, keypair.map(|k| k.secret)).await;
            let _ = actor.send(Internal::RedeemDone {
                result,
                our_discovery_public,
                reply,
            });
        });
        Ok(())
    }

    fn finish_redeem(
        &mut self,
        redeemed: session::Redeemed,
        our_discovery_public: Option<[u8; 32]>,
    ) -> Result<ContactView, MessagingError> {
        if !valid_name(&redeemed.peer.display_name) {
            return Err(MessagingError::InviteRejected);
        }
        let repo = self.store.messaging();
        if repo
            .contact_by_key(&redeemed.peer.key.0)
            .map_err(store_err)?
            .is_some_and(|c| c.blocked)
        {
            return Err(MessagingError::InviteRejected);
        }
        let contact_id = repo
            .upsert_contact(
                &NewMessagingContact {
                    identity_key: &redeemed.peer.key.0,
                    contacts_address: &redeemed.contacts_address.to_string(),
                    display_name: &redeemed.peer.display_name,
                    discovery_key: our_discovery_public.as_ref().map(|k| &k[..]),
                },
                now_ms(),
            )
            .map_err(store_err)?;
        if let Some(secret) = &redeemed.discovery_secret {
            self.transport
                .add_discovery_secret(&redeemed.contacts_address, secret.as_bytes())
                .map_err(|_| MessagingError::Network)?;
        }
        self.refresh_contacts_service();
        self.emit(Event::ContactsChanged);
        self.emit(Event::ContactAdded { contact_id });
        self.contacts()?
            .into_iter()
            .find(|c| c.id == contact_id)
            .ok_or(MessagingError::Storage)
    }

    fn contacts(&self) -> Result<Vec<ContactView>, MessagingError> {
        let repo = self.store.messaging();
        let unread: HashMap<i64, u32> = repo
            .unread_counts()
            .map_err(store_err)?
            .into_iter()
            .collect();
        Ok(repo
            .contacts()
            .map_err(store_err)?
            .into_iter()
            .map(|c| ContactView {
                id: c.id,
                name: c.shown_name().to_string(),
                display_name: c.display_name.clone(),
                nickname: c.nickname.clone(),
                contacts_slot: c.contacts_slot,
                main_slot: c.main_slot,
                blocked: c.blocked,
                unread: unread.get(&c.id).copied().unwrap_or(0),
                connected: self.sessions.contains_key(&c.id),
                last_seen_ms: c.last_seen_ms,
            })
            .collect())
    }

    fn send(&mut self, contact_id: i64, text: &str) -> Result<MessageView, MessagingError> {
        if self.identity.is_none() {
            return Err(MessagingError::NotOnline);
        }
        if !valid_text(text) {
            return Err(MessagingError::InvalidMessage);
        }
        let repo = self.store.messaging();
        let contact = repo
            .contact(contact_id)
            .map_err(store_err)?
            .ok_or(MessagingError::UnknownContact)?;
        if contact.blocked {
            return Err(MessagingError::Blocked);
        }
        let uuid = new_uuid()?;
        let row = repo
            .insert_outgoing(contact_id, &uuid, text, now_ms())
            .map_err(store_err)?;
        if self.sessions.contains_key(&contact_id) {
            self.flush(contact_id);
        } else if self.online() {
            self.dial(contact_id);
        } else {
            self.mark_waiting(contact_id);
        }
        let message = self
            .store
            .messaging()
            .message(row)
            .map_err(store_err)?
            .ok_or(MessagingError::Storage)?;
        Ok(MessageView::from(message))
    }

    fn dial(&mut self, contact_id: i64) {
        if !self.online()
            || self.dialing.contains(&contact_id)
            || self.sessions.contains_key(&contact_id)
        {
            return;
        }
        let Some(ctx) = self.ctx() else {
            return;
        };
        let Ok(Some(contact)) = self.store.messaging().contact(contact_id) else {
            return;
        };
        if contact.blocked {
            return;
        }
        let (Some(key), Some(address)) = (
            PublicKey::from_slice(&contact.identity_key),
            OnionAddress::parse(&contact.contacts_address),
        ) else {
            return;
        };
        self.dialing.insert(contact_id);
        let transport = self.transport.clone();
        let dial_address = address.clone();
        tokio::spawn(async move {
            let connect = async move { transport.connect(&dial_address).await };
            session::dial_contact(ctx, connect, contact_id, key, address).await;
        });
    }

    fn session_up(
        &mut self,
        contact_id: i64,
        tx: mpsc::Sender<SessionCmd>,
        id_reply: oneshot::Sender<u64>,
    ) {
        self.dialing.remove(&contact_id);
        let allowed = self
            .store
            .messaging()
            .contact(contact_id)
            .ok()
            .flatten()
            .is_some_and(|c| !c.blocked);
        if !allowed || !self.enabled {
            let _ = tx.try_send(SessionCmd::Close);
            return;
        }
        let id = self.next_session_id;
        self.next_session_id += 1;
        if id_reply.send(id).is_err() {
            return;
        }
        let was_connected = self.sessions.contains_key(&contact_id);
        self.sessions.insert(
            contact_id,
            Session {
                id,
                tx,
                inflight: HashSet::new(),
            },
        );
        self.backoff.remove(&contact_id);
        let _ = self.store.messaging().touch_contact(contact_id, now_ms());
        if !was_connected {
            self.emit(Event::Presence {
                contact_id,
                connected: true,
            });
        }
        self.flush(contact_id);
    }

    fn session_down(&mut self, contact_id: i64, session_id: u64) {
        if self
            .sessions
            .get(&contact_id)
            .is_some_and(|s| s.id == session_id)
        {
            self.sessions.remove(&contact_id);
            self.emit(Event::Presence {
                contact_id,
                connected: false,
            });
            let pending = self
                .store
                .messaging()
                .outbox(contact_id, 1)
                .map(|o| !o.is_empty())
                .unwrap_or(false);
            if pending {
                self.attempt_failed(contact_id);
            }
        }
    }

    /// Sends every undelivered message that is not already in flight.
    fn flush(&mut self, contact_id: i64) {
        let Ok(outbox) = self.store.messaging().outbox(contact_id, OUTBOX_BATCH) else {
            return;
        };
        let Some(session) = self.sessions.get_mut(&contact_id) else {
            return;
        };
        let mut changed = Vec::new();
        for message in outbox {
            if !session.inflight.insert(message.message_uuid.clone()) {
                continue;
            }
            let frame = Frame::Message {
                id: message.message_uuid.clone(),
                sent_at: message.sent_at_ms,
                text: message.text.clone(),
            };
            if session.tx.try_send(SessionCmd::Send(frame)).is_err() {
                session.inflight.remove(&message.message_uuid);
                break;
            }
            if message.state == MessagingMessageState::Waiting {
                changed.push(message.id);
            }
        }
        for id in changed {
            if self.store.messaging().mark_sending(id).unwrap_or(false) {
                self.emit(Event::MessageUpdated {
                    contact_id,
                    message_id: id,
                    state: MessageState::Sending,
                });
            }
        }
    }

    fn mark_waiting(&mut self, contact_id: i64) {
        if let Ok(ids) = self.store.messaging().mark_waiting(contact_id) {
            for message_id in ids {
                self.emit(Event::MessageUpdated {
                    contact_id,
                    message_id,
                    state: MessageState::Waiting,
                });
            }
        }
    }

    fn attempt_failed(&mut self, contact_id: i64) {
        self.mark_waiting(contact_id);
        let failures = self.backoff.get(&contact_id).map_or(0, |b| b.failures) + 1;
        let delays = &self.config.retry_delays;
        let delay = delays
            .get((failures as usize).saturating_sub(1))
            .or(delays.last())
            .copied()
            .unwrap_or(Duration::from_secs(60));
        self.backoff.insert(
            contact_id,
            Backoff {
                failures,
                next_at: Instant::now() + delay,
            },
        );
    }

    fn retry_outbox(&mut self, ignore_backoff: bool) {
        if !self.online() {
            return;
        }
        let Ok(contacts) = self.store.messaging().contacts_with_outbox() else {
            return;
        };
        let now = Instant::now();
        for contact_id in contacts {
            let due = ignore_backoff
                || self
                    .backoff
                    .get(&contact_id)
                    .is_none_or(|b| b.next_at <= now);
            if due {
                self.dial(contact_id);
            }
        }
    }

    fn incoming(&mut self, contact_id: i64, id: &str, sent_at: i64, text: &str) -> bool {
        let repo = self.store.messaging();
        let Ok(Some(contact)) = repo.contact(contact_id) else {
            return false;
        };
        if contact.blocked {
            return false;
        }
        let (burst, refill) = (self.config.rate_burst, self.config.rate_refill);
        if !self
            .rates
            .entry(contact_id)
            .or_insert_with(|| RateLimit::new(burst, refill))
            .take()
        {
            return false;
        }
        match repo.insert_incoming(contact_id, id, text, sent_at, now_ms()) {
            Ok(Some(message_id)) => {
                let _ = repo.touch_contact(contact_id, now_ms());
                self.emit(Event::MessageReceived {
                    contact_id,
                    message_id,
                    contact_name: contact.shown_name().to_string(),
                });
                true
            }
            // A duplicate: acknowledge again so the sender stops retrying.
            Ok(None) => true,
            Err(_) => false,
        }
    }

    fn acked(&mut self, contact_id: i64, id: &str) {
        if let Some(session) = self.sessions.get_mut(&contact_id) {
            session.inflight.remove(id);
        }
        if let Ok(Some(message_id)) =
            self.store
                .messaging()
                .mark_delivered(contact_id, id, now_ms())
        {
            self.emit(Event::MessageUpdated {
                contact_id,
                message_id,
                state: MessageState::Delivered,
            });
        }
    }

    fn close_session(&mut self, contact_id: i64) {
        if let Some(session) = self.sessions.remove(&contact_id) {
            let _ = session.tx.try_send(SessionCmd::Close);
        }
    }

    fn tick(&mut self) {
        if !self.enabled {
            return;
        }
        if self.net == NetworkState::Offline
            && self.next_start_at.is_some_and(|at| at <= Instant::now())
        {
            self.start_network();
            return;
        }
        if let Ok(invites) = self.store.messaging().invites() {
            let now = now_ms();
            for invite in invites.into_iter().filter(|i| i.expires_at_ms <= now) {
                self.retire_invite(&invite.id);
            }
        }
        self.retry_outbox(false);
    }
}

impl From<MessagingMessageRecord> for MessageView {
    fn from(m: MessagingMessageRecord) -> Self {
        Self {
            id: m.id,
            contact_id: m.contact_id,
            outgoing: m.direction == MessagingDirection::Outgoing,
            text: m.text,
            sent_at_ms: m.sent_at_ms,
            received_at_ms: m.received_at_ms,
            state: match m.state {
                MessagingMessageState::Sending => MessageState::Sending,
                MessagingMessageState::Waiting => MessageState::Waiting,
                MessagingMessageState::Delivered => MessageState::Delivered,
                MessagingMessageState::Received => MessageState::Received,
            },
            unread: m.unread,
        }
    }
}
