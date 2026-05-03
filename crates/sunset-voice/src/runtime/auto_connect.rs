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
