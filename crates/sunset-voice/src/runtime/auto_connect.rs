//! Auto-connect FSM: per-peer Unknown → Dialing on first voice-presence
//! observation; Gone via membership_liveness Stale → back to Unknown.
//!
//! The dial trigger comes from durable `voice-presence/<room_fp>/<sender>`
//! entries published by remote peers before WebRTC is established. This
//! avoids the bootstrap chicken-and-egg where heartbeats (ephemeral) can't
//! reach peers we have no WebRTC connection to yet.
//!
//! Once WebRTC is up, heartbeats flow through `membership_liveness`. Its
//! Stale events are still used here for Gone/cleanup once the connection
//! has been established — so the FSM consumes from both streams.
//!
//! ## Glare avoidance
//!
//! In a 2-peer auto-connect, both sides see each other's voice-presence
//! and would call `connect_direct` simultaneously. The browser WebRTC
//! transport handles glare by ignoring duplicate Offers from a peer it's
//! already mid-handshake with — but that drops the *initiator-side*
//! Offer too, so each side ends up with one connect-side handshake
//! waiting for an Answer that's been suppressed and one accept-side
//! handshake from the peer's Offer. The two independently-derived
//! RTCPeerConnections then race ICE/SCTP setup against each other and
//! neither completes within the test/UX budget.
//!
//! We avoid the collision by asymmetry: only the peer with the
//! lexicographically smaller public key initiates the dial; the other
//! waits for its accept-side handshake to complete. This is the
//! cheapest possible tiebreak — no negotiation rounds, no clocks, no
//! state. A future Perfect Negotiation implementation could replace
//! this with proper rollback semantics.

use std::rc::Weak;

use bytes::Bytes;
use futures::{FutureExt, StreamExt};

use sunset_core::bus::BusEvent;
use sunset_core::liveness::LivenessState;
use sunset_store::VerifyingKey;
use sunset_sync::PeerId;

use super::state::{AutoConnectState, RuntimeInner};
use super::voice_presence_prefix;

pub(crate) fn spawn(weak: Weak<RuntimeInner>) -> futures::future::LocalBoxFuture<'static, ()> {
    async move {
        let Some(inner) = weak.upgrade() else {
            return;
        };
        let room_fp = inner.room.fingerprint().to_hex();
        let prefix = voice_presence_prefix(&room_fp);
        let self_pk = inner.identity.store_verifying_key();
        let bus = inner.bus.clone();
        let membership_arc = inner.membership_liveness.clone();
        drop(inner);

        let mut presence_stream = match bus.subscribe_prefix(prefix.clone()).await {
            Ok(s) => s,
            Err(e) => {
                tracing::error!(error = %e, "voice-presence subscribe failed");
                return;
            }
        };
        let mut life_sub = membership_arc.subscribe().await;

        loop {
            tokio::select! {
                Some(ev) = presence_stream.next() => {
                    let entry = match ev {
                        BusEvent::Durable { entry, .. } => entry,
                        BusEvent::Ephemeral(_) => continue,
                    };
                    // Skip self.
                    if entry.verifying_key == self_pk {
                        continue;
                    }
                    // Glare avoidance: only the side with the
                    // lexicographically smaller public key initiates
                    // the WebRTC dial; the other side accepts via the
                    // accept path. Without this, both peers' WebRTC
                    // `connect()` futures race, each creating an
                    // RTCPeerConnection that the other side answers
                    // through the accept worker — the two
                    // independently-derived connections collide on
                    // ICE/SCTP setup and neither completes within the
                    // test budget. Asymmetry breaks the tie cheaply
                    // without needing a full Perfect Negotiation
                    // implementation.
                    if self_pk.as_bytes() >= entry.verifying_key.as_bytes() {
                        continue;
                    }
                    // Extract peer PeerId from verifying_key.
                    let peer = PeerId(entry.verifying_key.clone());
                    // Also verify the name segment matches (belt-and-suspenders).
                    if !is_valid_presence_name(&entry.name, &prefix, &entry.verifying_key) {
                        continue;
                    }

                    let dialer_to_call = {
                        let Some(inner) = weak.upgrade() else { return; };
                        let mut state = inner.auto_connect_state.borrow_mut();
                        let slot = state.entry(peer.clone()).or_insert(AutoConnectState::Unknown);
                        if *slot == AutoConnectState::Unknown {
                            *slot = AutoConnectState::Dialing;
                            Some(inner.dialer.clone())
                        } else {
                            None
                        }
                    };
                    if let Some(dialer) = dialer_to_call {
                        dialer.ensure_direct(peer).await;
                    }
                }
                Some(ev) = life_sub.next() => {
                    if ev.state == LivenessState::Stale {
                        let Some(inner) = weak.upgrade() else { return; };
                        let mut state = inner.auto_connect_state.borrow_mut();
                        state.insert(ev.peer.clone(), AutoConnectState::Unknown);
                        drop(state);
                        // Drop per-peer playback resources.
                        inner.frame_sink.borrow().drop_peer(&ev.peer);
                        // Drop per-peer jitter buffer so re-entry starts fresh.
                        inner.jitter.borrow_mut().remove(&ev.peer);
                        inner.last_delivered.borrow_mut().remove(&ev.peer);
                    }
                }
                else => return,
            }
        }
    }
    .boxed_local()
}

/// Check that the entry name is `voice-presence/<prefix_room_fp>/<pk_hex>`
/// where `<pk_hex>` hex-encodes the entry's own `verifying_key`.
fn is_valid_presence_name(name: &Bytes, prefix: &Bytes, vk: &VerifyingKey) -> bool {
    if !name.starts_with(prefix.as_ref()) {
        return false;
    }
    let suffix = &name[prefix.len()..];
    let expected = hex::encode(vk.as_bytes());
    suffix == expected.as_bytes()
}
