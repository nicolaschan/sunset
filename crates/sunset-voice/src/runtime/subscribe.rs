//! Subscribe loop: opens a Bus subscription with prefix `voice/<fp>/`,
//! decrypts each `EncryptedVoicePacket`, dispatches by enum:
//! - `Frame` → feed `frame_liveness` + decode/denoise + deliver
//!   directly to the `FrameSink`. No intermediate jitter buffer:
//!   the host (e.g. the browser playback worklet) absorbs network
//!   jitter at the audio clock. When `deafened` is set, skip the
//!   decode entirely.
//! - `Heartbeat` → feed `membership_liveness` + record `is_muted` so
//!   the combiner can emit it.

use std::rc::Weak;
use std::time::SystemTime;

use bytes::Bytes;
use futures::{FutureExt, StreamExt};

use sunset_core::bus::BusEvent;
use sunset_core::identity::IdentityKey;
use sunset_sync::PeerId;

use super::state::RuntimeInner;
use crate::packet::{EncryptedVoicePacket, VoicePacket, decrypt};

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
            // The authoritative per-stream seq lives on the envelope, not
            // inside the encrypted packet.
            let envelope_seq = datagram.seq;
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
                    // Receiver-side dedup on the envelope seq. During a
                    // direct/relay switchover a receiver briefly gets the
                    // same frame both ways; deliver it once. The gate uses
                    // an `Option` keyed on the sender — seq 0 is a real
                    // first value, so an absent entry must NOT be treated
                    // as seq 0 (no `unwrap_or(0)`). The decoder is
                    // stateful (Opus predictor history), so a duplicate
                    // must be dropped before decode, not just before the
                    // sink.
                    {
                        let mut last = inner.last_delivered_seq.borrow_mut();
                        match last.get(&peer) {
                            Some(&h) if envelope_seq <= h => continue,
                            _ => {
                                last.insert(peer.clone(), envelope_seq);
                            }
                        }
                    }
                    // Deafened: skip decode + delivery. We still feed
                    // `frame_liveness` above so the combiner can keep
                    // the peer's `talking`/`in_call` state honest while
                    // we're not listening.
                    if *inner.deafened.borrow() {
                        continue;
                    }
                    // Decode through the per-peer Opus decoder. One
                    // decoder per peer is mandatory: libopus's
                    // predictor history, SILK state, and CELT pitch
                    // tracking all assume a single continuous stream,
                    // so a shared decoder would corrupt every frame
                    // whenever the active sender changes. Lazily
                    // construct on first frame; an init failure for
                    // this peer drops their packet but keeps the
                    // subscribe loop alive for everyone else.
                    let decoded = {
                        let mut decoders = inner.decoders.borrow_mut();
                        let decoder = match decoders.entry(peer.clone()) {
                            std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
                            std::collections::hash_map::Entry::Vacant(e) => {
                                match crate::VoiceDecoder::new() {
                                    Ok(d) => e.insert(d),
                                    Err(err) => {
                                        tracing::warn!(error = %err, "decoder init failed");
                                        continue;
                                    }
                                }
                            }
                        };
                        decoder.decode(&payload)
                    };
                    match decoded {
                        Ok(mut pcm) => {
                            // Denoise per peer unless the local user
                            // has toggled this peer off in their
                            // popover. Each peer owns a stateful
                            // `Denoiser` so RNNoise's predictor is
                            // never crossed between sources. On size
                            // mismatch the bug is in the decoder, so
                            // surface it but still deliver the frame.
                            if !inner.denoise_disabled.borrow().contains(&peer) {
                                let mut denoisers = inner.denoisers.borrow_mut();
                                let d = denoisers
                                    .entry(peer.clone())
                                    .or_insert_with(crate::Denoiser::start);
                                if let Err(e) = d.denoise_in_place(&mut pcm) {
                                    tracing::warn!(error = %e, "denoise skipped");
                                }
                            }
                            // The low 32 bits of the envelope seq are
                            // passed to the sink for sequence-indexed
                            // buffering downstream. The per-peer HWM was
                            // already advanced by the dedup gate above.
                            inner
                                .frame_sink
                                .borrow()
                                .deliver(&peer, envelope_seq as u32, &pcm);
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

                    // Emit immediately on mute change.
                    inner.apply(peer.clone(), |s| s.is_muted = is_muted);
                }
            }
        }
    }
    .boxed_local()
}
