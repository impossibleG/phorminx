//! The network boundary. Production uses Tor onion services (`tor` feature);
//! tests use [`MemoryTransport`], which keeps the same reachability rules.
use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{mpsc, watch};

use crate::identity::{SecretBytes, random_bytes, sha256};
use crate::invite::OnionAddress;

pub trait AsyncStream: AsyncRead + AsyncWrite + Send + Unpin {}
impl<T: AsyncRead + AsyncWrite + Send + Unpin> AsyncStream for T {}
pub type BoxStream = Box<dyn AsyncStream>;

/// Network state as shown to the user.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NetStatus {
    Stopped,
    Starting,
    Online,
    Offline,
}

#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    #[error("the Tor network is not running")]
    NotRunning,
    #[error("could not reach the contact")]
    Unreachable,
    #[error("the Tor network could not start")]
    Bootstrap,
    #[error("onion service error")]
    Service,
    #[error("key storage error")]
    Keys,
}

/// A published onion service. Dropping it stops publishing.
pub struct ServiceHandle {
    pub address: OnionAddress,
    pub incoming: mpsc::Receiver<BoxStream>,
    pub(crate) _guard: Box<dyn Send + Sync>,
}

/// A restricted-discovery client keypair. The service authorizes `public`
/// and the contact who receives `secret` can find the service.
pub struct DiscoveryKeypair {
    pub public: [u8; 32],
    pub secret: SecretBytes,
}

pub trait Transport: Send + Sync + 'static {
    /// Connects to the network. Resolves once onion services can be used.
    fn start(&self) -> impl Future<Output = Result<(), TransportError>> + Send;
    /// Leaves the network and drops all services.
    fn stop(&self);
    fn status(&self) -> watch::Receiver<NetStatus>;
    /// The address of a named service, creating its key if needed, without
    /// publishing anything.
    fn service_address(&self, nickname: &str) -> Result<OnionAddress, TransportError>;
    /// Publishes a named service. With `authorized`, only holders of one of
    /// those discovery keys can find it (restricted discovery).
    fn launch(
        &self,
        nickname: &str,
        authorized: Option<Vec<[u8; 32]>>,
    ) -> Result<ServiceHandle, TransportError>;
    /// Replaces the authorized discovery keys of a running service.
    fn reauthorize(&self, nickname: &str, authorized: Vec<[u8; 32]>) -> Result<(), TransportError>;
    /// Deletes a service's keys so its address is gone for good.
    fn destroy(&self, nickname: &str) -> Result<(), TransportError>;
    fn supports_restricted_discovery(&self) -> bool;
    fn new_discovery_keypair(&self) -> Result<DiscoveryKeypair, TransportError>;
    /// Stores the secret that lets us find a contact's restricted service.
    fn add_discovery_secret(
        &self,
        address: &OnionAddress,
        secret: &[u8],
    ) -> Result<(), TransportError>;
    fn connect(
        &self,
        address: &OnionAddress,
    ) -> impl Future<Output = Result<BoxStream, TransportError>> + Send;
}

impl<T: Transport> Transport for Arc<T> {
    fn start(&self) -> impl Future<Output = Result<(), TransportError>> + Send {
        (**self).start()
    }
    fn stop(&self) {
        (**self).stop()
    }
    fn status(&self) -> watch::Receiver<NetStatus> {
        (**self).status()
    }
    fn service_address(&self, nickname: &str) -> Result<OnionAddress, TransportError> {
        (**self).service_address(nickname)
    }
    fn launch(
        &self,
        nickname: &str,
        authorized: Option<Vec<[u8; 32]>>,
    ) -> Result<ServiceHandle, TransportError> {
        (**self).launch(nickname, authorized)
    }
    fn reauthorize(&self, nickname: &str, authorized: Vec<[u8; 32]>) -> Result<(), TransportError> {
        (**self).reauthorize(nickname, authorized)
    }
    fn destroy(&self, nickname: &str) -> Result<(), TransportError> {
        (**self).destroy(nickname)
    }
    fn supports_restricted_discovery(&self) -> bool {
        (**self).supports_restricted_discovery()
    }
    fn new_discovery_keypair(&self) -> Result<DiscoveryKeypair, TransportError> {
        (**self).new_discovery_keypair()
    }
    fn add_discovery_secret(
        &self,
        address: &OnionAddress,
        secret: &[u8],
    ) -> Result<(), TransportError> {
        (**self).add_discovery_secret(address, secret)
    }
    fn connect(
        &self,
        address: &OnionAddress,
    ) -> impl Future<Output = Result<BoxStream, TransportError>> + Send {
        (**self).connect(address)
    }
}

