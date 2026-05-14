//! `QuicRawTransport`: the NAT-hole-punched QUIC `RawTransport` impl.
//!
//! Owns one shared UDP socket (wrapped in [`HolepunchSocket`]) and one
//! [`quinn::Endpoint`] that demultiplexes all peer connections.
//! Coordinates the holepunch + QUIC handshake via the [`Signaler`].

use std::cell::RefCell;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use tokio::sync::{Mutex, mpsc, oneshot};
use tokio::task::spawn_local;

use sunset_store::VerifyingKey;
use sunset_sync::{
    Error as SyncError, PeerAddr, PeerId, RawTransport, Result as SyncResult, SignalMessage,
    Signaler,
};

use crate::cert::{SelfSignedCert, generate as generate_cert};
use crate::connection::QuicRawConnection;
use crate::coordinator::HolepunchCoordinator;
use crate::discovery::discover;
use crate::socket::HolepunchSocket;
use crate::verifier::PinnedCertVerifier;
use crate::wire::{Candidates, Probe, QuicSignal};

const HANDSHAKE_BUDGET: Duration = Duration::from_secs(5);
const SNI: &str = "sunset";

/// Cheap-to-clone handle to a single QUIC-over-holepunch transport.
/// Clones share the same UDP socket, quinn endpoint, dispatcher state,
/// and `accept()` queue.
#[derive(Clone)]
pub struct QuicRawTransport {
    signaler: Rc<dyn Signaler>,
    local_peer: PeerId,
    cert: Arc<SelfSignedCert>,
    socket: Arc<HolepunchSocket>,
    endpoint: quinn::Endpoint,
    /// Discovered at `bind()` time — local interface + STUN-reflexive
    /// addresses for the shared UDP socket. Cached for the transport's
    /// lifetime; the NAT mapping is held open by ongoing QUIC traffic.
    local_candidates: Rc<Vec<SocketAddr>>,
    inner: Rc<RefCell<Inner>>,
    completed_rx: Rc<Mutex<mpsc::UnboundedReceiver<SyncResult<QuicRawConnection>>>>,
}

type ProbeRouteKey = ([u8; 16], [u8; 32]);
type ProbeBytes = (SocketAddr, Bytes);
type ProbeRouteTable = HashMap<ProbeRouteKey, mpsc::UnboundedSender<ProbeBytes>>;

/// One per-peer handshake-in-flight slot, keyed by `PeerId` in
/// [`Inner::per_peer`]. The slot lives for the duration of one
/// `run_handshake` invocation (either as initiator or acceptor) and is
/// removed when that invocation returns.
struct PeerHandshake {
    /// Inbound Candidates queue for this peer (driven by the signaler
    /// dispatcher). The handshake's owner drains this in step 2 of
    /// `run_handshake`.
    candidates_tx: mpsc::UnboundedSender<Candidates>,
    /// Who's running the handshake.
    role_owner: RoleOwner,
}

/// Identifies who is running the in-flight handshake for a peer and
/// where its result should be delivered.
enum RoleOwner {
    /// A `connect()` call from local code is running the handshake.
    /// A second `connect()` to the same peer can't usefully claim this
    /// in-flight attempt — the result connection moves only once.
    Connector,
    /// The dispatcher spawned an acceptor task in response to an
    /// inbound `Candidates`. The `Option<oneshot>` lets a *later*
    /// `connect()` call claim the acceptor's result: when set, the
    /// acceptor sends its `Ok(connection)`/`Err(_)` here instead of to
    /// `completed_tx`. This is how true simultaneous-open works:
    /// peer A's inbound Candidates triggers our acceptor, then our own
    /// `connect(A)` arrives and "claims" the in-flight handshake.
    Acceptor(Option<oneshot::Sender<SyncResult<QuicRawConnection>>>),
}

/// One QUIC accept-side waiter. Pre-registered by a responder task at
/// step 3a of `run_handshake` so an early-arriving QUIC initial isn't
/// refused by the accept router.
struct AcceptWaiter {
    tx: mpsc::UnboundedSender<quinn::Incoming>,
    /// The remote peer's advertised candidate addresses, used to
    /// route inbound `Incoming`s to the correct responder when several
    /// peers are connecting concurrently.
    candidate_addrs: Vec<SocketAddr>,
}

