//! Per-peer voice-provider convergence.
//!
//! For every call participant `A`, this component arms exactly one
//! `subscribe_via` routing interest whose *provider* is a pure function of
//! observed connectivity:
//!
//! ```text
//! desired_provider(A) = A      if current_peers() contains (A, Secondary)
//!                     = relay  otherwise (the sole Primary peer)
//! ```
//!
//! When a direct WebRTC link to `A` exists, `A` forwards its own voice to
//! us (zero relay egress for the pair, "prefer WebRTC"). Otherwise the
//! relay re-forwards `A`'s voice (Layer 1). The provider is recomputed and
//! re-asserted on every engine peer-event and roster change — it is
//! *derived*, never edge-toggled, so a WebRTC flap or a stale-generation
//! event self-heals on the next recompute rather than wedging the wrong
//! provider.
//!
//! The roster (who is in the call) is the live set of the durable
//! voice-presence tracker (`voice_presence_liveness`), the same source of
//! truth the combiner uses for `in_voice_channel`. The presence stream
//! relays through the store, so a participant is known before its ephemeral
//! voice flows — the provider arms it the moment it joins the roster.

use std::collections::{HashMap, HashSet};
use std::rc::Weak;

use bytes::Bytes;
use futures::{FutureExt, StreamExt};

use sunset_core::bus::{Filter, SubscriptionPolicy};
use sunset_core::liveness::LivenessState;
use sunset_sync::PeerId;
use sunset_sync::transport::TransportKind;

use super::state::RuntimeInner;

pub(crate) fn spawn(weak: Weak<RuntimeInner>) -> futures::future::LocalBoxFuture<'static, ()> {
    async move {
        let Some(inner) = weak.upgrade() else {
            return;
        };
        let room_fp = inner.room.fingerprint().to_hex();
        let bus = inner.bus.clone();
        let presence_arc = inner.voice_presence_liveness.clone();
        drop(inner);

        let mut events = bus.subscribe_engine_events().await;
        let mut roster_changes = presence_arc.subscribe().await;

        // Derived state, both recomputable from (roster ∪ current_peers):
        //   - `roster`: participants currently in the call (presence-live).
        //   - `armed`:  the provider each participant is currently armed to.
        // `armed` is kept only to diff against the freshly-computed desired
        // provider so we issue `unsubscribe_via`/`subscribe_via` only on a
        // real change; it never carries authority of its own.
        let mut roster: HashSet<PeerId> = HashSet::new();
        let mut armed: HashMap<PeerId, PeerId> = HashMap::new();

        loop {
            tokio::select! {
                ev = events.recv() => {
                    if ev.is_none() {
                        return;
                    }
                    // Connectivity changed (or any engine event): reconverge.
                }
                change = roster_changes.next() => {
                    let Some(change) = change else { return; };
                    match change.state {
                        LivenessState::Live => { roster.insert(change.peer); }
                        LivenessState::Stale => {
                            // Participant left: withdraw whatever it was
                            // armed to, then forget it.
                            roster.remove(&change.peer);
                            if let Some(old) = armed.remove(&change.peer) {
                                let filter = voice_filter(&room_fp, &change.peer);
                                let _ = bus.unsubscribe_via(filter, old).await;
                            }
                            continue;
                        }
                    }
                }
                else => return,
            }

            if weak.upgrade().is_none() {
                return;
            }

            converge(&bus, &room_fp, &roster, &mut armed).await;
        }
    }
    .boxed_local()
}

/// Recompute `desired_provider(A)` for every roster participant and
/// converge `armed` to it, issuing the minimal routing mutations.
async fn converge(
    bus: &std::rc::Rc<dyn super::DynBus>,
    room_fp: &str,
    roster: &HashSet<PeerId>,
    armed: &mut HashMap<PeerId, PeerId>,
) {
    let peers = bus.current_peers().await;
    let relay = sole_primary(&peers);

    for participant in roster {
        let Some(desired) = desired_provider(participant, &peers, &relay) else {
            // No reachable provider this tick (e.g. no relay and no direct
            // link). Leave any existing arm untouched; the next event with a
            // provider present will arm it.
            continue;
        };
        if armed.get(participant) == Some(&desired) {
            continue;
        }
        let filter = voice_filter(room_fp, participant);
        if let Some(old) = armed.get(participant) {
            let _ = bus.unsubscribe_via(filter.clone(), old.clone()).await;
        }
        let _ = bus
            .subscribe_via(filter, desired.clone(), SubscriptionPolicy::store_data())
            .await;
        armed.insert(participant.clone(), desired);
    }
}

/// `desired_provider(A)`: the direct peer `A` when a `Secondary` link to it
/// exists, otherwise the sole `Primary` peer (the relay). `None` when
/// neither is available this tick.
fn desired_provider(
    participant: &PeerId,
    peers: &[(PeerId, TransportKind)],
    relay: &Option<PeerId>,
) -> Option<PeerId> {
    let direct = peers
        .iter()
        .any(|(p, kind)| p == participant && *kind == TransportKind::Secondary);
    if direct {
        Some(participant.clone())
    } else {
        relay.clone()
    }
}

/// The single `Primary` peer (the relay) in the v1 single-relay star, if
/// connected.
fn sole_primary(peers: &[(PeerId, TransportKind)]) -> Option<PeerId> {
    let mut primaries = peers
        .iter()
        .filter(|(_, kind)| *kind == TransportKind::Primary)
        .map(|(p, _)| p.clone());
    primaries.next()
}

fn voice_filter(room_fp: &str, participant: &PeerId) -> Filter {
    let pk_hex = hex::encode(participant.0.as_bytes());
    Filter::NamePrefix(Bytes::from(format!("voice/{room_fp}/{pk_hex}")))
}
