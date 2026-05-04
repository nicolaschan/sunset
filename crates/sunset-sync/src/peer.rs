//! Per-peer connection task.

use std::rc::Rc;

use bytes::Bytes;
use tokio::sync::mpsc;

use crate::error::Result;
use crate::message::SyncMessage;
use crate::transport::TransportConnection;
use crate::types::PeerId;

/// An event emitted by a per-peer task to the engine.
#[derive(Debug)]
pub(crate) enum InboundEvent {
    /// Hello received; the peer's identity is now known.
    /// Protocol-version validation has already happened in the per-peer task.
    /// `out_tx` is the outbound sender to register under `peer_id`.
    PeerHello {
        peer_id: PeerId,
        // Stored alongside the per-peer outbound sender so Task 6 can
        // filter stale Disconnected events from defunct connection
        // generations (cross-checks against `peer_outbound[peer_id].conn_id`).
        conn_id: crate::engine::ConnectionId,
        kind: crate::transport::TransportKind,
        out_tx: tokio::sync::mpsc::UnboundedSender<SyncMessage>,
        /// One-shot fired by the engine after `peer_outbound` is
        /// populated, to wake the `add_peer().await` caller. Threading
        /// the signal through the inbound event (rather than firing it
        /// directly from the per-peer task on Hello receipt) is what
        /// makes `add_peer().await` returning imply the peer is fully
        /// routable: a caller that immediately publishes / inserts
        /// won't drop the entry on the floor against an empty
        /// `peer_outbound`. Inbound peers (server-side accept) pass
        /// `None`.
        registered: Option<
            tokio::sync::oneshot::Sender<
                crate::error::Result<(PeerId, crate::transport::TransportKind)>,
            >,
        >,
    },
    /// A SyncMessage arrived (other than Hello).
    Message { from: PeerId, message: SyncMessage },
    /// The peer's connection closed (graceful or error). The `conn_id`
    /// identifies *which* connection died; the engine filters stale
    /// disconnects whose `conn_id` no longer matches the current entry
    /// in `peer_outbound[peer_id]`.
    Disconnected {
        peer_id: PeerId,
        /// Identifies the connection generation that died. The engine
        /// compares this against `peer_outbound[peer_id].conn_id` and
        /// drops stale events from defunct generations.
        conn_id: crate::engine::ConnectionId,
        reason: String,
    },
    /// A `Pong` was received from a peer; carries the round-trip time
    /// measured against the most recent `Ping` send and the wall-clock
    /// instant the Pong was observed. Engine re-emits as
    /// `EngineEvent::PongObserved` for supervisor / UI consumption.
    PongObserved {
        peer_id: PeerId,
        rtt_ms: u64,
        observed_at_unix_ms: u64,
    },
}

/// Engine-level context shared across all per-peer tasks. Bundling these
/// into a single struct keeps `run_peer` / `spawn_run_peer` under clippy's
/// `too_many_arguments` threshold without papering over the lint.
#[derive(Clone)]
pub(crate) struct PeerEnv {
    pub local_peer: PeerId,
    pub protocol_version: u32,
    pub heartbeat_interval: std::time::Duration,
    pub heartbeat_timeout: std::time::Duration,
}

