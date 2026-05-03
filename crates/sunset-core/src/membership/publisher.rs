//! Heartbeat publisher: spawns a task that periodically writes a
//! `<room_fp>/presence/<my_pk>` entry into the local store. The
//! engine's existing room_filter subscription propagates these to
//! peers automatically.
//!
//! Moved from `sunset-web-wasm::presence_publisher` so non-web hosts
//! (TUI, Minecraft mod, native relay) can use the same logic.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;

// Portable sleep: native uses tokio::time, wasm uses wasmtimer's
// setTimeout-backed drop-in.
#[cfg(not(target_arch = "wasm32"))]
use tokio::time::sleep;
#[cfg(target_arch = "wasm32")]
use wasmtimer::tokio::sleep;

use crate::Identity;
use sunset_store::{ContentBlock, SignedKvEntry, Store, canonical::signing_payload};

/// Spawn the heartbeat publisher. Runs forever (host-process / page lifetime).
pub fn spawn_publisher<S: Store + 'static>(
    identity: Identity,
    room_fp_hex: String,
    store: Arc<S>,
    interval_ms: u64,
    ttl_ms: u64,
) {
    sunset_sync::spawn::spawn_local(async move {
        let my_hex = hex::encode(identity.store_verifying_key().as_bytes());
        let name_str = format!("{room_fp_hex}/presence/{my_hex}");
        loop {
            if let Err(e) = publish_once(&identity, &name_str, &*store, ttl_ms).await {
                tracing::warn!("presence publisher: {e}");
            }
            sleep(Duration::from_millis(interval_ms)).await;
        }
    });
}

async fn publish_once<S: Store + 'static>(
    identity: &Identity,
    name_str: &str,
    store: &S,
    ttl_ms: u64,
) -> Result<(), String> {
    let block = ContentBlock {
        data: Bytes::new(),
        references: vec![],
    };
    let value_hash = block.hash();
    let now = web_time::SystemTime::now()
        .duration_since(web_time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let mut entry = SignedKvEntry {
        verifying_key: identity.store_verifying_key(),
        name: Bytes::from(name_str.to_owned()),
        value_hash,
        priority: now,
        expires_at: Some(now + ttl_ms),
        signature: Bytes::new(),
    };
    let payload = signing_payload(&entry);
    let sig = identity.sign(&payload);
    entry.signature = Bytes::copy_from_slice(&sig.to_bytes());
    store
        .insert(entry, Some(block))
        .await
        .map_err(|e| format!("{e}"))?;
    Ok(())
}
