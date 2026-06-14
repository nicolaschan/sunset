//! `Dialer` impl that wraps `OpenRoom::connect_direct`.
//!
//! The voice subsystem calls `ensure_direct` when it sees a peer's
//! voice-presence entry, and `release` when the runtime decides the
//! peer is gone (membership-stale) or on shutdown. The actual WebRTC
//! negotiation is handled by `PeerSupervisor`; this file just
//! translates those lifecycle events into `connect_direct` /
//! `cancel_direct` calls on the room handle.
//!
//! ## Session-scoped intent lifecycle (why this file exists)
//!
//! `OpenRoom::connect_direct` registers a *durable* supervisor intent
//! deduplicated by the resolved `webrtc://<pk>#x25519=<x>` address.
//! Voice sessions are session-scoped, so without an explicit
//! `cancel_direct` the intent outlives the runtime: on the next
//! `voice_start` after a leave (or on the next presence event for a
//! peer the runtime previously decided was Stale), the supervisor
//! sees an existing intent and short-circuits the dial via dedup. If
//! the underlying WebRTC connection silently died in the gap (NAT
//! mapping expired, ICE failed, peer suspended), the orphaned intent
//! lingers in `Connected` or `Backoff` until the engine heartbeat
//! timeout (45 s default) — during which a fresh `connect_direct`
//! is a no-op, and the user sees "rejoined but no audio".
//!
//! We close that gap by tracking every `IntentId` we register, keyed
//! by the dialed `PeerId`, and tearing them down in two places:
//!
//! - `Dialer::release(peer)` — called from the runtime's auto-connect
//!   task when membership goes stale. Removes the per-peer entry.
//! - `Drop` — runs when the runtime drops (i.e. `voice_stop`). Spawns
//!   a `wasm_bindgen_futures::spawn_local` task to `cancel_direct`
//!   any remaining intents. The supervisor's command channel is FIFO
//!   so any immediately-following `voice_start`'s `connect_direct`
//!   queues after these `Remove`s, restoring "rejoin = fresh dial".

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use async_trait::async_trait;

use sunset_sync::{IntentId, PeerId};
use sunset_voice::Dialer;

use crate::room_handle::OpenRoomT;

pub(crate) struct WebDialer {
    pub open_room: Rc<OpenRoomT>,
    /// Per-peer supervisor `IntentId`s registered via
    /// `ensure_direct`. Cleared by `release(peer)` on
    /// membership-stale, and (for any survivors) by `Drop` on
    /// runtime shutdown. See the module docs for the full lifecycle.
    pub intent_ids: RefCell<HashMap<PeerId, IntentId>>,
    /// Relay-only mode: never attempt a direct WebRTC link, so all voice
    /// flows through the relay's re-forward. A user-facing privacy/firewall
    /// option (no P2P IP exposure); also what the relay-audio-fallback e2e
    /// uses to deterministically exercise the relayed path.
    pub relay_only: bool,
}

#[async_trait(?Send)]
impl Dialer for WebDialer {
    async fn ensure_direct(&self, peer: PeerId) {
        if self.relay_only {
            // No direct dial: the relay re-forwards this peer's voice.
            return;
        }
        let pk_bytes = peer.0.as_bytes();
        let arr: [u8; 32] = match pk_bytes.try_into() {
            Ok(a) => a,
            Err(_) => {
                tracing::warn!("WebDialer: peer public key is not 32 bytes, skipping dial");
                return;
            }
        };
        match self.open_room.connect_direct(arr).await {
            Ok(id) => {
                self.intent_ids.borrow_mut().insert(peer, id);
            }
            Err(e) => {
                tracing::warn!(error = %e, "voice ensure_direct failed");
            }
        }
    }

    async fn release(&self, peer: PeerId) {
        let id = self.intent_ids.borrow_mut().remove(&peer);
        if let Some(id) = id {
            self.open_room.cancel_direct(id).await;
        }
    }
}

impl Drop for WebDialer {
    fn drop(&mut self) {
        let ids: Vec<IntentId> = std::mem::take(&mut *self.intent_ids.borrow_mut())
            .into_values()
            .collect();
        if ids.is_empty() {
            return;
        }
        let open_room = self.open_room.clone();
        // Sync `Drop` can't `.await`, so the cancellation runs as a
        // spawn_local task. The supervisor's command channel is FIFO
        // (`mpsc::UnboundedSender`) and the cleanup's `Remove`
        // commands are enqueued from this task before the *next*
        // `voice_start`'s `Add` commands run — the user-initiated
        // gap between voice_stop and voice_start is many milliseconds;
        // the microtask queue drains in microseconds — so a
        // post-cleanup `connect_direct` sees a clean dedup slate and
        // creates a fresh intent.
        wasm_bindgen_futures::spawn_local(async move {
            for id in ids {
                open_room.cancel_direct(id).await;
            }
        });
    }
}