/// Drive a single peer's connection.
///
/// Sends our `Hello`, waits for the peer's `Hello`, then runs concurrent
/// recv + send loops until the connection drops.
///
/// `outbound_rx` is the receiver half of the per-peer outbound channel —
/// the engine sends `SyncMessage`s into this channel and they are written
/// to the transport here.
///
/// `inbound_tx` is the shared sender into the engine's main inbound queue.
///
/// `env` carries the engine's `local_peer`, protocol version, and heartbeat
/// timing.
///
/// `out_tx` is the outbound sender — passed through `PeerHello` so the engine
/// can register it under the Hello-declared peer_id (not the transport-level
/// peer_id, which may differ in schemes that separate routing identity from
/// application identity, e.g. X25519 routing + Ed25519 application keys).
pub(crate) async fn run_peer<C: TransportConnection + 'static>(
    conn: Rc<C>,
    env: PeerEnv,
    conn_id: crate::engine::ConnectionId,
    out_tx: mpsc::UnboundedSender<SyncMessage>,
    mut outbound_rx: mpsc::UnboundedReceiver<SyncMessage>,
    inbound_tx: mpsc::UnboundedSender<InboundEvent>,
    hello_done: Option<
        tokio::sync::oneshot::Sender<Result<(PeerId, crate::transport::TransportKind)>>,
    >,
) {
    let PeerEnv {
        local_peer,
        protocol_version: local_protocol_version,
        heartbeat_interval,
        heartbeat_timeout,
    } = env;
    let local_kind = conn.kind();
    // Clone of the outbound sender used by the recv-side Pong responder
    // and the liveness Ping sender. The original `out_tx` is moved into
    // the `PeerHello` event so the engine can register it under the
    // peer-declared `peer_id`; both background tasks need an additional
    // handle to enqueue messages on the same per-peer outbound channel.
    let out_tx_clone = out_tx.clone();
    // Send our Hello.
    let our_hello = SyncMessage::Hello {
        protocol_version: local_protocol_version,
        peer_id: local_peer.clone(),
    };
    if let Err(e) = send_reliable_message(&*conn, &our_hello).await {
        let err_str = format!("send hello: {e}");
        if let Some(s) = hello_done {
            let _ = s.send(Err(crate::error::Error::Transport(err_str.clone())));
        }
        let _ = inbound_tx.send(InboundEvent::Disconnected {
            peer_id: conn.peer_id(),
            conn_id,
            reason: err_str,
        });
        return;
    }

    // Receive the peer's Hello.
    let peer_id = match recv_reliable_message(&*conn).await {
        Ok(SyncMessage::Hello {
            protocol_version,
            peer_id,
        }) => {
            if protocol_version != local_protocol_version {
                let err_str = format!(
                    "protocol version mismatch: ours {} theirs {}",
                    local_protocol_version, protocol_version
                );
                if let Some(s) = hello_done {
                    let _ = s.send(Err(crate::error::Error::Transport(err_str.clone())));
                }
                let _ = inbound_tx.send(InboundEvent::Disconnected {
                    peer_id,
                    conn_id,
                    reason: err_str,
                });
                return;
            }
            // Pass `hello_done` through the inbound event. The engine
            // fires it after `peer_outbound` is populated, so the
            // `add_peer().await` caller can rely on the peer being
            // routable when the call returns.
            let _ = inbound_tx.send(InboundEvent::PeerHello {
                peer_id: peer_id.clone(),
                conn_id,
                kind: local_kind,
                out_tx,
                registered: hello_done,
            });
            peer_id
        }
        Ok(other) => {
            let err_str = format!("expected Hello, got {:?}", other);
            if let Some(s) = hello_done {
                let _ = s.send(Err(crate::error::Error::Transport(err_str.clone())));
            }
            let _ = inbound_tx.send(InboundEvent::Disconnected {
                peer_id: conn.peer_id(),
                conn_id,
                reason: err_str,
            });
            return;
        }
        Err(e) => {
            let err_str = format!("recv hello: {e}");
            if let Some(s) = hello_done {
                let _ = s.send(Err(crate::error::Error::Transport(err_str.clone())));
            }
            let _ = inbound_tx.send(InboundEvent::Disconnected {
                peer_id: conn.peer_id(),
                conn_id,
                reason: err_str,
            });
            return;
        }
    };

    // Pong delivery channel: recv_reliable_task forwards every observed
    // Pong's nonce here so the liveness_task can update last_pong_at AND
    // emit PongObserved with measured RTT, without sharing mutable state
    // across tasks. The `nonce` is the Pong's echoed nonce — informational
    // for logs; RTT is measured against `last_ping_sent_at` in the
    // liveness loop, which is correct because at most one Ping is in
    // flight (loop sleeps `heartbeat_interval` between sends).
    let (pong_tx, mut pong_rx) = mpsc::unbounded_channel::<u64>();

    // Concurrent recv loops — reliable and unreliable channels are
    // independent; each drains its own physical channel and routes the
    // decoded SyncMessage into the same `inbound_tx`. The engine's
    // dispatch is channel-agnostic; only the per-peer task knows which
    // wire carried the message.
    let recv_reliable_task = {
        let conn = conn.clone();
        let inbound_tx = inbound_tx.clone();
        let peer_id = peer_id.clone();
        let out_tx_for_pong = out_tx_clone.clone();
        let pong_tx = pong_tx.clone();
        async move {
            loop {
                match recv_reliable_message(&*conn).await {
                    Ok(SyncMessage::Goodbye {}) => {
                        let _ = inbound_tx.send(InboundEvent::Disconnected {
                            peer_id: peer_id.clone(),
                            conn_id,
                            reason: "peer goodbye".into(),
                        });
                        break;
                    }
                    Ok(SyncMessage::Ping { nonce }) => {
                        // Respond via the outbound channel; never call
                        // conn.send_reliable directly to avoid concurrent
                        // writes (NoiseTransport tracks nonces per send).
                        let _ = out_tx_for_pong.send(SyncMessage::Pong { nonce });
                    }
                    Ok(SyncMessage::Pong { nonce }) => {
                        // Notify liveness_task with the echoed nonce.
                        let _ = pong_tx.send(nonce);
                    }
                    Ok(message) => {
                        if inbound_tx
                            .send(InboundEvent::Message {
                                from: peer_id.clone(),
                                message,
                            })
                            .is_err()
                        {
                            break;
                        }
                    }
                    Err(e) => {
                        let _ = inbound_tx.send(InboundEvent::Disconnected {
                            peer_id: peer_id.clone(),
                            conn_id,
                            reason: format!("recv reliable: {e}"),
                        });
                        break;
                    }
                }
            }
        }
    };

    let recv_unreliable_task = {
        let conn = conn.clone();
        let inbound_tx = inbound_tx.clone();
        let peer_id = peer_id.clone();
        async move {
            // Unreliable recv error: stop the unreliable loop only.
            // Disconnection is reported by the reliable recv task —
            // unreliable can fail independently without tearing down
            // the peer. In practice the underlying channel is paired
            // with reliable, so a real disconnect will surface there
            // too.
            while let Ok(message) = recv_unreliable_message(&*conn).await {
                if inbound_tx
                    .send(InboundEvent::Message {
                        from: peer_id.clone(),
                        message,
                    })
                    .is_err()
                {
                    break;
                }
            }
        }
    };

    let send_task = {
        let conn = conn.clone();
        let inbound_tx = inbound_tx.clone();
        let peer_id = peer_id.clone();
        async move {
            while let Some(msg) = outbound_rx.recv().await {
                match outbound_kind(&msg) {
                    ChannelKind::Reliable => {
                        // Reliable failures indicate a real disconnect; emit
                        // Disconnected so the engine learns about it faster
                        // than the heartbeat timeout would, then tear down.
                        if let Err(e) = send_reliable_message(&*conn, &msg).await {
                            let _ = inbound_tx.send(InboundEvent::Disconnected {
                                peer_id: peer_id.clone(),
                                conn_id,
                                reason: format!("send reliable: {e}"),
                            });
                            break;
                        }
                    }
                    ChannelKind::Unreliable => {
                        // Unreliable is by-design lossy. A failure (transport
                        // doesn't support unreliable, queue full, etc.) drops
                        // the datagram but MUST NOT disconnect the peer —
                        // peers who only have reliable transports (e.g.
                        // WS-only) still need to function for chat traffic.
                        // Per spec failure-mode table.
                        let _ = send_unreliable_message(&*conn, &msg).await;
                    }
                }
            }
            let _ = send_reliable_message(&*conn, &SyncMessage::Goodbye {}).await;
            let _ = conn.close().await;
        }
    };

    let liveness_task = {
        let inbound_tx = inbound_tx.clone();
        let peer_id = peer_id.clone();
        let out_tx_for_ping = out_tx_clone.clone();
        async move {
            let mut next_nonce: u64 = 1;

            // Cross-platform monotonic clock for RTT and last-pong age.
            #[cfg(not(target_arch = "wasm32"))]
            use tokio::time::Instant;
            #[cfg(target_arch = "wasm32")]
            use wasmtimer::std::Instant;

            let mut last_pong_at: Instant = Instant::now();
            // Time we sent the most recent Ping. Some only between Ping
            // send and corresponding Pong receipt (or the next Ping
            // send, whichever comes first). Under healthy operation
            // exactly one Ping is in flight at a time; under congestion
            // we may send up to `heartbeat_timeout / heartbeat_interval`
            // (default 3) before the timeout fires, in which case a
            // late Pong's RTT is measured against the most recent send
            // and so under-reports — acceptable for an observability
            // signal where the precise lag matters less than "is the
            // peer alive at all", which the timeout enforces separately.
            let mut last_ping_sent_at: Option<Instant> = None;

            loop {
                #[cfg(not(target_arch = "wasm32"))]
                let tick = tokio::time::sleep(heartbeat_interval);
                #[cfg(target_arch = "wasm32")]
                let tick = wasmtimer::tokio::sleep(heartbeat_interval);

                tokio::select! {
                    _ = tick => {
                        if out_tx_for_ping
                            .send(SyncMessage::Ping { nonce: next_nonce })
                            .is_err()
                        {
                            return;
                        }
                        last_ping_sent_at = Some(Instant::now());
                        next_nonce = next_nonce.wrapping_add(1);

                        let now = Instant::now();
                        if now.duration_since(last_pong_at) > heartbeat_timeout {
                            let _ = inbound_tx.send(InboundEvent::Disconnected {
                                peer_id: peer_id.clone(),
                                conn_id,
                                reason: "heartbeat timeout".into(),
                            });
                            return;
                        }
                    }
                    Some(_nonce) = pong_rx.recv() => {
                        let now = Instant::now();
                        last_pong_at = now;
                        let rtt_ms = match last_ping_sent_at.take() {
                            Some(sent) => now.duration_since(sent).as_millis() as u64,
                            // Pong with no in-flight Ping (peer-initiated
                            // probe, replay, or post-disconnect race). RTT
                            // is undefined; clamp to 0 so we still update
                            // last_pong_at and surface a heartbeat.
                            None => 0,
                        };
                        let observed_at_unix_ms = web_time::SystemTime::now()
                            .duration_since(web_time::UNIX_EPOCH)
                            .map(|d| d.as_millis() as u64)
                            .unwrap_or(0);
                        let _ = inbound_tx.send(InboundEvent::PongObserved {
                            peer_id: peer_id.clone(),
                            rtt_ms,
                            observed_at_unix_ms,
                        });
                    }
                    else => return,
                }
            }
        }
    };

    // Drop the local pong_tx so the channel closes when recv_reliable_task
    // exits — otherwise the liveness_task's `pong_rx.recv()` would never
    // complete after recv exits.
    drop(pong_tx);

    tokio::join!(
        recv_reliable_task,
        recv_unreliable_task,
        send_task,
        liveness_task
    );
}