/// A shared in-process "network" for [`MemoryTransport`] instances.
#[derive(Default)]
pub struct MemoryNetwork {
    services: Mutex<HashMap<OnionAddress, MemoryService>>,
    next_generation: AtomicU64,
}

struct MemoryService {
    nickname: String,
    generation: u64,
    owner_online: Arc<AtomicBool>,
    authorized: Option<HashSet<[u8; 32]>>,
    incoming: mpsc::Sender<BoxStream>,
}

impl MemoryNetwork {
    pub fn new() -> Arc<Self> {
        Arc::default()
    }

    /// How many services are currently published.
    pub fn published_count(&self) -> usize {
        self.services.lock().unwrap().len()
    }

    /// Whether a service is currently published at `address`.
    pub fn is_published(&self, address: &OnionAddress) -> bool {
        self.services.lock().unwrap().contains_key(address)
    }
}

/// A deterministic test transport: the same `seed` reproduces the same
/// service addresses, like Arti's persistent keystore.
pub struct MemoryTransport {
    network: Arc<MemoryNetwork>,
    seed: [u8; 16],
    online: Arc<AtomicBool>,
    status: watch::Sender<NetStatus>,
    discovery_secrets: Mutex<HashMap<OnionAddress, Vec<u8>>>,
    destroyed: Mutex<HashSet<String>>,
    restricted: bool,
}

impl MemoryTransport {
    pub fn new(network: Arc<MemoryNetwork>, seed: [u8; 16]) -> Self {
        Self {
            network,
            seed,
            online: Arc::new(AtomicBool::new(false)),
            status: watch::channel(NetStatus::Stopped).0,
            discovery_secrets: Mutex::default(),
            destroyed: Mutex::default(),
            restricted: true,
        }
    }

    /// A transport without restricted discovery (services are open).
    pub fn without_restricted_discovery(mut self) -> Self {
        self.restricted = false;
        self
    }

    /// Simulates losing or regaining connectivity.
    pub fn set_online(&self, online: bool) {
        self.online.store(online, Ordering::SeqCst);
        let _ = self.status.send(if online {
            NetStatus::Online
        } else {
            NetStatus::Offline
        });
    }

    /// Nicknames whose keys were destroyed (for tests).
    pub fn destroyed(&self) -> Vec<String> {
        let mut all: Vec<_> = self.destroyed.lock().unwrap().iter().cloned().collect();
        all.sort();
        all
    }

    fn discovery_public(secret: &[u8]) -> [u8; 32] {
        let mut input = b"memory-discovery-public".to_vec();
        input.extend_from_slice(secret);
        sha256(&input)
    }
}

impl Transport for MemoryTransport {
    async fn start(&self) -> Result<(), TransportError> {
        self.set_online(true);
        Ok(())
    }

    fn stop(&self) {
        self.online.store(false, Ordering::SeqCst);
        let _ = self.status.send(NetStatus::Stopped);
    }

    fn status(&self) -> watch::Receiver<NetStatus> {
        self.status.subscribe()
    }

    fn service_address(&self, nickname: &str) -> Result<OnionAddress, TransportError> {
        let mut input = self.seed.to_vec();
        input.extend_from_slice(nickname.as_bytes());
        let digest = sha256(&input);
        let mut raw = [0u8; 35];
        raw[..32].copy_from_slice(&digest);
        raw[34] = 3;
        OnionAddress::from_raw(&raw).ok_or(TransportError::Keys)
    }

