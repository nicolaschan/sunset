//! Relay: identity + store + engine + axum HTTP/WS host.
//!
//! `Relay::start(config)` does setup synchronously (in async fn form):
//! identity, store, engine, the SpawningAcceptor that wraps a
//! WebSocketRawTransport::serving(), the command pump, and a bound
//! TcpListener. The returned `RelayHandle` exposes the dial URL + a
//! `run`/`run_for_test` method that drives axum and the engine task
//! until shutdown.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use bytes::Bytes;
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use zeroize::Zeroizing;

use sunset_core::Identity;
use sunset_noise::{
    NoiseConnection, NoiseIdentity, NoiseTransport, do_handshake_responder,
    ed25519_seed_to_x25519_secret,
};
use sunset_store::{Filter, VerifyingKey};
use sunset_store_fs::FsStore;
use sunset_sync::{PeerAddr, PeerId, Signer, SpawningAcceptor, SyncConfig, SyncEngine};
use sunset_sync_webtransport_native::{
    WebTransportRawConnection, WebTransportRawTransport, build_server_endpoint,
    sha256_digest_to_hex,
};
use sunset_sync_ws_native::WebSocketRawTransport;

use crate::app::{AppState, build_app};
use crate::bridge::RelayCommand;
use crate::config::{Config, InterestFilter};
use crate::error::Result;
use crate::identity;
use crate::snapshot::{build_dashboard_snapshot, build_identity_snapshot};
use crate::wt_combinator::DualInboundTransport;

/// Concrete inbound-side `Transport` the engine sees. Kept private —
/// callers interact with `RelayHandle`, not this type.
type InboundTransport = DualInboundTransport<WsAcceptor, WtAcceptor>;

/// WebSocket half. Same shape as before WT was added.
type WsAcceptor = SpawningAcceptor<
    WebSocketRawTransport,
    NoiseTransport<WebSocketRawTransport>,
    WsPromote,
    WsHandshakeFuture,
    NoiseConnection<sunset_sync_ws_native::WebSocketRawConnection>,
>;

type WsPromote =
    Box<dyn Fn(sunset_sync_ws_native::WebSocketRawConnection) -> WsHandshakeFuture + 'static>;

type WsHandshakeFuture = std::pin::Pin<
    Box<
        dyn std::future::Future<
                Output = sunset_sync::Result<
                    NoiseConnection<sunset_sync_ws_native::WebSocketRawConnection>,
                >,
            > + 'static,
    >,
>;

/// WebTransport half. Mirrors the WS shape.
type WtAcceptor = SpawningAcceptor<
    WebTransportRawTransport,
    NoiseTransport<WebTransportRawTransport>,
    WtPromote,
    WtHandshakeFuture,
    NoiseConnection<WebTransportRawConnection>,
>;

type WtPromote = Box<dyn Fn(WebTransportRawConnection) -> WtHandshakeFuture + 'static>;

type WtHandshakeFuture = std::pin::Pin<
    Box<
        dyn std::future::Future<
                Output = sunset_sync::Result<NoiseConnection<WebTransportRawConnection>>,
            > + 'static,
    >,
>;

type Engine = SyncEngine<FsStore, InboundTransport>;

pub struct Relay {/* sealed; see RelayHandle */}

pub struct RelayHandle {
    pub local_address: String,
    pub ed25519_public: [u8; 32],
    pub x25519_public: [u8; 32],

    engine: Rc<Engine>,
    peers: Vec<String>,
    subscription_filter: Filter,
    listener: Option<TcpListener>,
    /// Senders the axum app uses. Built once in `new`; cloned into
    /// `AppState` in `run` / `run_for_test`.
    ws_tx: mpsc::UnboundedSender<axum::extract::ws::WebSocket>,
    cmd_tx: mpsc::UnboundedSender<RelayCommand>,
    /// Engine-side context used by the command pump (one shared Rc).
    /// Held here so the pump's Rc graph stays alive for the relay's
    /// lifetime. The field is read-only (the pump task already holds
    /// its own clone), but storing it here documents the ownership.
    #[allow(dead_code)]
    cmd_ctx: Rc<CommandContext>,
}

