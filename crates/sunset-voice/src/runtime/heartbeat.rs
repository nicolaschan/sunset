//! Periodic heartbeat publisher. 2 s cadence, carries the runtime's
//! current `muted` flag.

use std::rc::Weak;

use bytes::Bytes;
use futures::FutureExt;
use rand_chacha::ChaCha20Rng;
use rand_core::SeedableRng;

use crate::packet::{VoicePacket, encrypt};
use super::{HEARTBEAT_INTERVAL, state::RuntimeInner};

pub(crate) fn spawn(weak: Weak<RuntimeInner>) -> futures::future::LocalBoxFuture<'static, ()> {
    async move {
        // Each task uses a divergent RNG seed so heartbeat nonces don't
        // collide with frame nonces.
        let now_nanos = web_time::SystemTime::now()
            .duration_since(web_time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        let mut rng = ChaCha20Rng::seed_from_u64(now_nanos ^ 0x55AA_55AA_55AA_55AA);

        loop {
            let Some(inner) = weak.upgrade() else { return; };
            let now_ms = web_time::SystemTime::now()
                .duration_since(web_time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);
            let muted = *inner.muted.borrow();
            let pkt = VoicePacket::Heartbeat { sent_at_ms: now_ms, is_muted: muted };
            let public = inner.identity.public();
            let room = inner.room.clone();
            let bus = inner.bus.clone();
            let room_fp = room.fingerprint().to_hex();
            let sender_pk = hex::encode(inner.identity.store_verifying_key().as_bytes());
            let name = Bytes::from(format!("voice/{room_fp}/{sender_pk}"));

            // Drop strong ref before awaiting so Drop can cancel us.
            drop(inner);

            match encrypt(&room, 0, &public, &pkt, &mut rng) {
                Ok(ev) => match postcard::to_stdvec(&ev) {
                    Ok(payload) => {
                        let _ = bus.publish_ephemeral(name, Bytes::from(payload)).await;
                    }
                    Err(e) => tracing::warn!(error = %e, "heartbeat postcard encode failed"),
                },
                Err(e) => tracing::warn!(error = %e, "heartbeat encrypt failed"),
            }

            sleep(HEARTBEAT_INTERVAL).await;
        }
    }
    .boxed_local()
}

#[cfg(target_arch = "wasm32")]
async fn sleep(d: std::time::Duration) {
    wasmtimer::tokio::sleep(d).await;
}
#[cfg(not(target_arch = "wasm32"))]
async fn sleep(d: std::time::Duration) {
    tokio::time::sleep(d).await;
}