/// Which physical channel a SyncMessage flows over.
enum ChannelKind {
    Reliable,
    Unreliable,
}

fn outbound_kind(msg: &SyncMessage) -> ChannelKind {
    // Exhaustive on purpose: when a new SyncMessage variant lands,
    // the compiler MUST force a routing decision here. Don't add a
    // wildcard arm — the silent default is the wrong way to fail.
    match msg {
        SyncMessage::EphemeralDelivery { .. } => ChannelKind::Unreliable,
        SyncMessage::Hello { .. }
        | SyncMessage::EventDelivery { .. }
        | SyncMessage::BlobRequest { .. }
        | SyncMessage::BlobResponse { .. }
        | SyncMessage::DigestExchange { .. }
        | SyncMessage::Fetch { .. }
        | SyncMessage::Goodbye {}
        | SyncMessage::Ping { .. }
        | SyncMessage::Pong { .. }
        | SyncMessage::DigestRequest { .. } => ChannelKind::Reliable,
    }
}

async fn send_reliable_message<C: TransportConnection + ?Sized>(
    conn: &C,
    msg: &SyncMessage,
) -> Result<()> {
    let bytes = msg.encode()?;
    conn.send_reliable(bytes).await
}

async fn send_unreliable_message<C: TransportConnection + ?Sized>(
    conn: &C,
    msg: &SyncMessage,
) -> Result<()> {
    let bytes = msg.encode()?;
    conn.send_unreliable(bytes).await
}