    fn launch(
        &self,
        nickname: &str,
        authorized: Option<Vec<[u8; 32]>>,
    ) -> Result<ServiceHandle, TransportError> {
        if !self.online.load(Ordering::SeqCst) {
            return Err(TransportError::NotRunning);
        }
        let address = self.service_address(nickname)?;
        let (tx, rx) = mpsc::channel(16);
        let generation = self.network.next_generation.fetch_add(1, Ordering::SeqCst);
        self.network.services.lock().unwrap().insert(
            address.clone(),
            MemoryService {
                nickname: nickname.to_string(),
                generation,
                owner_online: self.online.clone(),
                authorized: authorized
                    .filter(|_| self.restricted)
                    .map(|keys| keys.into_iter().collect()),
                incoming: tx,
            },
        );
        struct Guard {
            network: Arc<MemoryNetwork>,
            address: OnionAddress,
            generation: u64,
        }
        impl Drop for Guard {
            fn drop(&mut self) {
                let mut services = self.network.services.lock().unwrap();
                if services
                    .get(&self.address)
                    .is_some_and(|s| s.generation == self.generation)
                {
                    services.remove(&self.address);
                }
            }
        }
        Ok(ServiceHandle {
            address: address.clone(),
            incoming: rx,
            _guard: Box::new(Guard {
                network: self.network.clone(),
                address,
                generation,
            }),
        })
    }

    fn reauthorize(&self, nickname: &str, authorized: Vec<[u8; 32]>) -> Result<(), TransportError> {
        let address = self.service_address(nickname)?;
        let mut services = self.network.services.lock().unwrap();
        let service = services
            .get_mut(&address)
            .filter(|s| s.nickname == nickname)
            .ok_or(TransportError::Service)?;
        if self.restricted {
            service.authorized = Some(authorized.into_iter().collect());
        }
        Ok(())
    }

    fn destroy(&self, nickname: &str) -> Result<(), TransportError> {
        self.destroyed.lock().unwrap().insert(nickname.to_string());
        Ok(())
    }

    fn supports_restricted_discovery(&self) -> bool {
        self.restricted
    }

    fn new_discovery_keypair(&self) -> Result<DiscoveryKeypair, TransportError> {
        let secret = random_bytes::<32>().map_err(|_| TransportError::Keys)?;
        Ok(DiscoveryKeypair {
            public: Self::discovery_public(&secret),
            secret: SecretBytes::new(secret.to_vec()),
        })
    }

    fn add_discovery_secret(
        &self,
        address: &OnionAddress,
        secret: &[u8],
    ) -> Result<(), TransportError> {
        self.discovery_secrets
            .lock()
            .unwrap()
            .insert(address.clone(), secret.to_vec());
        Ok(())
    }

    async fn connect(&self, address: &OnionAddress) -> Result<BoxStream, TransportError> {
        if !self.online.load(Ordering::SeqCst) {
            return Err(TransportError::NotRunning);
        }
        let incoming = {
            let services = self.network.services.lock().unwrap();
            let service = services.get(address).ok_or(TransportError::Unreachable)?;
            if !service.owner_online.load(Ordering::SeqCst) {
                return Err(TransportError::Unreachable);
            }
            if let Some(authorized) = &service.authorized {
                let secrets = self.discovery_secrets.lock().unwrap();
                let allowed = secrets
                    .get(address)
                    .is_some_and(|s| authorized.contains(&Self::discovery_public(s)));
                if !allowed {
                    // Without a valid discovery key the descriptor cannot be
                    // decrypted, so the service looks absent.
                    return Err(TransportError::Unreachable);
                }
            }
            service.incoming.clone()
        };
        let (client, server) = tokio::io::duplex(64 * 1024);
        incoming
            .send(Box::new(server))
            .await
            .map_err(|_| TransportError::Unreachable)?;
        Ok(Box::new(client))
    }
}