/// Held by the command pump task on the engine side. Captures the
/// references it needs to build snapshots without crossing runtimes.
struct CommandContext {
    engine: Rc<Engine>,
    store: Arc<FsStore>,
    data_dir: PathBuf,
    ed25519_public: [u8; 32],
    x25519_public: [u8; 32],
    listen_addr: SocketAddr,
    dial_url: String,
    /// `wt://`/`wts://` URL with `cert-sha256=…` fragment. `None` when
    /// the relay couldn't bind a UDP listener (logs the failure and
    /// degrades to WS-only without aborting).
    webtransport_address: Option<String>,
    configured_peers: Vec<String>,
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
    /// handle ready for `run()` / `run_for_test()`.
    ///
    /// **Precondition:** must be called from within a `tokio::task::LocalSet`.
    /// The constructor spawns `spawn_local` tasks (the command pump and
    /// `SpawningAcceptor`'s internal handshake pump); calling it without
    /// an active LocalSet will panic.
    pub async fn start(config: Config) -> Result<RelayHandle> {
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

        // 2. Store.
        let store_root = config.data_dir.join("store");
        tokio::fs::create_dir_all(&store_root).await?;
        let store = Arc::new(
            FsStore::with_verifier(&store_root, Arc::new(sunset_core::Ed25519Verifier)).await?,
        );

        // 3. Bind the HTTP/WS listener up front so we know the bound port.
        let listener = TcpListener::bind(config.listen_addr).await?;
        let bound = listener.local_addr().unwrap_or(config.listen_addr);
        let local_address = format!("ws://{}#x25519={}", bound, hex::encode(x25519_public));

        // 4. WebSocket inbound + outbound (existing path).
        let (ws_raw_inbound, ws_tx) = WebSocketRawTransport::serving();
        let ws_raw_outbound = WebSocketRawTransport::dial_only();
        let noise_id: Arc<dyn NoiseIdentity> = Arc::new(IdentityNoiseAdapter(identity.clone()));
        let ws_connector = NoiseTransport::new(ws_raw_outbound, noise_id.clone());

        // 5. WebSocket SpawningAcceptor — Noise IK on its own task.
        let handshake_timeout = Duration::from_secs(config.accept_handshake_timeout_secs);
        let ws_promote: WsPromote = {
            let identity = noise_id.clone();
            Box::new(move |raw_conn| {
                let identity = identity.clone();
                Box::pin(async move {
                    do_handshake_responder(raw_conn, identity)
                        .await
                        .map_err(|e| sunset_sync::Error::Transport(format!("noise responder: {e}")))
                })
            })
        };
        let ws_transport =
            SpawningAcceptor::new(ws_raw_inbound, ws_connector, ws_promote, handshake_timeout);

        // 6. WebTransport listener. `Identity::self_signed` produces an
        //    ECDSA-P256 cert with 14-day validity — exactly what the
        //    `serverCertificateHashes` API expects. Bind UDP on the same
        //    `listen_addr` (UDP and TCP have separate socket spaces, so
        //    this doesn't conflict with the WS listener). On bind
        //    failure, degrade to WS-only with a warning rather than
        //    aborting startup.
        let (wt_raw_inbound, wt_accept_tx) = WebTransportRawTransport::serving();
        let wt_raw_outbound = WebTransportRawTransport::dial_only();
        let wt_connector = NoiseTransport::new(wt_raw_outbound, noise_id.clone());
        let wt_promote: WtPromote = {
            let identity = noise_id.clone();
            Box::new(move |raw_conn| {
                let identity = identity.clone();
                Box::pin(async move {
                    do_handshake_responder(raw_conn, identity)
                        .await
                        .map_err(|e| sunset_sync::Error::Transport(format!("noise responder: {e}")))
                })
            })
        };
        let wt_transport =
            SpawningAcceptor::new(wt_raw_inbound, wt_connector, wt_promote, handshake_timeout);

        // The dial-host SAN list determines which hostnames can be used
        // to reach this WT listener. Tests dial 127.0.0.1; production
        // dials by external hostname (set via the relay config's
        // listen_addr or — eventually — a dedicated `webtransport_san`
        // setting). For now we always include `127.0.0.1` and
        // `localhost` plus the listen address's literal IP if it's not
        // already in that list.
        let mut wt_sans: Vec<String> = vec!["127.0.0.1".into(), "localhost".into()];
        let listen_ip = bound.ip().to_string();
        if !wt_sans.iter().any(|s| s == &listen_ip) && !bound.ip().is_unspecified() {
            wt_sans.push(listen_ip);
        }
        let wt_addr_opt = match wtransport::Identity::self_signed(&wt_sans) {
            Ok(wt_identity) => {
                let cert_hash = wt_identity.certificate_chain().as_slice()[0].hash();
                let cert_hex = sha256_digest_to_hex(&cert_hash);
                let wt_bind: SocketAddr = bound;
                match build_server_endpoint(wt_bind, wt_identity, Some(Duration::from_secs(15))) {
                    Ok(endpoint) => {
                        let wt_url = format!(
                            "wt://{bound}#x25519={}&cert-sha256={cert_hex}",
                            hex::encode(x25519_public),
                        );
                        spawn_wt_accept_loop(endpoint, wt_accept_tx);
                        Some(wt_url)
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "wt: server endpoint bind failed; degrading to WS-only");
                        None
                    }
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "wt: self-signed identity failed; degrading to WS-only");
                None
            }
        };