struct Inner {
    per_peer: HashMap<PeerId, PeerHandshake>,
    probe_routes: ProbeRouteTable,
    /// Pre-registered acceptors keyed by the remote peer's advertised
    /// candidate addresses. Routed by `run_accept_router` using
    /// best-match: exact `(ip, port)` → IP-only → oldest live waiter.
    accept_waiters: Vec<AcceptWaiter>,
    completed_tx: mpsc::UnboundedSender<SyncResult<QuicRawConnection>>,
    probe_rx: Option<mpsc::UnboundedReceiver<ProbeBytes>>,
    dispatcher_started: bool,
}

impl QuicRawTransport {
    /// Bind the shared UDP socket, run STUN, build the quinn endpoint.
    /// Dispatcher tasks start lazily on the first
    /// `connect()`/`accept()` call (so a caller can `bind()` outside a
    /// LocalSet and then start using the transport inside one).
    pub async fn bind(
        signaler: Rc<dyn Signaler>,
        local_peer: PeerId,
        stun_servers: Vec<String>,
    ) -> SyncResult<Self> {
        // rustls 0.23 requires a CryptoProvider to be installed as the
        // process-default before any ServerConfig/ClientConfig builder
        // can run. `ring` is gated by our Cargo features. Ignoring the
        // result lets concurrent transport instances coexist.
        let _ = rustls::crypto::ring::default_provider().install_default();

        // Bind, set non-blocking, do STUN on the bound socket BEFORE
        // handing it off to quinn (avoids the two-fd-same-socket recv
        // race). After STUN we convert back to std::net::UdpSocket and
        // hand to quinn.
        let std_udp = std::net::UdpSocket::bind("0.0.0.0:0")
            .map_err(|e| SyncError::Transport(format!("quic bind udp: {e}")))?;
        std_udp
            .set_nonblocking(true)
            .map_err(|e| SyncError::Transport(format!("set_nonblocking: {e}")))?;
        let tokio_udp = tokio::net::UdpSocket::from_std(std_udp)
            .map_err(|e| SyncError::Transport(format!("tokio from_std: {e}")))?;
        let local_addrs = discover(&tokio_udp, &stun_servers).await;
        let std_udp = tokio_udp
            .into_std()
            .map_err(|e| SyncError::Transport(format!("tokio into_std: {e}")))?;

        let (probe_tx, probe_rx) = mpsc::unbounded_channel();
        let socket = Arc::new(
            HolepunchSocket::new(std_udp, probe_tx)
                .map_err(|e| SyncError::Transport(format!("holepunch socket: {e}")))?,
        );
        let cert = generate_cert().map_err(|e| SyncError::Transport(format!("cert gen: {e}")))?;
        let server_config = build_server_config(&cert)
            .map_err(|e| SyncError::Transport(format!("build server config: {e}")))?;
        let runtime: Arc<dyn quinn::Runtime> = Arc::new(quinn::TokioRuntime);
        let abstract_socket: Arc<dyn quinn::AsyncUdpSocket> = socket.clone();
        let endpoint = quinn::Endpoint::new_with_abstract_socket(
            quinn::EndpointConfig::default(),
            Some(server_config),
            abstract_socket,
            runtime,
        )
        .map_err(|e| SyncError::Transport(format!("quinn endpoint: {e}")))?;

        let (completed_tx, completed_rx) = mpsc::unbounded_channel();

        Ok(Self {
            signaler,
            local_peer,
            cert: Arc::new(cert),
            socket,
            endpoint,
            local_candidates: Rc::new(local_addrs),
            inner: Rc::new(RefCell::new(Inner {
                per_peer: HashMap::new(),
                probe_routes: HashMap::new(),
                accept_waiters: Vec::new(),
                completed_tx,
                probe_rx: Some(probe_rx),
                dispatcher_started: false,
            })),
            completed_rx: Rc::new(Mutex::new(completed_rx)),
        })
    }

