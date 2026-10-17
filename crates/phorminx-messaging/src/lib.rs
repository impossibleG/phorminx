//! Peer-to-peer text messaging over Tor onion services.
//!
//! Two people add each other by sharing a one-time invite string. Messages go
//! directly between their onion services: no server, mailbox or relay, and
//! neither side learns the other's IP address. Nothing touches the network
//! until the user enables messaging.
//!
//! [`Messenger`] runs the engine on a background thread with its own tokio
//! runtime and exposes a small synchronous API plus an event callback.

mod base32;
mod engine;
mod identity;
mod invite;
mod protocol;
mod session;
#[cfg(feature = "tor")]
mod tor;
mod transport;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;
use tokio::sync::mpsc;

pub use identity::{Identity, PublicKey, SecretBytes};
pub use invite::{Invite, OnionAddress};
pub use protocol::{
    Frame, MAX_FRAME_BYTES, MAX_NAME_CHARS, MAX_TEXT_BYTES, PROTOCOL_VERSION, ProtocolError,
    read_frame, write_frame,
};
#[cfg(feature = "tor")]
pub use tor::TorTransport;
pub use transport::{
    BoxStream, DiscoveryKeypair, MemoryNetwork, MemoryTransport, NetStatus, ServiceHandle,
    Transport, TransportError,
};

use engine::{Engine, Internal, Request};

#[derive(Debug, thiserror::Error)]
pub enum MessagingError {
    #[error("messaging is not running")]
    Stopped,
    #[error("messaging is not online yet")]
    NotOnline,
    #[error("the Tor network is unavailable")]
    Network,
    #[error("the messaging identity could not be created or unlocked")]
    Identity,
    #[error("local message storage failed")]
    Storage,
    #[error("names must be 1-64 printable characters")]
    InvalidName,
    #[error("messages must be non-empty and at most 4 KB")]
    InvalidMessage,
    #[error("that invite is not valid; copy the whole text")]
    InvalidInvite,
    #[error("that invite needs a newer version of Phorminx")]
    UnsupportedInvite,
    #[error("that invite has expired; ask for a new one")]
    InviteExpired,
    #[error("that invite is your own")]
    OwnInvite,
    #[error("could not reach the person who made that invite; they must be online")]
    InviteUnreachable,
    #[error("that invite was already used or is no longer valid")]
    InviteRejected,
    #[error("unknown contact")]
    UnknownContact,
    #[error("that contact is blocked")]
    Blocked,
    #[error("invalid key slot")]
    InvalidSlot,
    #[error("too many open invites; wait for one to be used or expire")]
    TooManyInvites,
}

/// Protects the identity key at rest (DPAPI in the app).
pub trait KeyVault: Send + Sync + 'static {
    fn protect(&self, secret: &[u8]) -> Result<Vec<u8>, MessagingError>;
    fn expose(&self, protected: &[u8]) -> Result<SecretBytes, MessagingError>;
}

