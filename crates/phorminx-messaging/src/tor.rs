//! Tor transport: Arti embedded in-process, onion services for listening and
//! onion connections for dialing. State and cache live in a `tor` folder
//! under the app data directory.
use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::str::FromStr;
use std::sync::{Arc, Mutex, Weak};

use arti_client::config::{CfgPath, Reconfigure};
use arti_client::{
    DormantMode, HsClientDescEncKey, HsId, KeystoreSelector, TorClient, TorClientConfig,
};
use futures::StreamExt;
use safelog::DisplayRedacted;
use tokio::sync::{mpsc, watch};
use tor_cell::relaycell::msg::Connected;
use tor_hscrypto::pk::HsClientDescEncSecretKey;
use tor_hsservice::config::OnionServiceConfigBuilder;
use tor_hsservice::config::restricted_discovery::HsClientNickname;
use tor_hsservice::{HsNickname, RunningOnionService, handle_rend_requests};
use tor_llcrypto::pk::curve25519;
use tor_rtcompat::PreferredRuntime;

use crate::identity::{SecretBytes, random_bytes};
use crate::invite::OnionAddress;
use crate::transport::{
    BoxStream, DiscoveryKeypair, NetStatus, ServiceHandle, Transport, TransportError,
};

/// The virtual port our services answer on. Every connection still has to
/// pass the handshake, so the number only needs to match on both ends.
const PORT: u16 = 80;

struct RunningService {
    running: Weak<RunningOnionService>,
    nickname: HsNickname,
}

pub struct TorTransport {
    root: PathBuf,
    client: Mutex<Option<Arc<TorClient<PreferredRuntime>>>>,
    services: Mutex<HashMap<String, RunningService>>,
    pending_destroy: Mutex<Vec<String>>,
    /// Resolves once a stopped client has released Arti's state lock.
    stopping: Mutex<Option<Pin<Box<dyn Future<Output = ()> + Send + Sync>>>>,
    status: watch::Sender<NetStatus>,
}

impl TorTransport {
    /// `root` is the `tor` folder; Arti's state and cache go beneath it.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            client: Mutex::new(None),
            services: Mutex::default(),
            pending_destroy: Mutex::default(),
            stopping: Mutex::default(),
            status: watch::channel(NetStatus::Stopped).0,
        }
    }

    fn state_dir(&self) -> PathBuf {
        self.root.join("state")
    }

    fn config(&self) -> Result<TorClientConfig, TransportError> {
        let mut builder = TorClientConfig::builder();
        builder
            .storage()
            .state_dir(CfgPath::new_literal(self.state_dir()))
            .cache_dir(CfgPath::new_literal(self.root.join("cache")));
        builder.build().map_err(|_| TransportError::Bootstrap)
    }

    fn client(&self) -> Result<Arc<TorClient<PreferredRuntime>>, TransportError> {
        self.client
            .lock()
            .unwrap()
            .clone()
            .ok_or(TransportError::NotRunning)
    }

    fn nickname(nickname: &str) -> Result<HsNickname, TransportError> {
        HsNickname::new(nickname.to_string()).map_err(|_| TransportError::Service)
    }

    fn service_config(
        nickname: &str,
        authorized: Option<&[[u8; 32]]>,
    ) -> Result<tor_hsservice::OnionServiceConfig, TransportError> {
        let mut builder = OnionServiceConfigBuilder::default();
        builder.nickname(Self::nickname(nickname)?);
        if let Some(keys) = authorized {
            let restricted = builder.restricted_discovery();
            restricted.enabled(true);
            for (index, key) in keys.iter().enumerate() {
                let client = HsClientNickname::from_str(&format!("contact{index}"))
                    .map_err(|_| TransportError::Service)?;
                restricted.static_keys().access().push((
                    client,
                    HsClientDescEncKey::from(curve25519::PublicKey::from(*key)),
                ));
            }
        }
        builder.build().map_err(|_| TransportError::Service)
    }

    fn hsid(address: &OnionAddress) -> Result<HsId, TransportError> {
        HsId::from_str(&address.as_onion()).map_err(|_| TransportError::Unreachable)
    }

    fn address_of(hsid: HsId) -> Result<OnionAddress, TransportError> {
        OnionAddress::parse(&hsid.display_unredacted().to_string()).ok_or(TransportError::Keys)
    }

    /// Deletes a stopped service's keys and state. Arti may still hold the
    /// files briefly after the service is dropped, so failures are retried.
    fn remove_service_files(&self, nickname: &str) -> bool {
        let state = self.state_dir();
        let paths = [
            state.join("keystore").join("hss").join(nickname),
            state.join("hss").join(nickname),
        ];
        let mut done = true;
        for path in paths {
            if path.exists() && std::fs::remove_dir_all(&path).is_err() {
                done = false;
            }
        }
        let lock = state.join("hss").join(format!("{nickname}.lock"));
        if lock.exists() && std::fs::remove_file(&lock).is_err() {
            done = false;
        }
        done
    }

    fn retry_pending_destroy(&self) {
        let pending = std::mem::take(&mut *self.pending_destroy.lock().unwrap());
        let running = self.services.lock().unwrap();
        let still: Vec<String> = pending
            .into_iter()
            .filter(|n| running.contains_key(n) || !self.remove_service_files(n))
            .collect();
        drop(running);
        self.pending_destroy.lock().unwrap().extend(still);
    }
}