    /// Lazily start the signaler dispatcher, probe router, and accept
    /// router. Must be called from inside a `tokio::task::LocalSet` —
    /// the signaler is `?Send` and tasks are spawn_local'd.
    fn ensure_dispatcher(&self) {
        let mut inner = self.inner.borrow_mut();
        if inner.dispatcher_started {
            return;
        }
        inner.dispatcher_started = true;
        let probe_rx = inner
            .probe_rx
            .take()
            .expect("probe_rx must be Some on first dispatcher start");
        drop(inner);

        let me = self.clone();
        spawn_local(async move {
            me.run_signaler_dispatcher().await;
        });
        let me = self.clone();
        spawn_local(async move {
            me.run_probe_router(probe_rx).await;
        });
        let me = self.clone();
        spawn_local(async move {
            me.run_accept_router().await;
        });
    }

    async fn run_signaler_dispatcher(self) {
        loop {
            let msg = match self.signaler.recv().await {
                Ok(m) => m,
                Err(e) => {
                    tracing::debug!("quic signaler.recv: {e}");
                    return;
                }
            };
            if msg.to != self.local_peer {
                continue;
            }
            let signal: QuicSignal = match postcard::from_bytes(&msg.payload) {
                Ok(s) => s,
                Err(e) => {
                    tracing::debug!("quic signal decode: {e}");
                    continue;
                }
            };
            match signal {
                QuicSignal::Candidates(candidates) => {
                    self.route_inbound_candidates(&msg.from, candidates);
                }
            }
        }
    }

    fn route_inbound_candidates(&self, from: &PeerId, candidates: Candidates) {
        // If a handshake is already in flight for this peer (either
        // we're running connect() to them, or a prior acceptor task
        // is mid-flight), forward the Candidates to that handshake's
        // queue. Don't spawn a second acceptor.
        let existing = self
            .inner
            .borrow()
            .per_peer
            .get(from)
            .map(|s| s.candidates_tx.clone());
        if let Some(tx) = existing {
            let _ = tx.send(candidates);
            return;
        }
        let (peer_tx, peer_rx) = mpsc::unbounded_channel();
        self.inner.borrow_mut().per_peer.insert(
            from.clone(),
            PeerHandshake {
                candidates_tx: peer_tx,
                role_owner: RoleOwner::Acceptor(None),
            },
        );
        let me = self.clone();
        let from = from.clone();
        spawn_local(async move {
            let result = me
                .run_handshake(
                    from.clone(),
                    peer_rx,
                    ConnectRole::Acceptor {
                        initial: candidates,
                    },
                )
                .await;
            // Take the slot out atomically so we can read its (possibly
            // updated) role_owner. A racing `connect(from)` may have
            // claimed the result by writing into `Acceptor(Some(_))`.
            let claim = me
                .inner
                .borrow_mut()
                .per_peer
                .remove(&from)
                .and_then(|slot| match slot.role_owner {
                    RoleOwner::Acceptor(claim) => claim,
                    RoleOwner::Connector => None,
                });
            match claim {
                Some(tx) => {
                    let _ = tx.send(result);
                }
                None => {
                    let completed_tx = me.inner.borrow().completed_tx.clone();
                    let _ = completed_tx.send(result);
                }
            }
        });
    }

    async fn run_probe_router(self, mut probe_rx: mpsc::UnboundedReceiver<ProbeBytes>) {
        while let Some((src, body)) = probe_rx.recv().await {
            let probe = match Probe::decode(&body) {
                Ok(Some(p)) => p,
                _ => continue,
            };
            let key = (probe.session_id, probe.sender_pk);
            let target = self.inner.borrow().probe_routes.get(&key).cloned();
            if let Some(tx) = target {
                let _ = tx.send((src, body));
            }
        }
    }

