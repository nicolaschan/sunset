//! Subscribes to durable `voice-presence/<room_fp>/<sender>` entries
//! and feeds the `voice_presence_liveness` tracker, which the
//! combiner reads to drive the `in_voice_channel` flag in
//! `VoicePeerState`.
//!
//! This is the source of truth for "who is currently in the voice
//! channel" — distinct from `auto_connect.rs` (which uses the same
//! stream to drive WebRTC dial decisions) and from
//! `subscribe.rs` (which feeds `frame_liveness` and
//! `membership_liveness` from the *ephemeral* per-peer voice
//! channel — those signals only fire after a P2P connection is up).
//!
//! Why a separate task: the existing `auto_connect` consumer filters
//! events by pubkey-comparison (only acts on peers it should dial),
//! while membership tracking needs *every* event regardless of dial
//! direction. Forking the consumer keeps each task's filter rules
//! local to its concern.
//!
//! The `Liveness` `stale_after` is `VOICE_PRESENCE_STALE_AFTER`
//! (deliberately a touch wider than `VOICE_PRESENCE_TTL` so a single
//! missed republish doesn't visibly bump a peer out of the roster).
//! We periodically tick a "no-op observe" on the local pubkey so the
//! Liveness sweep fires even when no remote presence events are
//! arriving — without this, a remote peer who left the channel would
//! stay `in_voice_channel=true` until the *next* presence event from
//! some other peer triggered the sweep.

use std::rc::Weak;
use std::time::SystemTime;

use futures::{FutureExt, StreamExt};

use sunset_core::bus::BusEvent;
use sunset_sync::PeerId;

use super::state::RuntimeInner;
use super::{VOICE_PRESENCE_REFRESH_INTERVAL, voice_presence_prefix};

pub(crate) fn spawn(weak: Weak<RuntimeInner>) -> futures::future::LocalBoxFuture<'static, ()> {
    async move {
        let Some(inner) = weak.upgrade() else {
            return;
        };
        let room_fp = inner.room.fingerprint().to_hex();
        let prefix = voice_presence_prefix(&room_fp);
        let self_pk = inner.identity.store_verifying_key();
        let bus = inner.bus.clone();
        let presence_arc = inner.voice_presence_liveness.clone();
        drop(inner);

        let mut stream = match bus.subscribe_prefix(prefix.clone()).await {
            Ok(s) => s,
            Err(e) => {
                tracing::error!(error = %e, "voice-presence membership subscribe failed");
                return;
            }
        };

        // Periodic sweep: observe a synthetic self entry so Liveness's
        // observe-time sweep transitions any stale remote peer to
        // Stale even when the bus stream is idle. Without this, a
        // peer who silently leaves the channel would stay
        // `in_voice_channel=true` until some *other* peer's presence
        // event triggered the sweep — which could be never in a
        // 2-peer call after one departs.
        //
        // We use a sleep-loop rather than `tokio::time::interval`
        // because Interval depends on `std::time::Instant` which
        // panics on wasm32. The cfg-gated `sleep` helper below
        // routes through `wasmtimer::tokio::sleep` on wasm — same
        // pattern as heartbeat, jitter pump, and presence publisher.
        let sweep_self_peer = PeerId(self_pk.clone());

        loop {
            let sleep = sleep(VOICE_PRESENCE_REFRESH_INTERVAL);
            tokio::pin!(sleep);

            tokio::select! {
                Some(ev) = stream.next() => {
                    let entry = match ev {
                        BusEvent::Durable { entry, .. } => entry,
                        BusEvent::Ephemeral(_) => continue,
                    };
                    // Skip self: our own publisher loops back here
                    // through the bus, but the UI tracks self via
                    // `self_in_call` not `voice.peers[self]`, so
                    // forwarding self into the combiner just churns
                    // the peer dict.
                    if entry.verifying_key == self_pk {
                        continue;
                    }
                    let st = SystemTime::UNIX_EPOCH
                        + std::time::Duration::from_millis(entry.priority);
                    let peer = PeerId(entry.verifying_key.clone());
                    presence_arc.observe(peer, st).await;
                }
                _ = &mut sleep => {
                    // Observe self with current wall-clock time so
                    // Liveness's per-observe sweep checks every peer's
                    // freshness against `stale_after`. `web_time` is
                    // used (rather than `std::time::SystemTime::now`)
                    // because std panics on wasm; we re-base onto
                    // `std::time::UNIX_EPOCH` via the duration so the
                    // type matches Liveness's API.
                    let d = web_time::SystemTime::now()
                        .duration_since(web_time::UNIX_EPOCH)
                        .unwrap_or(std::time::Duration::ZERO);
                    let now = SystemTime::UNIX_EPOCH + d;
                    presence_arc.observe(sweep_self_peer.clone(), now).await;
                }
                else => return,
            }

            if weak.upgrade().is_none() {
                return;
            }
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