impl Transport for TorTransport {
    async fn start(&self) -> Result<(), TransportError> {
        // Several crates in the app enable rustls; pick one provider for the process.
        let _ = rustls::crypto::ring::default_provider().install_default();
        if self.client.lock().unwrap().is_some() {
            let _ = self.status.send(NetStatus::Online);
            return Ok(());
        }
        let _ = self.status.send(NetStatus::Starting);
        // After "turn off", wait for the old client to let go of its state.
        let stopping = self.stopping.lock().unwrap().take();
        if let Some(stopped) = stopping {
            let _ = tokio::time::timeout(std::time::Duration::from_secs(15), stopped).await;
        }
        let config = self.config()?;
        let client = match TorClient::create_bootstrapped(config).await {
            Ok(client) => client,
            Err(_) => {
                let _ = self.status.send(NetStatus::Offline);
                return Err(TransportError::Bootstrap);
            }
        };
        let mut events = client.bootstrap_events();
        let status = self.status.clone();
        tokio::spawn(async move {
            while let Some(event) = events.next().await {
                let next = if event.ready_for_traffic() {
                    NetStatus::Online
                } else if event.blocked().is_some() {
                    NetStatus::Offline
                } else {
                    continue;
                };
                if status.send(next).is_err() {
                    break;
                }
            }
        });
        *self.client.lock().unwrap() = Some(client);
        let _ = self.status.send(NetStatus::Online);
        self.retry_pending_destroy();
        Ok(())
    }

    fn stop(&self) {
        self.services.lock().unwrap().clear();
        if let Some(client) = self.client.lock().unwrap().take() {
            client.set_dormant(DormantMode::Soft);
            *self.stopping.lock().unwrap() = Some(Box::pin(client.wait_for_stop()));
            // Dropping the last handle shuts the client down and closes its
            // connections to the Tor network.
            drop(client);
        }
        let _ = self.status.send(NetStatus::Stopped);
    }

    fn status(&self) -> watch::Receiver<NetStatus> {
        self.status.subscribe()
    }

    fn service_address(&self, nickname: &str) -> Result<OnionAddress, TransportError> {
        let service = TorClient::<PreferredRuntime>::create_onion_service(
            &self.config()?,
            Self::service_config(nickname, None)?,
        )
        .map_err(|_| TransportError::Keys)?;
        let hsid = service
            .generate_identity_key(KeystoreSelector::Primary)
            .map_err(|_| TransportError::Keys)?;
        Self::address_of(hsid)
    }