    async fn run_accept_router(self) {
        while let Some(incoming) = self.endpoint.accept().await {
            let mut incoming = Some(incoming);
            loop {
                let target_idx = {
                    let mut inner = self.inner.borrow_mut();
                    // Sweep waiters whose receiver has been dropped
                    // (responders that errored before awaiting). This
                    // both bounds the Vec and skips them in selection.
                    inner.accept_waiters.retain(|w| !w.tx.is_closed());
                    let inc_addr = incoming
                        .as_ref()
                        .expect("incoming present at start of loop")
                        .remote_address();
                    // Best-match routing for multi-peer demux:
                    // 1) exact (ip, port) — peer's QUIC initial source
                    //    matches one of their advertised candidates;
                    // 2) IP-only — same NAT, different port (path
                    //    drift between holepunch and QUIC initial);
                    // 3) oldest live waiter — single-peer fallback so
                    //    a probe/initial source mismatch still works.
                    let exact = inner
                        .accept_waiters
                        .iter()
                        .position(|w| w.candidate_addrs.contains(&inc_addr));
                    let by_ip = exact.or_else(|| {
                        let ip = inc_addr.ip();
                        inner
                            .accept_waiters
                            .iter()
                            .position(|w| w.candidate_addrs.iter().any(|c| c.ip() == ip))
                    });
                    by_ip.or_else(|| {
                        if inner.accept_waiters.is_empty() {
                            None
                        } else {
                            Some(0)
                        }
                    })
                };
                let Some(idx) = target_idx else { break };
                let waiter = self.inner.borrow_mut().accept_waiters.remove(idx);
                let i = incoming.take().expect("incoming present at send");
                match waiter.tx.send(i) {
                    Ok(()) => break,
                    Err(e) => {
                        // Receiver was dropped between sweep and send.
                        // Recover the Incoming and try the next waiter.
                        incoming = Some(e.0);
                    }
                }
            }
            if let Some(i) = incoming {
                i.refuse();
            }
        }
    }

    async fn run_handshake(
        &self,
        remote: PeerId,
        peer_rx: mpsc::UnboundedReceiver<Candidates>,
        role: ConnectRole,
    ) -> SyncResult<QuicRawConnection> {
        let session_id: [u8; 16] = rand_bytes_16()?;
        let local_pk = pubkey_array(self.local_peer.verifying_key())?;
        let remote_pk = pubkey_array(remote.verifying_key())?;

        // 1. Build & send our Candidates via the signaler.
        let local_candidates = Candidates {
            session_id,
            addresses: (*self.local_candidates).clone(),
            server_cert_sha256: self.cert.cert_sha256,
        };
        let payload = postcard::to_allocvec(&QuicSignal::Candidates(local_candidates))
            .map_err(|e| SyncError::Transport(format!("encode candidates: {e}")))?;
        self.signaler
            .send(SignalMessage {
                from: self.local_peer.clone(),
                to: remote.clone(),
                seq: 0,
                payload: Bytes::from(payload),
            })
            .await?;

        // 2. Receive the peer's Candidates.
        let remote_candidates = match role {
            ConnectRole::Acceptor { initial } => initial,
            ConnectRole::Initiator => {
                let mut peer_rx = peer_rx;
                tokio::time::timeout(HANDSHAKE_BUDGET, peer_rx.recv())
                    .await
                    .map_err(|_| SyncError::Transport("holepunch: signaling timeout".into()))?
                    .ok_or_else(|| {
                        SyncError::Transport("holepunch: signaling channel closed".into())
                    })?
            }
        };

        // 3. Pick the shared session_id deterministically (lower-pubkey
        //    side's session wins). Both peers compute the same value
        //    without an extra round-trip. Probes use this shared id;
        //    Candidates carries each side's own choice for the tiebreak.
        let is_initiator = local_pk < remote_pk;
        let shared_session_id = if is_initiator {
            session_id
        } else {
            remote_candidates.session_id
        };

        // 3a. If we'll be the QUIC responder, pre-register our accept
        //     waiter NOW — before holepunch runs. The QUIC client may
        //     send its Initial packet within milliseconds of confirming
        //     its side of the holepunch, and that can land at the
        //     responder's accept_router before the responder finishes
        //     holepunch on its side. With no waiter registered, the
        //     accept_router refuses the Incoming and the client's
        //     handshake fails. Registering early keeps an unbounded
        //     buffer ready to catch the Incoming.
        let accept_rx_opt = if is_initiator {
            None
        } else {
            let (accept_tx, accept_rx) = mpsc::unbounded_channel();
            self.inner.borrow_mut().accept_waiters.push(AcceptWaiter {
                tx: accept_tx,
                candidate_addrs: remote_candidates.addresses.clone(),
            });
            Some(accept_rx)
        };

        // 4. Register probe route + run coordinator.
        let (probe_tx, coord_rx) = mpsc::unbounded_channel();
        let probe_key = (shared_session_id, remote_pk);
        self.inner
            .borrow_mut()
            .probe_routes
            .insert(probe_key, probe_tx);
        let confirmed = HolepunchCoordinator::new(
            Arc::clone(&self.socket),
            shared_session_id,
            local_pk,
            remote_pk,
            remote_candidates.addresses.clone(),
            coord_rx,
        )
        .run(HANDSHAKE_BUDGET)
        .await
        .map_err(|e| {
            self.inner.borrow_mut().probe_routes.remove(&probe_key);
            SyncError::Transport(format!("{e}"))
        })?;
        self.inner.borrow_mut().probe_routes.remove(&probe_key);

        // 5. QUIC handshake.
        let quic_conn = if is_initiator {
            let client_config = build_client_config(remote_candidates.server_cert_sha256)?;
            let connecting = self
                .endpoint
                .connect_with(client_config, confirmed.addr, SNI)
                .map_err(|e| SyncError::Transport(format!("quic connect_with: {e}")))?;
            connecting
                .await
                .map_err(|e| SyncError::Transport(format!("quic connect: {e}")))?
        } else {
            // confirmed.addr is unused on the responder side: the
            // pre-registered accept_waiter (step 3a) holds whatever
            // Incoming the QUIC client sends, regardless of source addr.
            let _ = confirmed;
            let mut accept_rx = accept_rx_opt
                .expect("accept_rx must exist when !is_initiator (pre-registered in step 3a)");
            let incoming = match tokio::time::timeout(HANDSHAKE_BUDGET, accept_rx.recv()).await {
                Ok(Some(i)) => i,
                Ok(None) => {
                    return Err(SyncError::Transport(
                        "quic accept: incoming channel closed".into(),
                    ));
                }
                Err(_) => {
                    return Err(SyncError::Transport("quic accept: incoming timeout".into()));
                }
            };
            let connecting = incoming
                .accept()
                .map_err(|e| SyncError::Transport(format!("quic accept incoming: {e}")))?;
            connecting
                .await
                .map_err(|e| SyncError::Transport(format!("quic accept finalize: {e}")))?
        };

        // 6. Open / accept the persistent reliable bidi stream.
        //
        // quinn's `open_bi` returns immediately without notifying the
        // peer — the stream is only visible to the responder once we
        // write a frame on it. So the initiator writes a 1-byte
        // stream-open marker; the responder reads it and discards.
        // After this exchange the bidi stream is fully established and
        // both sides exchange length-prefixed SyncMessage frames.
        let (send, recv) = if is_initiator {
            let (mut s, r) = quic_conn
                .open_bi()
                .await
                .map_err(|e| SyncError::Transport(format!("quic open_bi: {e}")))?;
            s.write_all(&[0u8])
                .await
                .map_err(|e| SyncError::Transport(format!("quic open marker: {e}")))?;
            (s, r)
        } else {
            let (s, mut r) = quic_conn
                .accept_bi()
                .await
                .map_err(|e| SyncError::Transport(format!("quic accept_bi: {e}")))?;
            let mut marker = [0u8; 1];
            r.read_exact(&mut marker)
                .await
                .map_err(|e| SyncError::Transport(format!("quic open marker read: {e}")))?;
            (s, r)
        };

        Ok(QuicRawConnection::new(quic_conn, send, recv))
    }
}

