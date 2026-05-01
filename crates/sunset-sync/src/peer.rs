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
/// `local_protocol_version` is `SyncConfig::protocol_version`.
/// `local_peer` is the engine's local PeerId.
/// `out_tx` is the outbound sender — passed through `PeerHello` so the engine
/// can register it under the Hello-declared peer_id (not the transport-level
/// peer_id, which may differ in schemes that separate routing identity from
/// application identity, e.g. X25519 routing + Ed25519 application keys).
pub(crate) async fn run_peer<C: TransportConnection + 'static>(
    conn: Rc<C>,
    local_peer: PeerId,
    local_protocol_version: u32,
    conn_id: crate::engine::ConnectionId,
    out_tx: mpsc::UnboundedSender<SyncMessage>,
    mut outbound_rx: mpsc::UnboundedReceiver<SyncMessage>,
    inbound_tx: mpsc::UnboundedSender<InboundEvent>,
) {
    let local_kind = conn.kind();
    // Send our Hello.
    let our_hello = SyncMessage::Hello {
        protocol_version: local_protocol_version,
        peer_id: local_peer.clone(),
    };
    if let Err(e) = send_reliable_message(&*conn, &our_hello).await {
        let _ = inbound_tx.send(InboundEvent::Disconnected {
            peer_id: conn.peer_id(),
            conn_id,
            reason: format!("send hello: {e}"),
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
                let _ = inbound_tx.send(InboundEvent::Disconnected {
                    peer_id,
                    conn_id,
                    reason: format!(
                        "protocol version mismatch: ours {} theirs {}",
                        local_protocol_version, protocol_version
                    ),
                });
                return;
            }
            let _ = inbound_tx.send(InboundEvent::PeerHello {
                peer_id: peer_id.clone(),
                conn_id,
                kind: local_kind,
                out_tx,
            });
            peer_id
        }
        Ok(other) => {
            let _ = inbound_tx.send(InboundEvent::Disconnected {
                peer_id: conn.peer_id(),
                conn_id,
                reason: format!("expected Hello, got {:?}", other),
            });
            return;
        }
        Err(e) => {
            let _ = inbound_tx.send(InboundEvent::Disconnected {
                peer_id: conn.peer_id(),
                conn_id,
                reason: format!("recv hello: {e}"),
            });
            return;
        }
    };

    // Concurrent recv loops — reliable and unreliable channels are
    // independent; each drains its own physical channel and routes the
    // decoded SyncMessage into the same `inbound_tx`. The engine's
    // dispatch is channel-agnostic; only the per-peer task knows which
    // wire carried the message.
    let recv_reliable_task = {
        let conn = conn.clone();
        let inbound_tx = inbound_tx.clone();
        let peer_id = peer_id.clone();
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
        async move {
            while let Some(msg) = outbound_rx.recv().await {
                match outbound_kind(&msg) {
                    ChannelKind::Reliable => {
                        // Reliable failures indicate a real disconnect; tear down.
                        if send_reliable_message(&*conn, &msg).await.is_err() {
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

    tokio::join!(recv_reliable_task, recv_unreliable_task, send_task);
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
        | SyncMessage::Pong { .. } => ChannelKind::Reliable,
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

                let (a_out_tx, a_out_rx) = mpsc::unbounded_channel::<SyncMessage>();
                let (b_out_tx, b_out_rx) = mpsc::unbounded_channel::<SyncMessage>();
                let (a_in_tx, mut a_in_rx) = mpsc::unbounded_channel::<InboundEvent>();
                let (b_in_tx, mut b_in_rx) = mpsc::unbounded_channel::<InboundEvent>();

                // Pass clones into run_peer (it takes ownership); keep originals
                // so we can drop them to trigger Goodbye.
                crate::spawn::spawn_local(run_peer(
                    Rc::new(alice_conn),
                    PeerId(vk(b"alice")),
                    1,
                    crate::engine::ConnectionId::for_test(1),
                    a_out_tx.clone(),
                    a_out_rx,
                    a_in_tx,
                ));
                crate::spawn::spawn_local(run_peer(
                    Rc::new(bob_conn),
                    PeerId(vk(b"bob")),
                    1,
                    crate::engine::ConnectionId::for_test(2),
                    b_out_tx.clone(),
                    b_out_rx,
                    b_in_tx,
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

                let (a_out_tx, a_out_rx) = mpsc::unbounded_channel::<SyncMessage>();
                let (b_out_tx, b_out_rx) = mpsc::unbounded_channel::<SyncMessage>();
                let (a_in_tx, mut a_in_rx) = mpsc::unbounded_channel::<InboundEvent>();
                let (b_in_tx, mut b_in_rx) = mpsc::unbounded_channel::<InboundEvent>();

                crate::spawn::spawn_local(run_peer(
                    Rc::new(alice_conn),
                    PeerId(vk(b"alice")),
                    1,
                    crate::engine::ConnectionId::for_test(3),
                    a_out_tx.clone(),
                    a_out_rx,
                    a_in_tx,
                ));
                crate::spawn::spawn_local(run_peer(
                    Rc::new(bob_conn),
                    PeerId(vk(b"bob")),
                    1,
                    crate::engine::ConnectionId::for_test(4),
                    b_out_tx.clone(),
                    b_out_rx,
                    b_in_tx,
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
}