        // 7. Combine WS + WT into the engine's inbound transport.
        let transport = DualInboundTransport::new(ws_transport, wt_transport);

        // 8. Engine.
        let local_peer = PeerId(VerifyingKey::new(Bytes::copy_from_slice(&ed25519_public)));
        let signer: Arc<dyn Signer> = Arc::new(identity.clone());
        let engine = Rc::new(SyncEngine::new(
            store.clone(),
            transport,
            SyncConfig::default(),
            local_peer,
            signer,
        ));

        // 9. Subscription filter for the relay's broad ingestion.
        let subscription_filter = match config.interest_filter {
            InterestFilter::All => Filter::NamePrefix(Bytes::new()),
        };

        // 10. Bridge channels.
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<RelayCommand>();

        // 11. Command pump context + task.
        let cmd_ctx = Rc::new(CommandContext {
            engine: engine.clone(),
            store: store.clone(),
            data_dir: config.data_dir.clone(),
            ed25519_public,
            x25519_public,
            listen_addr: bound,
            dial_url: local_address.clone(),
            webtransport_address: wt_addr_opt.clone(),
            configured_peers: config.peers.clone(),
        });
        spawn_command_pump(cmd_rx, cmd_ctx.clone());

        // 12. Banner.
        let mut banner = identity::format_address(&bound, &identity);
        banner.push_str(&format!("\n  dashboard: http://{bound}/dashboard"));
        banner.push_str(&format!("\n  identity:  http://{bound}/"));
        if let Some(wt) = &wt_addr_opt {
            banner.push_str(&format!("\n  wt:        {wt}"));
        } else {
            banner.push_str("\n  wt:        (disabled — UDP bind failed)");
        }
        tracing::info!("\n{}", banner);
        println!("{banner}");

        Ok(RelayHandle {
            local_address,
            ed25519_public,
            x25519_public,
            engine,
            peers: config.peers,
            subscription_filter,
            listener: Some(listener),
            ws_tx,
            cmd_tx,
            cmd_ctx,
        })
    }
}

/// Accept loop for inbound WebTransport sessions. Each accepted session
/// is pushed onto `accept_tx` for the engine-side
/// `WebTransportRawTransport::serving()` to drain. Failures inside one
/// session don't break the loop; the relay tolerates malformed or
/// half-completed handshakes by skipping them.
///
/// Runs as a `tokio::spawn` (Send) task because
/// `wtransport::Endpoint::accept` is itself Send-friendly. The
/// per-session `Connection` it produces is not Send, but it crosses to
/// the engine LocalSet via the `accept_tx` channel and never gets
/// touched from this task again.
fn spawn_wt_accept_loop(
    endpoint: wtransport::Endpoint<wtransport::endpoint::endpoint_side::Server>,
    accept_tx: mpsc::UnboundedSender<wtransport::Connection>,
) {
    tokio::spawn(async move {
        loop {
            let incoming = endpoint.accept().await;
            let session_request = match incoming.await {
                Ok(req) => req,
                Err(e) => {
                    tracing::debug!(error = %e, "wt: incoming session rejected before request");
                    continue;
                }
            };
            let conn = match session_request.accept().await {
                Ok(c) => c,
                Err(e) => {
                    tracing::debug!(error = %e, "wt: session_request.accept failed");
                    continue;
                }
            };
            if accept_tx.send(conn).is_err() {
                tracing::info!("wt: accept channel closed; loop exiting");
                break;
            }
        }
    });
}