enum ConnectRole {
    Initiator,
    Acceptor { initial: Candidates },
}

#[async_trait(?Send)]
impl RawTransport for QuicRawTransport {
    type Connection = QuicRawConnection;

    async fn connect(&self, addr: PeerAddr) -> SyncResult<Self::Connection> {
        self.ensure_dispatcher();
        let remote = parse_addr(&addr)?;

        // First try to *claim* an in-flight acceptor for this peer.
        // The dispatcher may have spawned an acceptor task on inbound
        // Candidates from `remote` before our local `connect()` was
        // called; rather than start a competing handshake, we hand the
        // acceptor a oneshot to deliver its result and wait for it.
        // Another concurrent `connect()` (or a prior claim) loses —
        // returns `Err`, caller may retry once the in-flight finishes.
        let claim_rx = {
            let mut inner = self.inner.borrow_mut();
            match inner.per_peer.get_mut(&remote) {
                Some(slot) => match &mut slot.role_owner {
                    RoleOwner::Connector => {
                        return Err(SyncError::Transport(format!(
                            "quic connect: another connect() in flight for {:?}",
                            remote.verifying_key().as_bytes()
                        )));
                    }
                    RoleOwner::Acceptor(claim) => {
                        if claim.is_some() {
                            return Err(SyncError::Transport(format!(
                                "quic connect: acceptor already claimed for {:?}",
                                remote.verifying_key().as_bytes()
                            )));
                        }
                        let (tx, rx) = oneshot::channel();
                        *claim = Some(tx);
                        Some(rx)
                    }
                },
                None => None,
            }
        };
        if let Some(rx) = claim_rx {
            return rx.await.unwrap_or_else(|_| {
                Err(SyncError::Transport(
                    "quic connect: in-flight acceptor was dropped".into(),
                ))
            });
        }

        // No in-flight handshake — install ourselves as Connector and
        // drive the handshake directly.
        let (peer_tx, peer_rx) = mpsc::unbounded_channel::<Candidates>();
        self.inner.borrow_mut().per_peer.insert(
            remote.clone(),
            PeerHandshake {
                candidates_tx: peer_tx,
                role_owner: RoleOwner::Connector,
            },
        );
        let result = self
            .run_handshake(remote.clone(), peer_rx, ConnectRole::Initiator)
            .await;
        self.inner.borrow_mut().per_peer.remove(&remote);
        result
    }

