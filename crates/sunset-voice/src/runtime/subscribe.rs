//! Subscribe loop: opens a Bus subscription with prefix `voice/<fp>/`,
//! decrypts each `EncryptedVoicePacket`, dispatches by enum:
//! - `Frame` → feed `frame_liveness` + push decoded PCM to per-peer
//!   jitter buffer.
//! - `Heartbeat` → feed `membership_liveness` + record `is_muted` so
//!   the combiner can emit it.

use std::rc::Weak;
use std::time::SystemTime;

use bytes::Bytes;
use futures::{FutureExt, StreamExt};

use sunset_core::bus::BusEvent;
use sunset_core::identity::IdentityKey;
use sunset_sync::PeerId;

use super::{JITTER_MAX_DEPTH, state::RuntimeInner};
use crate::packet::{EncryptedVoicePacket, VoicePacket, decrypt};
use crate::runtime::traits::VoicePeerState;

pub(crate) fn spawn(weak: Weak<RuntimeInner>) -> futures::future::LocalBoxFuture<'static, ()> {
    async move {
        let Some(inner) = weak.upgrade() else {
            return;
        };
        let room_fp = inner.room.fingerprint().to_hex();
        let prefix = Bytes::from(format!("voice/{room_fp}/"));
        let bus = inner.bus.clone();
        let self_pk = inner.identity.store_verifying_key();
        drop(inner);

        let mut stream = match bus.subscribe_voice_prefix(prefix).await {
            Ok(s) => s,
            Err(e) => {
                tracing::error!(error = %e, "subscribe failed");
                return;
            }
        };
        let mut decoder = match crate::VoiceDecoder::new() {
            Ok(d) => d,
            Err(e) => {
                tracing::error!(error = %e, "decoder init failed");
                return;
            }
        };

        while let Some(ev) = stream.next().await {
            let Some(inner) = weak.upgrade() else {
                return;
            };
            let datagram = match ev {
                BusEvent::Ephemeral(d) => d,
                BusEvent::Durable { .. } => continue,
            };
            if datagram.verifying_key == self_pk {
                continue;
            }
            let peer = PeerId(datagram.verifying_key.clone());
            let sender = match IdentityKey::from_store_verifying_key(&datagram.verifying_key) {
                Ok(s) => s,
                Err(_) => continue,
            };
            let evp: EncryptedVoicePacket = match postcard::from_bytes(&datagram.payload) {
                Ok(e) => e,
                Err(_) => continue,
            };
            let packet = match decrypt(&inner.room, 0, &sender, &evp) {
                Ok(p) => p,
                Err(e) => {
                    tracing::warn!(error = %e, "decrypt failed");
                    continue;
                }
            };
            match packet {
                VoicePacket::Frame {
                    payload,
                    sender_time_ms,
                    ..
                } => {
                    let st =
                        SystemTime::UNIX_EPOCH + std::time::Duration::from_millis(sender_time_ms);
                    inner.frame_liveness.observe(peer.clone(), st).await;
                    match decoder.decode(&payload) {
                        Ok(pcm) => {
                            let mut jitter = inner.jitter.borrow_mut();
                            let q = jitter.entry(peer).or_default();
                            q.push_back(pcm);
                            // Cap at JITTER_MAX_DEPTH.
                            while q.len() > JITTER_MAX_DEPTH {
                                q.pop_front();
                            }
                        }
                        Err(e) => tracing::warn!(error = %e, "decode failed"),
                    }
                }
                VoicePacket::Heartbeat {
                    sent_at_ms,
                    is_muted,
                } => {
                    let st = SystemTime::UNIX_EPOCH + std::time::Duration::from_millis(sent_at_ms);
                    inner.membership_liveness.observe(peer.clone(), st).await;

                    // Notify auto-connect task.
                    let _ = inner.auto_connect_chan.tx.send(peer.clone());

                    // Emit immediately on mute change.
                    if inner.last_emitted_set_muted_seen(peer.clone(), is_muted) {
                        let entry = *inner.last_emitted.borrow().get(&peer).unwrap();
                        let state = VoicePeerState {
                            peer: peer.clone(),
                            in_call: entry.in_call,
                            talking: entry.talking,
                            is_muted: entry.is_muted,
                        };
                        inner.peer_state_sink.emit(&state);
                    }
                }
            }
        }
    }
    .boxed_local()
}
