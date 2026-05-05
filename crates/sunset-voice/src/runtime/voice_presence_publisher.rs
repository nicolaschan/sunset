//! Publishes a per-(peer, room) durable presence entry every
//! `VOICE_PRESENCE_REFRESH_INTERVAL` with `priority = current_ms`.
//! Entry value is empty bytes (presence by existence + LWW priority).
//!
//! Pattern: `SignedKvEntry` on name `voice-presence/<room_fp>/<sender>`,
//! value = empty, priority = now_ms, TTL deadline = now + TTL.
//! Re-publishing replaces (LWW) the prior entry.

use std::rc::Weak;

use bytes::Bytes;
use futures::FutureExt;

use sunset_store::{ContentBlock, SignedKvEntry, canonical::signing_payload};

use super::{
    VOICE_PRESENCE_REFRESH_INTERVAL, VOICE_PRESENCE_TTL, state::RuntimeInner, voice_presence_name,
};

pub(crate) fn spawn(weak: Weak<RuntimeInner>) -> futures::future::LocalBoxFuture<'static, ()> {
    async move {
        let Some(inner) = weak.upgrade() else {
            return;
        };
        let room_fp = inner.room.fingerprint().to_hex();
        let sender_pk_hex = hex::encode(inner.identity.store_verifying_key().as_bytes());
        let name = voice_presence_name(&room_fp, &sender_pk_hex);
        let identity = inner.identity.clone();
        let verifying_key = inner.identity.store_verifying_key();
        let bus = inner.bus.clone();
        drop(inner);

        loop {
            let now_ms = web_time::SystemTime::now()
                .duration_since(web_time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);

            let block = ContentBlock {
                data: Bytes::new(),
                references: vec![],
            };
            let value_hash = block.hash();
            let ttl_ms = VOICE_PRESENCE_TTL.as_millis() as u64;
            let mut entry = SignedKvEntry {
                verifying_key: verifying_key.clone(),
                name: name.clone(),
                value_hash,
                priority: now_ms,
                expires_at: Some(now_ms + ttl_ms),
                signature: Bytes::new(),
            };
            let payload = signing_payload(&entry);
            let sig = identity.sign(&payload);
            entry.signature = Bytes::copy_from_slice(&sig.to_bytes());

            if let Err(e) = bus.publish_durable(entry, Some(block)).await {
                tracing::warn!(error = %e, "voice-presence publish failed");
            }

            sleep(VOICE_PRESENCE_REFRESH_INTERVAL).await;

            let Some(_) = weak.upgrade() else {
                return;
            };
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