/// Timing and limits. `Default` is the production configuration.
#[derive(Clone, Debug)]
pub struct Config {
    pub invite_ttl: Duration,
    /// Delays between delivery attempts after consecutive failures; the last
    /// one repeats.
    pub retry_delays: Vec<Duration>,
    pub tick: Duration,
    pub handshake_timeout: Duration,
    pub connect_timeout: Duration,
    pub idle_timeout: Duration,
    pub restart_delay: Duration,
    /// Messages a contact may send in a burst, refilled one per `rate_refill`.
    pub rate_burst: u32,
    pub rate_refill: Duration,
    /// How many recently seen contacts to dial after coming online, so they
    /// deliver what they queued for us right away.
    pub announce_limit: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            invite_ttl: Duration::from_secs(24 * 60 * 60),
            retry_delays: [30, 60, 120, 300, 600, 900]
                .into_iter()
                .map(Duration::from_secs)
                .collect(),
            tick: Duration::from_secs(5),
            handshake_timeout: Duration::from_secs(90),
            connect_timeout: Duration::from_secs(150),
            idle_timeout: Duration::from_secs(10 * 60),
            restart_delay: Duration::from_secs(60),
            rate_burst: 30,
            rate_refill: Duration::from_secs(2),
            announce_limit: 20,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum NetworkState {
    Disabled,
    Starting,
    Online,
    Offline,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageState {
    Sending,
    Waiting,
    Delivered,
    Received,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusView {
    pub enabled: bool,
    pub has_identity: bool,
    pub display_name: Option<String>,
    pub network: NetworkState,
}

/// A contact as the UI sees it: by name only, never an address or key.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContactView {
    pub id: i64,
    pub name: String,
    pub display_name: String,
    pub nickname: Option<String>,
    pub contacts_slot: Option<u8>,
    pub main_slot: Option<u8>,
    pub blocked: bool,
    pub unread: u32,
    pub connected: bool,
    pub last_seen_ms: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageView {
    pub id: i64,
    pub contact_id: i64,
    pub outgoing: bool,
    pub text: String,
    pub sent_at_ms: i64,
    pub received_at_ms: Option<i64>,
    pub state: MessageState,
    pub unread: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    Status(NetworkState),
    ContactsChanged,
    ContactAdded {
        contact_id: i64,
    },
    MessageReceived {
        contact_id: i64,
        message_id: i64,
        contact_name: String,
    },
    MessageUpdated {
        contact_id: i64,
        message_id: i64,
        state: MessageState,
    },
    ConversationChanged {
        contact_id: i64,
    },
    Presence {
        contact_id: i64,
        connected: bool,
    },
}

pub type EventSink = Arc<dyn Fn(Event) + Send + Sync>;

/// Handle to the messaging engine thread. Dropping it shuts the engine down.
pub struct Messenger {
    actor: mpsc::UnboundedSender<Internal>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Messenger {
    /// Starts the engine. If messaging was enabled last time, it reconnects to
    /// Tor and retries the outbox; otherwise nothing touches the network.
    pub fn start<T: Transport>(
        transport: T,
        database: &Path,
        vault: Arc<dyn KeyVault>,
        config: Config,
        events: impl Fn(Event) + Send + Sync + 'static,
    ) -> Result<Self, MessagingError> {
        let store = phorminx_persistence::Persistence::open(database)
            .map_err(|_| MessagingError::Storage)?;
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("phorminx-messaging")
            .enable_all()
            .build()
            .map_err(|_| MessagingError::Stopped)?;
        let (actor, inbox) = mpsc::unbounded_channel();
        let engine_actor = actor.clone();
        let events: EventSink = Arc::new(events);
        let thread = std::thread::Builder::new()
            .name("phorminx-messaging".into())
            .spawn(move || {
                runtime.block_on(async move {
                    let engine = Engine::new(
                        Arc::new(transport),
                        store,
                        vault,
                        Arc::new(config),
                        events,
                        engine_actor,
                    );
                    engine.run(inbox).await;
                });
                runtime.shutdown_timeout(Duration::from_secs(2));
            })
            .map_err(|_| MessagingError::Stopped)?;
        Ok(Self {
            actor,
            thread: Some(thread),
        })
    }

    fn call<R>(&self, make: impl FnOnce(engine::Reply<R>) -> Request) -> Result<R, MessagingError> {
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        self.actor
            .send(Internal::Request(make(tx)))
            .map_err(|_| MessagingError::Stopped)?;
        rx.recv().map_err(|_| MessagingError::Stopped)?
    }

    pub fn status(&self) -> Result<StatusView, MessagingError> {
        self.call(Request::Status)
    }

    /// Enables messaging, creating the identity on first use (a display name
    /// is required then).
    pub fn enable(&self, display_name: Option<&str>) -> Result<(), MessagingError> {
        let display_name = display_name.map(str::to_string);
        self.call(|reply| Request::Enable {
            display_name,
            reply,
        })
    }

    /// Leaves Tor and stops all services. Contacts and history are kept.
    pub fn disable(&self) -> Result<(), MessagingError> {
        self.call(Request::Disable)
    }

    pub fn set_display_name(&self, display_name: &str) -> Result<(), MessagingError> {
        let display_name = display_name.to_string();
        self.call(|reply| Request::SetDisplayName {
            display_name,
            reply,
        })
    }

    /// Publishes a fresh single-use invite service and returns `name#code`.
    pub fn create_invite(&self) -> Result<String, MessagingError> {
        self.call(Request::CreateInvite)
    }

    /// Redeems an invite. Blocks until the exchange finishes, which takes
    /// seconds over Tor; call it off the UI thread.
    pub fn redeem_invite(&self, invite: &str) -> Result<ContactView, MessagingError> {
        let invite = invite.to_string();
        self.call(|reply| Request::RedeemInvite { invite, reply })
    }

    pub fn contacts(&self) -> Result<Vec<ContactView>, MessagingError> {
        self.call(Request::Contacts)
    }

    pub fn conversation(
        &self,
        contact_id: i64,
        limit: usize,
    ) -> Result<Vec<MessageView>, MessagingError> {
        self.call(|reply| Request::Conversation {
            contact_id,
            limit,
            reply,
        })
    }

    /// Queues a message. It is delivered now if the contact is reachable,
    /// otherwise retried on a backoff and whenever the contact comes online.
    pub fn send(&self, contact_id: i64, text: &str) -> Result<MessageView, MessagingError> {
        let text = text.to_string();
        self.call(|reply| Request::Send {
            contact_id,
            text,
            reply,
        })
    }

    pub fn mark_read(&self, contact_id: i64) -> Result<(), MessagingError> {
        self.call(|reply| Request::MarkRead { contact_id, reply })
    }

    pub fn rename(&self, contact_id: i64, nickname: Option<&str>) -> Result<(), MessagingError> {
        let nickname = nickname.map(str::to_string);
        self.call(|reply| Request::Rename {
            contact_id,
            nickname,
            reply,
        })
    }

    pub fn set_blocked(&self, contact_id: i64, blocked: bool) -> Result<(), MessagingError> {
        self.call(|reply| Request::SetBlocked {
            contact_id,
            blocked,
            reply,
        })
    }

    /// Removes a contact and its history.
    pub fn remove(&self, contact_id: i64) -> Result<(), MessagingError> {
        self.call(|reply| Request::Remove { contact_id, reply })
    }

    pub fn set_contacts_slot(
        &self,
        contact_id: i64,
        slot: Option<u8>,
    ) -> Result<(), MessagingError> {
        self.call(|reply| Request::SetContactsSlot {
            contact_id,
            slot,
            reply,
        })
    }

    pub fn set_main_slot(&self, contact_id: i64, slot: Option<u8>) -> Result<(), MessagingError> {
        self.call(|reply| Request::SetMainSlot {
            contact_id,
            slot,
            reply,
        })
    }

    pub fn delete_history(&self, contact_id: i64) -> Result<(), MessagingError> {
        self.call(|reply| Request::DeleteHistory { contact_id, reply })
    }
}

impl Drop for Messenger {
    fn drop(&mut self) {
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        if self
            .actor
            .send(Internal::Request(Request::Shutdown(tx)))
            .is_ok()
        {
            let _ = rx.recv_timeout(Duration::from_secs(5));
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
