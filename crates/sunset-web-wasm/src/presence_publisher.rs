//! Heartbeat publisher: spawns a task that periodically writes a
//! `<room_fp>/presence/<my_pk>` entry into the local store. The
//! engine's existing room_filter subscription propagates these to
//! peers automatically.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use wasm_bindgen::prelude::*;
use wasmtimer::tokio::sleep;

use sunset_core::Identity;
use sunset_store::{ContentBlock, SignedKvEntry, Store, canonical::signing_payload};
use sunset_store_memory::MemoryStore;

/// Spawn the heartbeat publisher. Runs forever (page lifetime).
#[allow(dead_code)]
pub fn spawn_publisher(
    identity: Identity,
    room_fp_hex: String,
    store: Arc<MemoryStore>,
    interval_ms: u64,
    ttl_ms: u64,
) {
    sunset_sync::spawn::spawn_local(async move {
        let my_hex = hex::encode(identity.store_verifying_key().as_bytes());
        let name_str = format!("{room_fp_hex}/presence/{my_hex}");
        loop {
            if let Err(e) = publish_once(&identity, &name_str, &store, ttl_ms).await {
                web_sys::console::warn_1(&JsValue::from_str(&format!("presence publisher: {e}")));
            }
            sleep(Duration::from_millis(interval_ms)).await;
        }
    });
}

#[allow(dead_code)]
async fn publish_once(
    identity: &Identity,
    name_str: &str,
    store: &MemoryStore,
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
