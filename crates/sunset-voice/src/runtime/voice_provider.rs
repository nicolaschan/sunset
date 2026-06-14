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
//! voice flows — the provider arms it the moment it joins the roster *once
//! the local user has actually joined the call*.
//!
//! Arming is gated on `is_active`: in observer mode (the user is watching the
//! roster but has not joined) the provider arms nothing, so the relay never
//! re-forwards anyone's audio to us — an observer neither hears the call nor
//! shows as `in_call`. On `set_active(true)` it converges and arms; on
//! `set_active(false)` it withdraws everything. The active transition is
//! delivered through the `is_active` watch so a join/leave is acted on
//! promptly rather than only on the next connectivity event.

use std::collections::{HashMap, HashSet};
use std::rc::Weak;

use bytes::Bytes;
use futures::{FutureExt, StreamExt};

use sunset_core::bus::{Bus, Filter, SubscriptionPolicy};
use sunset_core::liveness::LivenessState;
use sunset_sync::PeerId;
use sunset_sync::transport::TransportKind;

use super::state::RuntimeInner;

pub(crate) fn spawn(weak: Weak<RuntimeInner>) -> futures::future::LocalBoxFuture<'static, ()> {
    // Subscribe to the active-state watch synchronously, before this future
    // is first polled, so a `set_active(true)` that races task startup is
    // still delivered as a change — otherwise the join that should arm the
    // call could be missed when the roster is already populated.
    let active_rx = weak.upgrade().map(|inner| inner.is_active.subscribe());
    async move {
        let Some(inner) = weak.upgrade() else {
            return;
        };
        let Some(mut active_rx) = active_rx else {
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
                changed = active_rx.changed() => {
                    // The local user joined or left the call: reconverge so we
                    // arm (on join) or withdraw everything (on leave).
                    if changed.is_err() {
                        return; // sender dropped — the runtime is gone.
                    }
                }
                else => return,
            }

            // This task holds its own `Rc<dyn Bus>`/`Arc<Liveness>`
            // clones, so the event/roster streams stay open even after the
            // sole `Rc<RuntimeInner>` is dropped. The upgrade check is how we
            // notice that drop and exit — it is the real shutdown signal, not
            // dead code (stream closure alone would never fire here).
            if weak.upgrade().is_none() {
                return;
            }

            let active = *active_rx.borrow();
            converge(&bus, &room_fp, active, &roster, &mut armed).await;
        }
    }
    .boxed_local()
}

/// Recompute the desired provider for every roster participant and converge
/// `armed` to it, issuing the minimal routing mutations. When the local user
/// is not active (observer mode) the desired set is empty: every arm is
/// withdrawn so the relay forwards no audio to us.
async fn converge(
    bus: &std::rc::Rc<dyn Bus>,
    room_fp: &str,
    active: bool,
    roster: &HashSet<PeerId>,
    armed: &mut HashMap<PeerId, PeerId>,
) {
    if !active {
        // Observer: pull no audio. Withdraw every arm (collect first so we
        // don't hold a borrow on `armed` across the awaits).
        let to_withdraw: Vec<(PeerId, PeerId)> = armed.drain().collect();
        for (participant, provider) in to_withdraw {
            let filter = voice_filter(room_fp, &participant);
            let _ = bus.unsubscribe_via(filter, provider).await;
        }
        return;
    }

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
