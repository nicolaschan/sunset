//! Voice subscribe loop — runs while voice_start is active. Subscribes
//! to `voice/<room_fp>/` via Bus, decrypts each VoicePacket, dispatches
//! Frames to the JS `on_frame` callback and feeds both Liveness arcs.

use std::rc::Rc;

use bytes::Bytes;
use futures::StreamExt as _;
use js_sys::Function;
use wasm_bindgen::prelude::*;

use sunset_core::Room;
use sunset_core::bus::{Bus, BusEvent};
use sunset_core::identity::IdentityKey;
use sunset_store::Filter;
use sunset_sync::PeerId;
use sunset_voice::VoiceDecoder;
use sunset_voice::packet::VoicePacket;

use super::VoiceCell;
use super::liveness::VoiceLiveness;
use super::transport::BusArc;

/// Spawn the subscribe loop. The loop exits when the Bus stream ends
/// or when `state` becomes None (voice_stop).
pub(crate) fn spawn_subscriber(
    state: VoiceCell,
    room: Rc<Room>,
    bus: BusArc,
    arcs: VoiceLiveness,
    on_frame: Function,
    self_pk: sunset_store::VerifyingKey,
) {
    wasm_bindgen_futures::spawn_local(async move {
        let room_fp_hex = room.fingerprint().to_hex();
        let prefix = Bytes::from(format!("voice/{room_fp_hex}/"));
        let mut stream = match bus.subscribe(Filter::NamePrefix(prefix)).await {
            Ok(s) => s,
            Err(e) => {
                tracing::error!(error = %e, "voice subscribe failed");
                return;
            }
        };

        let mut decoder = match VoiceDecoder::new() {
            Ok(d) => d,
            Err(e) => {
                tracing::error!(error = %e, "voice decoder init failed");
                return;
            }
        };

        while let Some(ev) = stream.next().await {
            // Allow voice_stop to terminate the loop.
            if state.borrow().is_none() {
                return;
            }
            let datagram = match ev {
                BusEvent::Ephemeral(d) => d,
                BusEvent::Durable { .. } => continue,
            };
            // Skip self-loopback: the user's own audio is already
            // played by the worklet locally.
            if datagram.verifying_key == self_pk {
                continue;
            }
            let peer = PeerId(datagram.verifying_key.clone());
            let sender = match IdentityKey::from_store_verifying_key(&datagram.verifying_key) {
                Ok(s) => s,
                Err(_) => continue,
            };

            let ev: sunset_voice::packet::EncryptedVoicePacket =
                match postcard::from_bytes(&datagram.payload) {
                    Ok(e) => e,
                    Err(_) => continue,
                };
            let packet = match sunset_voice::packet::decrypt(&room, 0, &sender, &ev) {
                Ok(p) => p,
                Err(e) => {
                    tracing::warn!(error = %e, "voice decrypt failed (drop frame)");
                    continue;
                }
            };
            match packet {
                VoicePacket::Frame {
                    sender_time_ms,
                    payload,
                    ..
                } => {
                    let st = std::time::SystemTime::UNIX_EPOCH
                        + std::time::Duration::from_millis(sender_time_ms);
                    arcs.frame.observe(peer.clone(), st).await;
                    match decoder.decode(&payload) {
                        Ok(pcm) => {
                            let id_arr = js_sys::Uint8Array::from(peer.0.as_bytes());
                            let pcm_arr = js_sys::Float32Array::from(pcm.as_slice());
                            let _ = on_frame.call2(&JsValue::NULL, &id_arr, &pcm_arr);
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, "voice decode failed");
                        }
                    }
                }
                VoicePacket::Heartbeat { sent_at_ms } => {
                    let st = std::time::SystemTime::UNIX_EPOCH
                        + std::time::Duration::from_millis(sent_at_ms);
                    arcs.membership.observe(peer, st).await;
                }
            }
        }
    });
}
