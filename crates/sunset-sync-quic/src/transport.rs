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
use tokio::sync::{Mutex, mpsc};
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

struct Inner {
    per_peer: HashMap<PeerId, mpsc::UnboundedSender<Candidates>>,
    probe_routes: ProbeRouteTable,
    /// FIFO queue of acceptor tasks awaiting the next QUIC `Incoming`.
    /// Source-address routing is unreliable on multi-homed hosts: the
    /// holepunch probe and the QUIC initial can take different paths,
    /// giving the responder a `confirmed.addr` that doesn't match the
    /// `Incoming.remote_address()`. The v1 contract is "at most one
    /// concurrent acceptor per transport"; multi-peer concurrent
    /// inbound is a known limitation (NoiseTransport on top catches
    /// any cross-routing via its identity binding).
    accept_waiters: std::collections::VecDeque<mpsc::UnboundedSender<quinn::Incoming>>,
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
                accept_waiters: std::collections::VecDeque::new(),
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
        let existing = self.inner.borrow().per_peer.get(from).cloned();
        if let Some(tx) = existing {
            let _ = tx.send(candidates);
            return;
        }
        let (peer_tx, peer_rx) = mpsc::unbounded_channel();
        self.inner
            .borrow_mut()
            .per_peer
            .insert(from.clone(), peer_tx);
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
            let completed_tx = me.inner.borrow().completed_tx.clone();
            let _ = completed_tx.send(result);
            me.inner.borrow_mut().per_peer.remove(&from);
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
            // Walk the FIFO, popping dead waiters (responders that
            // pre-registered but errored before awaiting their rx)
            // until we find a live one or run out. If we run out, the
            // Incoming is refused (no one's listening).
            let mut incoming = Some(incoming);
            loop {
                let tx = match self.inner.borrow_mut().accept_waiters.pop_front() {
                    Some(t) => t,
                    None => break,
                };
                if tx.is_closed() {
                    continue;
                }
                let i = incoming.take().expect("incoming present in loop");
                match tx.send(i) {
                    Ok(()) => break,
                    Err(e) => {
                        // Receiver was dropped between is_closed() and
                        // send(). Recover the Incoming and try the next
                        // waiter.
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
            self.inner.borrow_mut().accept_waiters.push_back(accept_tx);
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
        let (peer_tx, peer_rx) = mpsc::unbounded_channel::<Candidates>();
        {
            let mut inner = self.inner.borrow_mut();
            if inner.per_peer.contains_key(&remote) {
                return Err(SyncError::Transport(format!(
                    "quic connect: handshake already in flight for {:?}",
                    remote.verifying_key().as_bytes()
                )));
            }
            inner.per_peer.insert(remote.clone(), peer_tx);
        }
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