async fn recv_reliable_message<C: TransportConnection + ?Sized>(conn: &C) -> Result<SyncMessage> {
    let bytes: Bytes = conn.recv_reliable().await?;
    SyncMessage::decode(&bytes)
}

async fn recv_unreliable_message<C: TransportConnection + ?Sized>(conn: &C) -> Result<SyncMessage> {
    let bytes: Bytes = conn.recv_unreliable().await?;
    SyncMessage::decode(&bytes)
}

#[cfg(all(test, feature = "test-helpers"))]
mod tests {
    use super::*;
    use crate::error::Error;
    use crate::test_transport::{TestConnection, TestNetwork};
    use crate::transport::{Transport, TransportKind};
    use crate::types::PeerAddr;
    use async_trait::async_trait;
    use bytes::Bytes;
    use sunset_store::VerifyingKey;

    fn vk(b: &[u8]) -> VerifyingKey {
        VerifyingKey::new(Bytes::copy_from_slice(b))
    }

    fn peer_addr(s: &'static str) -> PeerAddr {
        PeerAddr::new(s)
    }

    /// Build a `PeerEnv` for tests that share a `SyncConfig`'s heartbeat
    /// timing and the v1 wire protocol.
    fn peer_env_for(name: &[u8], cfg: &crate::types::SyncConfig) -> PeerEnv {
        PeerEnv {
            local_peer: PeerId(vk(name)),
            protocol_version: 1,
            heartbeat_interval: cfg.heartbeat_interval,
            heartbeat_timeout: cfg.heartbeat_timeout,
        }
    }

    /// Wraps a `TestConnection` but forces every `send_unreliable` call to
    /// return `Err`. Models a transport (e.g. WebSocket) that doesn't support
    /// unreliable datagrams.
    struct FailingUnreliableConn {
        inner: TestConnection,
    }