    fn launch(
        &self,
        nickname: &str,
        authorized: Option<Vec<[u8; 32]>>,
    ) -> Result<ServiceHandle, TransportError> {
        let client = self.client()?;
        let config = Self::service_config(nickname, authorized.as_deref())?;
        let (running, requests) = client
            .launch_onion_service(config)
            .map_err(|_| TransportError::Service)?
            .ok_or(TransportError::Service)?;
        let address = Self::address_of(running.onion_address().ok_or(TransportError::Keys)?)?;
        let (tx, rx) = mpsc::channel::<BoxStream>(16);
        let task = tokio::spawn(async move {
            let mut streams = handle_rend_requests(requests);
            while let Some(request) = streams.next().await {
                let tx = tx.clone();
                tokio::spawn(async move {
                    if let Ok(stream) = request.accept(Connected::new_empty()).await {
                        let _ = tx.send(Box::new(stream)).await;
                    }
                });
            }
        });
        self.services.lock().unwrap().insert(
            nickname.to_string(),
            RunningService {
                running: Arc::downgrade(&running),
                nickname: Self::nickname(nickname)?,
            },
        );
        struct Guard {
            _running: Arc<RunningOnionService>,
            task: tokio::task::JoinHandle<()>,
        }
        impl Drop for Guard {
            fn drop(&mut self) {
                self.task.abort();
            }
        }
        Ok(ServiceHandle {
            address,
            incoming: rx,
            _guard: Box::new(Guard {
                _running: running,
                task,
            }),
        })
    }

    fn reauthorize(&self, nickname: &str, authorized: Vec<[u8; 32]>) -> Result<(), TransportError> {
        let services = self.services.lock().unwrap();
        let service = services.get(nickname).ok_or(TransportError::Service)?;
        let running = service.running.upgrade().ok_or(TransportError::Service)?;
        debug_assert_eq!(service.nickname.as_ref(), nickname);
        running
            .reconfigure(
                Self::service_config(nickname, Some(&authorized))?,
                Reconfigure::WarnOnFailures,
            )
            .map_err(|_| TransportError::Service)
    }

    fn destroy(&self, nickname: &str) -> Result<(), TransportError> {
        self.services.lock().unwrap().remove(nickname);
        if !self.remove_service_files(nickname) {
            self.pending_destroy
                .lock()
                .unwrap()
                .push(nickname.to_string());
        }
        Ok(())
    }

    fn supports_restricted_discovery(&self) -> bool {
        true
    }

    fn new_discovery_keypair(&self) -> Result<DiscoveryKeypair, TransportError> {
        let secret = random_bytes::<32>().map_err(|_| TransportError::Keys)?;
        let public = curve25519::PublicKey::from(&curve25519::StaticSecret::from(secret));
        Ok(DiscoveryKeypair {
            public: public.to_bytes(),
            secret: SecretBytes::new(secret.to_vec()),
        })
    }

    fn add_discovery_secret(
        &self,
        address: &OnionAddress,
        secret: &[u8],
    ) -> Result<(), TransportError> {
        let client = self.client()?;
        let hsid = Self::hsid(address)?;
        let bytes: [u8; 32] = secret.try_into().map_err(|_| TransportError::Keys)?;
        let key = HsClientDescEncSecretKey::from(curve25519::StaticSecret::from(bytes));
        // A second invite with the same person replaces the old key.
        let _ = client.remove_service_discovery_key(KeystoreSelector::Primary, hsid);
        client
            .insert_service_discovery_key(KeystoreSelector::Primary, hsid, key)
            .map(|_| ())
            .map_err(|_| TransportError::Keys)
    }

    async fn connect(&self, address: &OnionAddress) -> Result<BoxStream, TransportError> {
        let client = self.client()?;
        let stream = client
            .connect((address.as_onion(), PORT))
            .await
            .map_err(|_| TransportError::Unreachable)?;
        Ok(Box::new(stream))
    }
}

impl std::fmt::Debug for TorTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TorTransport").finish_non_exhaustive()
    }
}