fn spawn_command_pump(mut cmd_rx: mpsc::UnboundedReceiver<RelayCommand>, ctx: Rc<CommandContext>) {
    tokio::task::spawn_local(async move {
        while let Some(cmd) = cmd_rx.recv().await {
            match cmd {
                RelayCommand::Snapshot { reply } => {
                    let meta = crate::snapshot::RelayMeta {
                        data_dir: &ctx.data_dir,
                        ed25519_public: ctx.ed25519_public,
                        x25519_public: ctx.x25519_public,
                        listen_addr: ctx.listen_addr,
                        dial_url: &ctx.dial_url,
                        configured_peers: &ctx.configured_peers,
                    };
                    let snap = build_dashboard_snapshot(&ctx.engine, &ctx.store, &meta).await;
                    let _ = reply.send(snap);
                }
                RelayCommand::Identity { reply } => {
                    let snap = build_identity_snapshot(
                        ctx.ed25519_public,
                        ctx.x25519_public,
                        &ctx.dial_url,
                        ctx.webtransport_address.as_deref(),
                    );
                    let _ = reply.send(snap);
                }
            }
        }
    });
}

impl RelayHandle {
    pub fn dial_address(&self) -> String {
        self.local_address.clone()
    }

    async fn dial_configured_peers(&self) {
        use sunset_relay_resolver::Resolver;
        let resolver = Resolver::new(crate::resolver_adapter::ReqwestFetch::default());
        for peer_url in &self.peers {
            let canonical = match resolver.resolve(peer_url).await {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!(peer = %peer_url, error = %e, "peer resolve failed, skipping");
                    continue;
                }
            };
            let addr = PeerAddr::new(Bytes::from(canonical));
            if let Err(e) = self.engine.add_peer(addr).await {
                tracing::warn!(peer = %peer_url, error = %e, "federated peer dial failed, continuing");
            } else {
                tracing::info!(peer = %peer_url, "federated peer dialed");
            }
        }
    }

    fn build_app_state(&self) -> AppState {
        AppState {
            ws_tx: self.ws_tx.clone(),
            cmd_tx: self.cmd_tx.clone(),
        }
    }

    /// Drive the engine + axum until shutdown.
    pub async fn run(mut self) -> Result<()> {
        let listener = self
            .listener
            .take()
            .expect("RelayHandle::run consumed twice");
        let app: Router = build_app(self.build_app_state());

        let engine_clone = self.engine.clone();
        let engine_task = tokio::task::spawn_local(async move { engine_clone.run().await });

        // axum runs as a Send task on the multi-thread runtime workers.
        let serve_task = tokio::spawn(async move { axum::serve(listener, app).await });

        // Subscription publish + federated dials happen on the engine side.
        self.engine
            .publish_subscription(self.subscription_filter.clone(), Duration::from_secs(3600))
            .await?;
        tracing::info!("published broad subscription");
        self.dial_configured_peers().await;

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
        serve_task.abort();
        Ok(())
    }

    /// For tests: drive engine + axum without waiting for OS signals.
    /// Returns the engine task handle so the caller can abort it during teardown.
    /// The axum task is detached; the test runtime drop will cancel it.
    #[cfg(any(test, feature = "test-helpers"))]
    pub async fn run_for_test(
        &mut self,
    ) -> Result<tokio::task::JoinHandle<sunset_sync::Result<()>>> {
        let listener = self
            .listener
            .take()
            .expect("RelayHandle::run_for_test consumed twice");
        let app: Router = build_app(self.build_app_state());

        let engine_clone = self.engine.clone();
        let engine_task = tokio::task::spawn_local(async move { engine_clone.run().await });

        let _serve_task = tokio::spawn(async move { axum::serve(listener, app).await });

        self.engine
            .publish_subscription(self.subscription_filter.clone(), Duration::from_secs(3600))
            .await?;
        self.dial_configured_peers().await;

        Ok(engine_task)
    }

    /// For tests: access the underlying engine.
    #[cfg(any(test, feature = "test-helpers"))]
    pub fn engine(&self) -> &Rc<Engine> {
        &self.engine
    }
}

// `cmd_ctx` is held inside `RelayHandle` so the command pump's `Rc` graph
// stays alive for the relay's lifetime. When `RelayHandle` drops:
//   • `cmd_tx` drops → cmd_rx returns None → pump task exits.
//   • `cmd_ctx` (this clone) drops → refcount drops by 1.
//   • The pump task's own `cmd_ctx` clone drops when the task ends →
//     refcount → 0 → CommandContext drops, releasing Rc<Engine> and Arc<FsStore>.
// The empty Drop body marks this as a deliberate ownership shape, not an
// oversight. tracing::trace! could go here in the future.
impl Drop for RelayHandle {
    fn drop(&mut self) {}
}