    async fn accept(&self) -> SyncResult<Self::Connection> {
        self.ensure_dispatcher();
        let mut rx = self.completed_rx.lock().await;
        rx.recv()
            .await
            .ok_or_else(|| SyncError::Transport("quic accept: completed channel closed".into()))?
    }
}

fn parse_addr(addr: &PeerAddr) -> SyncResult<PeerId> {
    let s = std::str::from_utf8(addr.as_bytes())
        .map_err(|e| SyncError::Transport(format!("quic addr not utf-8: {e}")))?;
    let no_frag = s.split('#').next().unwrap_or(s);
    let suffix = no_frag
        .strip_prefix("quic://")
        .ok_or_else(|| SyncError::Transport(format!("quic addr not quic://: {s}")))?;
    let bytes = hex::decode(suffix)
        .map_err(|e| SyncError::Transport(format!("quic addr hex decode: {e}")))?;
    Ok(PeerId(VerifyingKey::new(Bytes::from(bytes))))
}

fn pubkey_array(vk: &VerifyingKey) -> SyncResult<[u8; 32]> {
    let b: &[u8] = vk.as_bytes();
    b.try_into()
        .map_err(|_| SyncError::Transport(format!("quic pubkey wrong length: {}", b.len())))
}

fn rand_bytes_16() -> SyncResult<[u8; 16]> {
    let mut out = [0u8; 16];
    getrandom::fill(&mut out).map_err(|e| SyncError::Transport(format!("getrandom: {e}")))?;
    Ok(out)
}

fn build_server_config(cert: &SelfSignedCert) -> Result<quinn::ServerConfig, String> {
    let cert_chain = vec![CertificateDer::from(cert.cert_der.clone())];
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(cert.private_key_der.clone()));
    let mut server_crypto = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_protocol_versions(&[&rustls::version::TLS13])
    .map_err(|e| format!("server tls13 only: {e}"))?
    .with_no_client_auth()
    .with_single_cert(cert_chain, key)
    .map_err(|e| format!("server with_single_cert: {e}"))?;
    server_crypto.alpn_protocols = vec![b"sunset-quic-v1".to_vec()];
    let quic_server = quinn::crypto::rustls::QuicServerConfig::try_from(server_crypto)
        .map_err(|e| format!("quic server crypto: {e}"))?;
    Ok(quinn::ServerConfig::with_crypto(Arc::new(quic_server)))
}

fn build_client_config(expected_cert_sha256: [u8; 32]) -> SyncResult<quinn::ClientConfig> {
    let verifier = PinnedCertVerifier::new(expected_cert_sha256);
    let mut client_crypto = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_protocol_versions(&[&rustls::version::TLS13])
    .map_err(|e| SyncError::Transport(format!("client tls13 only: {e}")))?
    .dangerous()
    .with_custom_certificate_verifier(verifier)
    .with_no_client_auth();
    client_crypto.alpn_protocols = vec![b"sunset-quic-v1".to_vec()];
    let quic_client = quinn::crypto::rustls::QuicClientConfig::try_from(client_crypto)
        .map_err(|e| SyncError::Transport(format!("quic client crypto: {e}")))?;
    Ok(quinn::ClientConfig::new(Arc::new(quic_client)))
}
