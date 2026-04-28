//! Relay: the wired-up store + identity + transport + engine.
//!
//! `Relay::new(config)` does all the setup synchronously (in async fn form).
//! The returned `RelayHandle` exposes the relay's address + a `run` method
//! that drives the engine until shutdown.

use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use zeroize::Zeroizing;

use sunset_core::Identity;
use sunset_noise::{NoiseIdentity, NoiseTransport, ed25519_seed_to_x25519_secret};
use sunset_store::{Filter, VerifyingKey};
use sunset_store_fs::FsStore;
use sunset_sync::{PeerAddr, PeerId, Signer, SyncConfig, SyncEngine};
use sunset_sync_ws_native::WebSocketRawTransport;

use crate::config::{Config, InterestFilter};
use crate::error::Result;
use crate::identity;

type Engine = SyncEngine<FsStore, NoiseTransport<WebSocketRawTransport>>;

pub struct Relay {/* sealed; see RelayHandle */}

pub struct RelayHandle {
    pub local_address: String,
    pub ed25519_public: [u8; 32],
    pub x25519_public: [u8; 32],

    engine: Rc<Engine>,
    peers: Vec<String>,
    subscription_filter: Filter,
}

/// Adapter so sunset-core's `Identity` can be used as a `NoiseIdentity`.
struct IdentityNoiseAdapter(Identity);

impl NoiseIdentity for IdentityNoiseAdapter {
    fn ed25519_public(&self) -> [u8; 32] {
        self.0.public().as_bytes()
    }
    fn ed25519_secret_seed(&self) -> Zeroizing<[u8; 32]> {
        Zeroizing::new(self.0.secret_bytes())
    }
}

impl Relay {
    /// Open store, load identity, bind listener, build engine. Returns a
    /// handle ready for `run()`.
    #[allow(clippy::new_ret_no_self)]
    pub async fn new(config: Config) -> Result<RelayHandle> {
        // 1. Identity (load-or-generate; persists to disk on first start).
        tokio::fs::create_dir_all(&config.data_dir).await?;
        let identity = identity::load_or_generate(&config.identity_secret_path).await?;

        let ed25519_public = identity.public().as_bytes();
        let x25519_public = {
            let s = ed25519_seed_to_x25519_secret(&identity.secret_bytes());
            use curve25519_dalek::{MontgomeryPoint, scalar::Scalar};
            let scalar = Scalar::from_bytes_mod_order(*s);
            MontgomeryPoint::mul_base(&scalar).to_bytes()
        };

        // 2. Store (FsStore with Ed25519Verifier).
        let store_root = config.data_dir.join("store");
        tokio::fs::create_dir_all(&store_root).await?;
        let store = Arc::new(
            FsStore::with_verifier(&store_root, Arc::new(sunset_core::Ed25519Verifier)).await?,
        );

        // 3. Listener + Noise wrapper.
        let raw = WebSocketRawTransport::listening_on(config.listen_addr).await?;
        // Resolve the actual bound address (matters when config requested
        // port 0 for an OS-assigned random port — the banner must reflect
        // what clients should connect to, not the unbound config value).
        let bound = raw.local_addr().unwrap_or(config.listen_addr);
        let banner = identity::format_address(&bound, &identity);
        tracing::info!("\n{}", banner);
        println!("{}", banner);
        let local_address = format!("ws://{}#x25519={}", bound, hex::encode(x25519_public));
        let noise = NoiseTransport::new(raw, Arc::new(IdentityNoiseAdapter(identity.clone())));

        // 4. SyncEngine.
        let local_peer = PeerId(VerifyingKey::new(Bytes::copy_from_slice(&ed25519_public)));
        let signer: Arc<dyn Signer> = Arc::new(identity.clone());
        let engine = Rc::new(SyncEngine::new(
            store,
            noise,
            SyncConfig::default(),
            local_peer,
            signer,
        ));

        // 5. Subscription filter.
        let subscription_filter = match config.interest_filter {
            InterestFilter::All => Filter::NamePrefix(Bytes::new()),
        };

        Ok(RelayHandle {
            local_address,
            ed25519_public,
            x25519_public,
            engine,
            peers: config.peers,
            subscription_filter,
        })
    }
}

impl RelayHandle {
    pub fn dial_address(&self) -> String {
        self.local_address.clone()
    }

    /// Drive the engine, dial federated peers, then run until shutdown.
    pub async fn run(self) -> Result<()> {
        let engine_clone = self.engine.clone();
        let engine_task = tokio::task::spawn_local(async move { engine_clone.run().await });

        self.engine
            .publish_subscription(self.subscription_filter.clone(), Duration::from_secs(3600))
            .await?;
        tracing::info!("published broad subscription");

        for peer_url in &self.peers {
            let addr = PeerAddr::new(Bytes::from(peer_url.clone()));
            match self.engine.add_peer(addr).await {
                Ok(()) => tracing::info!(peer = %peer_url, "federated peer dialed"),
                Err(e) => {
                    tracing::warn!(peer = %peer_url, error = %e, "federated peer dial failed (continuing)")
                }
            }
        }

        #[cfg(unix)]
        {
            let mut sigterm =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {
                    tracing::info!("received SIGINT, shutting down");
                }
                _ = sigterm.recv() => {
                    tracing::info!("received SIGTERM, shutting down");
                }
            }
        }
        #[cfg(not(unix))]
        {
            tokio::signal::ctrl_c().await?;
            tracing::info!("received Ctrl+C, shutting down");
        }

        engine_task.abort();
        Ok(())
    }

    /// For tests: drive the engine without waiting for OS signals. Returns
    /// the engine task handle so the caller can abort it during teardown.
    #[cfg(any(test, feature = "test-helpers"))]
    pub async fn run_for_test(&self) -> Result<tokio::task::JoinHandle<sunset_sync::Result<()>>> {
        let engine_clone = self.engine.clone();
        let engine_task = tokio::task::spawn_local(async move { engine_clone.run().await });

        self.engine
            .publish_subscription(self.subscription_filter.clone(), Duration::from_secs(3600))
            .await?;

        for peer_url in &self.peers {
            let addr = PeerAddr::new(Bytes::from(peer_url.clone()));
            if let Err(e) = self.engine.add_peer(addr).await {
                tracing::warn!(peer = %peer_url, error = %e, "federated peer dial failed (test)");
            }
        }

        Ok(engine_task)
    }

    /// For tests: access the underlying engine.
    #[cfg(any(test, feature = "test-helpers"))]
    pub fn engine(&self) -> &Rc<Engine> {
        &self.engine
    }
}
