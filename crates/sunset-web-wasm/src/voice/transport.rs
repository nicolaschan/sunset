//! Voice transport — heartbeat publisher and Bus type alias.
//!
//! Owns the periodic heartbeat task. Frame send is in `voice/mod.rs`
//! (it's per-call, not periodic).

use std::rc::Rc;
use std::time::Duration;

use bytes::Bytes;
use rand_chacha::ChaCha20Rng;
use rand_core::SeedableRng;
use wasmtimer::tokio::sleep;

use sunset_core::bus::{Bus, BusImpl};
use sunset_core::{Identity, Room};
use sunset_noise::NoiseTransport;
use sunset_store_memory::MemoryStore;
use sunset_sync::MultiTransport;
use sunset_sync_webrtc_browser::WebRtcRawTransport;
use sunset_sync_ws_browser::WebSocketRawTransport;

use super::VoiceCell;

type WsT = NoiseTransport<WebSocketRawTransport>;
type RtcT = NoiseTransport<WebRtcRawTransport>;
pub(crate) type BusArc = Rc<BusImpl<MemoryStore, MultiTransport<WsT, RtcT>>>;

/// Heartbeat cadence. Liveness considers a peer "in-call" if heartbeats
/// arrive within ~5 s, so 2 s leaves room for one or two losses.
#[allow(dead_code)] // Used by spawn_heartbeat once Task 6 wires the FFI.
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(2);

/// Spawn the periodic heartbeat task. Exits when `state` becomes None
/// (voice_stop has been called and the cell content has been dropped).
#[allow(dead_code)] // Wired up by Client in Task 6.
pub(crate) fn spawn_heartbeat(state: VoiceCell, identity: Identity, room: Rc<Room>, bus: BusArc) {
    wasm_bindgen_futures::spawn_local(async move {
        let now_nanos = web_time::SystemTime::now()
            .duration_since(web_time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        // XOR with a constant so heartbeat RNG diverges from voice_input's RNG.
        let mut rng = ChaCha20Rng::seed_from_u64(now_nanos ^ 0x55AA_55AA_55AA_55AA);

        let room_fp_hex = room.fingerprint().to_hex();
        let sender_pk_hex = hex::encode(identity.store_verifying_key().as_bytes());
        let name = Bytes::from(format!("voice/{room_fp_hex}/{sender_pk_hex}"));

        loop {
            // Exit if voice_stop has been called.
            if state.borrow().is_none() {
                return;
            }

            let now_ms = web_time::SystemTime::now()
                .duration_since(web_time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);

            let packet = sunset_voice::packet::VoicePacket::Heartbeat { sent_at_ms: now_ms };
            match sunset_voice::packet::encrypt(&room, 0, &identity.public(), &packet, &mut rng) {
                Ok(ev) => match postcard::to_stdvec(&ev) {
                    Ok(payload) => {
                        if let Err(e) = bus
                            .publish_ephemeral(name.clone(), Bytes::from(payload))
                            .await
                        {
                            web_sys::console::warn_1(
                                &format!("voice heartbeat publish failed: {e}").into(),
                            );
                        }
                    }
                    Err(e) => {
                        web_sys::console::warn_1(
                            &format!("voice heartbeat postcard encode failed: {e}").into(),
                        );
                    }
                },
                Err(e) => {
                    web_sys::console::warn_1(
                        &format!("voice heartbeat encrypt failed: {e}").into(),
                    );
                }
            }

            sleep(HEARTBEAT_INTERVAL).await;
        }
    });
}
