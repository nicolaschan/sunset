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
        kind: crate::transport::TransportKind,
        out_tx: tokio::sync::mpsc::UnboundedSender<SyncMessage>,
    },
    /// A SyncMessage arrived (other than Hello).
    Message { from: PeerId, message: SyncMessage },
    /// The peer's connection closed (graceful or error).
    Disconnected { peer_id: PeerId, reason: String },
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
                    reason: format!(
                        "protocol version mismatch: ours {} theirs {}",
                        local_protocol_version, protocol_version
                    ),
                });
                return;
            }
            let _ = inbound_tx.send(InboundEvent::PeerHello {
                peer_id: peer_id.clone(),
                kind: local_kind,
                out_tx,
            });
            peer_id
        }
        Ok(other) => {
            let _ = inbound_tx.send(InboundEvent::Disconnected {
                peer_id: conn.peer_id(),
                reason: format!("expected Hello, got {:?}", other),
            });
            return;
        }
        Err(e) => {
            let _ = inbound_tx.send(InboundEvent::Disconnected {
                peer_id: conn.peer_id(),
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
                let result = match outbound_kind(&msg) {
                    ChannelKind::Reliable => send_reliable_message(&*conn, &msg).await,
                    ChannelKind::Unreliable => send_unreliable_message(&*conn, &msg).await,
                };
                if result.is_err() {
                    break;
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
    match msg {
        SyncMessage::EphemeralDelivery { .. } => ChannelKind::Unreliable,
        _ => ChannelKind::Reliable,
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
    use crate::test_transport::TestNetwork;
    use crate::transport::Transport;
    use crate::types::PeerAddr;
    use bytes::Bytes;
    use sunset_store::VerifyingKey;

    fn vk(b: &[u8]) -> VerifyingKey {
        VerifyingKey::new(Bytes::copy_from_slice(b))
    }

    fn peer_addr(s: &'static str) -> PeerAddr {
        PeerAddr::new(s)
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
                    a_out_tx.clone(),
                    a_out_rx,
                    a_in_tx,
                ));
                crate::spawn::spawn_local(run_peer(
                    Rc::new(bob_conn),
                    PeerId(vk(b"bob")),
                    1,
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
}