    #[async_trait(?Send)]
    impl TransportConnection for FailingUnreliableConn {
        async fn send_reliable(&self, bytes: Bytes) -> Result<()> {
            self.inner.send_reliable(bytes).await
        }
        async fn recv_reliable(&self) -> Result<Bytes> {
            self.inner.recv_reliable().await
        }
        async fn send_unreliable(&self, _bytes: Bytes) -> Result<()> {
            Err(Error::Transport(
                "websocket: unreliable channel unsupported".into(),
            ))
        }
        async fn recv_unreliable(&self) -> Result<Bytes> {
            self.inner.recv_unreliable().await
        }
        fn peer_id(&self) -> PeerId {
            self.inner.peer_id()
        }
        fn kind(&self) -> TransportKind {
            self.inner.kind()
        }
        async fn close(&self) -> Result<()> {
            self.inner.close().await
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn hello_exchange_succeeds() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let net = TestNetwork::new();
                let alice = net.transport(PeerId(vk(b"alice")), peer_addr("alice"));
                let bob = net.transport(PeerId(vk(b"bob")), peer_addr("bob"));
                let bob_accept =
                    crate::spawn::spawn_local(async move { bob.accept().await.unwrap() });
                let alice_conn = alice.connect(peer_addr("bob")).await.unwrap();
                let bob_conn = bob_accept.await.unwrap();

                let cfg = crate::types::SyncConfig::default();

                let (a_out_tx, a_out_rx) = mpsc::unbounded_channel::<SyncMessage>();
                let (b_out_tx, b_out_rx) = mpsc::unbounded_channel::<SyncMessage>();
                let (a_in_tx, mut a_in_rx) = mpsc::unbounded_channel::<InboundEvent>();
                let (b_in_tx, mut b_in_rx) = mpsc::unbounded_channel::<InboundEvent>();

                // Pass clones into run_peer (it takes ownership); keep originals
                // so we can drop them to trigger Goodbye.
                crate::spawn::spawn_local(run_peer(
                    Rc::new(alice_conn),
                    peer_env_for(b"alice", &cfg),
                    crate::engine::ConnectionId::for_test(1),
                    a_out_tx.clone(),
                    a_out_rx,
                    a_in_tx,
                    None,
                ));
                crate::spawn::spawn_local(run_peer(
                    Rc::new(bob_conn),
                    peer_env_for(b"bob", &cfg),
                    crate::engine::ConnectionId::for_test(2),
                    b_out_tx.clone(),
                    b_out_rx,
                    b_in_tx,
                    None,
                ));

                // Each side observes the other's Hello.
                match a_in_rx.recv().await.unwrap() {
                    InboundEvent::PeerHello { peer_id, .. } => {
                        assert_eq!(peer_id, PeerId(vk(b"bob")));
                    }
                    other => panic!("expected Hello, got {other:?}"),
                }
                match b_in_rx.recv().await.unwrap() {
                    InboundEvent::PeerHello { peer_id, .. } => {
                        assert_eq!(peer_id, PeerId(vk(b"alice")));
                    }
                    other => panic!("expected Hello, got {other:?}"),
                }

                // Drop outbound senders → channels close → send_tasks exit →
                // both peers send Goodbye and the connections shut down.
                // (run_peer passes the clone via PeerHello; the test's
                // pattern-match above dropped it implicitly, so only the
                // originals below keep the channels open.)
                drop(a_out_tx);
                drop(b_out_tx);
            })
            .await;
    }

    /// Regression test for the spec's failure-mode invariant: an unreliable
    /// send failure must NOT tear down the per-peer task. Otherwise a peer
    /// whose transport doesn't support unreliable (e.g. WS-only relay) would
    /// be disconnected the moment anyone published an ephemeral matching
    /// its filter.
    ///
    /// We wrap one side of a `TestConnection` pair in `FailingUnreliableConn`
    /// (so every `send_unreliable` returns Err), do the Hello exchange,
    /// push an `EphemeralDelivery` outbound (which would route to unreliable
    /// and fail), then push a follow-up `EventDelivery` (reliable) and
    /// confirm the peer task is still alive — i.e. the reliable message
    /// arrives on the other side.
    #[tokio::test(flavor = "current_thread")]
    async fn unreliable_send_failure_does_not_disconnect_peer() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let net = TestNetwork::new();
                let alice = net.transport(PeerId(vk(b"alice")), peer_addr("alice"));
                let bob = net.transport(PeerId(vk(b"bob")), peer_addr("bob"));
                let bob_accept =
                    crate::spawn::spawn_local(async move { bob.accept().await.unwrap() });
                let alice_conn = alice.connect(peer_addr("bob")).await.unwrap();
                let bob_conn = bob_accept.await.unwrap();

                // Wrap alice's side so every send_unreliable returns Err.
                let alice_conn = FailingUnreliableConn { inner: alice_conn };

                let cfg = crate::types::SyncConfig::default();

                let (a_out_tx, a_out_rx) = mpsc::unbounded_channel::<SyncMessage>();
                let (b_out_tx, b_out_rx) = mpsc::unbounded_channel::<SyncMessage>();
                let (a_in_tx, mut a_in_rx) = mpsc::unbounded_channel::<InboundEvent>();
                let (b_in_tx, mut b_in_rx) = mpsc::unbounded_channel::<InboundEvent>();

                crate::spawn::spawn_local(run_peer(
                    Rc::new(alice_conn),
                    peer_env_for(b"alice", &cfg),
                    crate::engine::ConnectionId::for_test(3),
                    a_out_tx.clone(),
                    a_out_rx,
                    a_in_tx,
                    None,
                ));
                crate::spawn::spawn_local(run_peer(
                    Rc::new(bob_conn),
                    peer_env_for(b"bob", &cfg),
                    crate::engine::ConnectionId::for_test(4),
                    b_out_tx.clone(),
                    b_out_rx,
                    b_in_tx,
                    None,
                ));

                // Drain the Hello on each side so the rest of the test
                // sees only the messages it pushes.
                match a_in_rx.recv().await.unwrap() {
                    InboundEvent::PeerHello { peer_id, .. } => {
                        assert_eq!(peer_id, PeerId(vk(b"bob")));
                    }
                    other => panic!("expected Hello, got {other:?}"),
                }
                match b_in_rx.recv().await.unwrap() {
                    InboundEvent::PeerHello { peer_id, .. } => {
                        assert_eq!(peer_id, PeerId(vk(b"alice")));
                    }
                    other => panic!("expected Hello, got {other:?}"),
                }

                // Push an EphemeralDelivery from alice → bob. Routing sends
                // it on alice's unreliable channel, where send_unreliable
                // returns Err. The OLD code would `break` the send_task;
                // the NEW code drops the datagram silently.
                let ephemeral = SyncMessage::EphemeralDelivery {
                    datagram: sunset_store::SignedDatagram {
                        verifying_key: vk(b"alice"),
                        name: Bytes::from_static(b"room/voice/alice/0"),
                        payload: Bytes::from_static(b"opus-frame"),
                        signature: Bytes::from_static(&[0xab; 64]),
                    },
                };
                a_out_tx.send(ephemeral).unwrap();

                // Now push a reliable follow-up. If the per-peer task is
                // still alive, bob will receive it. If the unreliable
                // failure broke the send_task (the bug), this message
                // never arrives and the recv times out / errors.
                let followup = SyncMessage::EventDelivery {
                    entries: vec![],
                    blobs: vec![],
                };
                a_out_tx.send(followup.clone()).unwrap();

                // Read events on bob's side. We tolerate spurious recv
                // events (e.g. an unreliable arriving — though it
                // shouldn't, since send_unreliable failed); the assertion
                // is that we eventually see the EventDelivery.
                let mut saw_event_delivery = false;
                for _ in 0..4 {
                    match tokio::time::timeout(
                        std::time::Duration::from_secs(2),
                        b_in_rx.recv(),
                    )
                    .await
                    .expect("bob should still be receiving — peer task must not have died")
                    {
                        Some(InboundEvent::Message {
                            message: SyncMessage::EventDelivery { .. },
                            ..
                        }) => {
                            saw_event_delivery = true;
                            break;
                        }
                        Some(_) => continue,
                        None => panic!("bob's inbound channel closed prematurely"),
                    }
                }
                assert!(
                    saw_event_delivery,
                    "reliable follow-up did not arrive — per-peer send_task likely died on the prior unreliable send failure"
                );

                drop(a_out_tx);
                drop(b_out_tx);
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn heartbeat_keeps_connection_alive_under_normal_traffic() {
        use crate::types::SyncConfig;
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let net = TestNetwork::new();
                let alice = net.transport(PeerId(vk(b"alice")), peer_addr("alice"));
                let bob = net.transport(PeerId(vk(b"bob")), peer_addr("bob"));
                let bob_accept =
                    crate::spawn::spawn_local(async move { bob.accept().await.unwrap() });
                let alice_conn = alice.connect(peer_addr("bob")).await.unwrap();
                let bob_conn = bob_accept.await.unwrap();

                let cfg = SyncConfig::default();

                let (a_out_tx, a_out_rx) = mpsc::unbounded_channel::<SyncMessage>();
                let (b_out_tx, b_out_rx) = mpsc::unbounded_channel::<SyncMessage>();
                let (a_in_tx, mut a_in_rx) = mpsc::unbounded_channel::<InboundEvent>();
                let (b_in_tx, mut b_in_rx) = mpsc::unbounded_channel::<InboundEvent>();

                crate::spawn::spawn_local(run_peer(
                    Rc::new(alice_conn),
                    peer_env_for(b"alice", &cfg),
                    crate::engine::ConnectionId::for_test(1),
                    a_out_tx.clone(),
                    a_out_rx,
                    a_in_tx,
                    None,
                ));
                crate::spawn::spawn_local(run_peer(
                    Rc::new(bob_conn),
                    peer_env_for(b"bob", &cfg),
                    crate::engine::ConnectionId::for_test(2),
                    b_out_tx.clone(),
                    b_out_rx,
                    b_in_tx,
                    None,
                ));

                // Drain Hellos.
                let _ = a_in_rx.recv().await.unwrap();
                let _ = b_in_rx.recv().await.unwrap();

                // Advance time across 5 ping intervals, one interval at a
                // time, yielding between each so the Ping/Pong round-trip
                // can complete before the next tick fires. Under
                // `tokio::time::pause`, advancing by N*interval in a single
                // call jumps `Instant::now()` past the heartbeat deadline
                // before any Pong has had a chance to update
                // `last_pong_at`, which would spuriously trip the timeout.
                for _ in 0..5 {
                    tokio::time::advance(cfg.heartbeat_interval).await;
                    // Yield several times so the round-trip
                    // (Ping → send_task → wire → bob recv → Pong →
                    // bob send_task → wire → alice recv → pong_tx) can
                    // make full progress before the next interval ticks.
                    for _ in 0..16 {
                        tokio::task::yield_now().await;
                    }
                }

                let got =
                    tokio::time::timeout(std::time::Duration::from_millis(10), a_in_rx.recv())
                        .await;
                match got {
                    Ok(Some(InboundEvent::Disconnected { reason, .. })) => {
                        panic!("unexpected disconnect: {reason}");
                    }
                    _ => { /* good */ }
                }

                drop(a_out_tx);
                drop(b_out_tx);
            })
            .await;
    }

    /// Wraps a `TestConnection` and silently swallows every `SyncMessage::Pong`
    /// the host tries to send. Used to simulate a peer whose pongs never
    /// reach the wire (or arrive at us).
    struct DropPongsConn {
        inner: TestConnection,
    }

    #[async_trait(?Send)]
    impl TransportConnection for DropPongsConn {
        async fn send_reliable(&self, bytes: Bytes) -> Result<()> {
            // Decode; if Pong, drop. Otherwise forward.
            if let Ok(SyncMessage::Pong { .. }) = SyncMessage::decode(&bytes) {
                return Ok(());
            }
            self.inner.send_reliable(bytes).await
        }
        async fn recv_reliable(&self) -> Result<Bytes> {
            self.inner.recv_reliable().await
        }
        async fn send_unreliable(&self, bytes: Bytes) -> Result<()> {
            self.inner.send_unreliable(bytes).await
        }
        async fn recv_unreliable(&self) -> Result<Bytes> {
            self.inner.recv_unreliable().await
        }
        fn peer_id(&self) -> PeerId {
            self.inner.peer_id()
        }
        fn kind(&self) -> crate::transport::TransportKind {
            self.inner.kind()
        }
        async fn close(&self) -> Result<()> {
            self.inner.close().await
        }
    }

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn heartbeat_timeout_emits_disconnected() {
        use crate::types::SyncConfig;
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let net = TestNetwork::new();
                let alice = net.transport(PeerId(vk(b"alice")), peer_addr("alice"));
                let bob = net.transport(PeerId(vk(b"bob")), peer_addr("bob"));
                let bob_accept =
                    crate::spawn::spawn_local(async move { bob.accept().await.unwrap() });
                let alice_conn = alice.connect(peer_addr("bob")).await.unwrap();
                let bob_conn = bob_accept.await.unwrap();

                // Wrap bob's side so its outbound Pongs are dropped: alice
                // never gets pongs, alice times out.
                let bob_conn = DropPongsConn { inner: bob_conn };

                let cfg = SyncConfig::default();

                let (a_out_tx, a_out_rx) = mpsc::unbounded_channel::<SyncMessage>();
                let (b_out_tx, b_out_rx) = mpsc::unbounded_channel::<SyncMessage>();
                let (a_in_tx, mut a_in_rx) = mpsc::unbounded_channel::<InboundEvent>();
                let (b_in_tx, mut b_in_rx) = mpsc::unbounded_channel::<InboundEvent>();

                crate::spawn::spawn_local(run_peer(
                    Rc::new(alice_conn),
                    peer_env_for(b"alice", &cfg),
                    crate::engine::ConnectionId::for_test(1),
                    a_out_tx.clone(),
                    a_out_rx,
                    a_in_tx,
                    None,
                ));
                crate::spawn::spawn_local(run_peer(
                    Rc::new(bob_conn),
                    peer_env_for(b"bob", &cfg),
                    crate::engine::ConnectionId::for_test(2),
                    b_out_tx.clone(),
                    b_out_rx,
                    b_in_tx,
                    None,
                ));

                // Drain Hellos.
                let _ = a_in_rx.recv().await.unwrap();
                let _ = b_in_rx.recv().await.unwrap();

                // Advance well past heartbeat_timeout.
                tokio::time::advance(cfg.heartbeat_timeout * 2).await;
                tokio::task::yield_now().await;

                // Alice should observe a heartbeat timeout disconnect.
                loop {
                    match a_in_rx.recv().await {
                        Some(InboundEvent::Disconnected { reason, .. })
                            if reason.contains("heartbeat timeout") =>
                        {
                            break;
                        }
                        Some(InboundEvent::Disconnected { reason, .. }) => {
                            // Could also fail via send error after channel
                            // drops; either is acceptable for this test.
                            assert!(
                                reason.contains("send reliable")
                                    || reason.contains("recv reliable"),
                                "unexpected disconnect reason: {reason}"
                            );
                            break;
                        }
                        Some(_) => continue,
                        None => panic!("inbound channel closed before disconnect"),
                    }
                }

                drop(a_out_tx);
                drop(b_out_tx);
            })
            .await;
    }

    /// Wraps a `TestConnection` and starts returning Err from
    /// `send_reliable` after a flag is flipped. Used to simulate a
    /// transport that detects an OS-level closed socket on the next
    /// write attempt.
    struct PoisonableSendConn {
        inner: TestConnection,
        poisoned: Rc<std::cell::RefCell<bool>>,
    }

    #[async_trait(?Send)]
    impl TransportConnection for PoisonableSendConn {
        async fn send_reliable(&self, bytes: Bytes) -> Result<()> {
            if *self.poisoned.borrow() {
                return Err(Error::Transport("simulated close".into()));
            }
            self.inner.send_reliable(bytes).await
        }
        async fn recv_reliable(&self) -> Result<Bytes> {
            self.inner.recv_reliable().await
        }
        async fn send_unreliable(&self, bytes: Bytes) -> Result<()> {
            self.inner.send_unreliable(bytes).await
        }
        async fn recv_unreliable(&self) -> Result<Bytes> {
            self.inner.recv_unreliable().await
        }
        fn peer_id(&self) -> PeerId {
            self.inner.peer_id()
        }
        fn kind(&self) -> crate::transport::TransportKind {
            self.inner.kind()
        }
        async fn close(&self) -> Result<()> {
            self.inner.close().await
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn liveness_emits_pong_observed_with_rtt() {
        use crate::message::SyncMessage;
        use std::time::Duration;
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let net = TestNetwork::new();
                let alice = net.transport(PeerId(vk(b"alice")), peer_addr("alice"));
                let bob = net.transport(PeerId(vk(b"bob")), peer_addr("bob"));
                let bob_accept =
                    crate::spawn::spawn_local(async move { bob.accept().await.unwrap() });
                let alice_conn = alice.connect(peer_addr("bob")).await.unwrap();
                let bob_conn = bob_accept.await.unwrap();

                // Tight heartbeat so the test doesn't sit on the default 15 s.
                let cfg = crate::types::SyncConfig {
                    heartbeat_interval: Duration::from_millis(20),
                    ..crate::types::SyncConfig::default()
                };

                let (a_out_tx, a_out_rx) = mpsc::unbounded_channel::<SyncMessage>();
                let (a_in_tx, mut a_in_rx) = mpsc::unbounded_channel::<InboundEvent>();
                crate::spawn::spawn_local(run_peer(
                    Rc::new(alice_conn),
                    peer_env_for(b"alice", &cfg),
                    crate::engine::ConnectionId::for_test(101),
                    a_out_tx.clone(),
                    a_out_rx,
                    a_in_tx,
                    None,
                ));

                // Bob side, driven manually:
                // 1) Receive alice's Hello.
                let alice_hello = super::recv_reliable_message(&bob_conn).await.unwrap();
                assert!(matches!(alice_hello, SyncMessage::Hello { .. }));
                // 2) Respond with our Hello so alice's run_peer enters the main loop.
                super::send_reliable_message(
                    &bob_conn,
                    &SyncMessage::Hello {
                        protocol_version: 1,
                        peer_id: PeerId(vk(b"bob")),
                    },
                )
                .await
                .unwrap();
                // 3) Drain alice's PeerHello inbound event (so subsequent
                // recv() can land on PongObserved without confusion).
                match tokio::time::timeout(Duration::from_millis(200), a_in_rx.recv())
                    .await
                    .expect("no PeerHello")
                    .expect("inbound channel closed")
                {
                    InboundEvent::PeerHello { .. } => {}
                    other => panic!("expected PeerHello, got {other:?}"),
                }
                // 4) Wait for alice's first Ping.
                let ping_nonce = loop {
                    let msg = super::recv_reliable_message(&bob_conn).await.unwrap();
                    match msg {
                        SyncMessage::Ping { nonce } => break nonce,
                        // Tolerate any other reliable traffic alice might send first.
                        _ => continue,
                    }
                };
                // 5) Reply Pong.
                super::send_reliable_message(&bob_conn, &SyncMessage::Pong { nonce: ping_nonce })
                    .await
                    .unwrap();
                // 6) Expect PongObserved on alice's inbound, within reasonable time.
                let mut found = None;
                for _ in 0..50 {
                    match tokio::time::timeout(Duration::from_millis(50), a_in_rx.recv()).await {
                        Ok(Some(InboundEvent::PongObserved {
                            peer_id,
                            rtt_ms,
                            observed_at_unix_ms,
                        })) => {
                            assert_eq!(peer_id, PeerId(vk(b"bob")));
                            // observed_at_unix_ms is wall-clock; non-zero in any
                            // realistic test environment.
                            assert!(observed_at_unix_ms > 0, "observed_at_unix_ms should be set");
                            found = Some(rtt_ms);
                            break;
                        }
                        Ok(Some(_)) => continue, // ignore other inbound events
                        _ => break,
                    }
                }
                let rtt = found.expect("no PongObserved seen");
                // RTT under wall-clock time should be small for an in-process
                // TestConnection. We don't assert non-zero (rtt_ms could
                // legitimately round to 0 ms on fast machines) — just bounded.
                assert!(rtt < 5_000, "rtt should be small; got {rtt} ms");

                drop(a_out_tx);
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn send_reliable_failure_emits_disconnected_with_conn_id() {
        use crate::types::SyncConfig;
        use std::cell::RefCell;
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let net = TestNetwork::new();
                let alice = net.transport(PeerId(vk(b"alice")), peer_addr("alice"));
                let bob = net.transport(PeerId(vk(b"bob")), peer_addr("bob"));
                let bob_accept =
                    crate::spawn::spawn_local(async move { bob.accept().await.unwrap() });
                let alice_conn_inner = alice.connect(peer_addr("bob")).await.unwrap();
                let bob_conn = bob_accept.await.unwrap();

                let poisoned = Rc::new(RefCell::new(false));
                let alice_conn = PoisonableSendConn {
                    inner: alice_conn_inner,
                    poisoned: poisoned.clone(),
                };

                let cfg = SyncConfig::default();

                let (a_out_tx, a_out_rx) = mpsc::unbounded_channel::<SyncMessage>();
                let (b_out_tx, b_out_rx) = mpsc::unbounded_channel::<SyncMessage>();
                let (a_in_tx, mut a_in_rx) = mpsc::unbounded_channel::<InboundEvent>();
                let (b_in_tx, _b_in_rx) = mpsc::unbounded_channel::<InboundEvent>();

                let alice_conn_id = crate::engine::ConnectionId::for_test(42);

                crate::spawn::spawn_local(run_peer(
                    Rc::new(alice_conn),
                    peer_env_for(b"alice", &cfg),
                    alice_conn_id,
                    a_out_tx.clone(),
                    a_out_rx,
                    a_in_tx,
                    None,
                ));
                crate::spawn::spawn_local(run_peer(
                    Rc::new(bob_conn),
                    peer_env_for(b"bob", &cfg),
                    crate::engine::ConnectionId::for_test(43),
                    b_out_tx.clone(),
                    b_out_rx,
                    b_in_tx,
                    None,
                ));

                // Drain Hello.
                let _ = a_in_rx.recv().await.unwrap();

                // Poison alice's send_reliable: next reliable send fails.
                *poisoned.borrow_mut() = true;

                // Send a reliable message — alice's send_task will fail.
                a_out_tx
                    .send(SyncMessage::EventDelivery {
                        entries: vec![],
                        blobs: vec![],
                    })
                    .unwrap();

                // Expect Disconnected with reason starting with "send reliable"
                // and matching conn_id, well before heartbeat_timeout elapses.
                tokio::task::yield_now().await;

                let got = tokio::time::timeout(cfg.heartbeat_timeout / 4, async {
                    loop {
                        match a_in_rx.recv().await {
                            Some(InboundEvent::Disconnected {
                                conn_id, reason, ..
                            }) => {
                                return (conn_id, reason);
                            }
                            Some(_) => continue,
                            None => panic!("inbound channel closed"),
                        }
                    }
                })
                .await
                .expect("disconnect should arrive before heartbeat timeout");

                assert_eq!(got.0, alice_conn_id);
                assert!(
                    got.1.contains("send reliable"),
                    "expected send-side reason, got {}",
                    got.1
                );

                drop(a_out_tx);
                drop(b_out_tx);
            })
            .await;
    }
}
