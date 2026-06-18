//! Per-peer voice-provider convergence.
//!
//! For every call participant `A`, this component arms a *set* of
//! `subscribe_via` routing interests, derived as a pure function of
//! observed connectivity **and** observed direct delivery:
//!
//! ```text
//! desired_providers(A) ⊆ {A, relay}
//!   A     ∈ desired  iff current_peers() contains (A, Secondary)
//!   relay ∈ desired  iff a relay (the sole Primary peer) is connected
//!                     AND the direct path to A is not yet *proven live*
//! ```
//!
//! The relay is the always-available fallback: it re-forwards `A`'s voice
//! (Layer 1) and is reachable the moment `A` joins the roster. A direct
//! WebRTC link lets `A` forward its own voice to us (zero relay egress,
//! "prefer WebRTC") — but only once `A` has actually armed our interest on
//! that link, which depends on cross-network subscription propagation that
//! can lag the link coming up. So a direct *link* existing is not the same
//! as the direct *path* delivering.
//!
//! The crucial invariant: **the relay is dropped only once direct frames
//! are actually arriving** (`direct_frame_liveness`), never on the mere
//! existence of a `Secondary` transport. When a direct link appears we arm
//! `A` *in addition to* the relay; once `FrameVia::Direct` traffic from `A`
//! is observed the relay becomes redundant and is withdrawn; if that direct
//! traffic later goes stale the relay is re-armed. The receiver dedup
//! (`peer_envelope_hwm`) discards the duplicate a peer briefly gets from
//! both paths during the overlap, so co-arming is free of double audio.
//!
//! This makes the relay→direct transition gap-free *by construction*: there
//! is no instant at which a roster participant has no delivering provider,
//! because the working path (relay) is never torn down before a strictly
//! better one is confirmed. Everything is *derived* and re-asserted on every
//! engine peer-event, roster change, and direct-liveness transition, so a
//! WebRTC flap or a stale-generation event self-heals on the next recompute.
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
        let direct_arc = inner.direct_frame_liveness.clone();
        drop(inner);

        let mut events = bus.subscribe_engine_events().await;
        let mut roster_changes = presence_arc.subscribe().await;
        // Transitions of the per-peer direct-path liveness. A `Live` here
        // means direct frames/heartbeats from that peer are now arriving, so
        // the relay can be dropped; a `Stale` means the direct path went
        // silent, so the relay must come back. This is the signal that makes
        // the downgrade gap-free — see the module doc.
        let mut direct_changes = direct_arc.subscribe().await;

        // Derived state, all recomputable from
        // (roster ∪ current_peers ∪ direct_live):
        //   - `roster`:      participants currently in the call (presence-live).
        //   - `direct_live`: participants whose direct path is proven delivering.
        //   - `armed`:       the provider *set* each participant is armed to.
        // `armed` is kept only to diff against the freshly-computed desired set
        // so we issue `unsubscribe_via`/`subscribe_via` only on a real change;
        // it never carries authority of its own. `direct_live` is written only
        // from the `direct_changes` arm (single source of truth).
        let mut roster: HashSet<PeerId> = HashSet::new();
        let mut direct_live: HashSet<PeerId> = HashSet::new();
        let mut armed: HashMap<PeerId, HashSet<PeerId>> = HashMap::new();

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
                            if let Some(providers) = armed.remove(&change.peer) {
                                let filter = voice_filter(&room_fp, &change.peer);
                                for provider in providers {
                                    let _ = bus.unsubscribe_via(filter.clone(), provider).await;
                                }
                            }
                            continue;
                        }
                    }
                }
                change = direct_changes.next() => {
                    let Some(change) = change else { return; };
                    match change.state {
                        LivenessState::Live => { direct_live.insert(change.peer); }
                        LivenessState::Stale => { direct_live.remove(&change.peer); }
                    }
                    // Fall through to reconverge: the direct path's delivery
                    // status changed, which may drop or re-arm the relay.
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
            converge(&bus, &room_fp, active, &roster, &direct_live, &mut armed).await;
        }
    }
    .boxed_local()
}

/// Recompute the desired provider *set* for every roster participant and
/// converge `armed` to it, issuing the minimal routing mutations. When the
/// local user is not active (observer mode) the desired set is empty for
/// everyone: every arm is withdrawn so the relay forwards no audio to us.
async fn converge(
    bus: &std::rc::Rc<dyn Bus>,
    room_fp: &str,
    active: bool,
    roster: &HashSet<PeerId>,
    direct_live: &HashSet<PeerId>,
    armed: &mut HashMap<PeerId, HashSet<PeerId>>,
) {
    if !active {
        // Observer: pull no audio. Withdraw every arm (collect first so we
        // don't hold a borrow on `armed` across the awaits).
        let to_withdraw: Vec<(PeerId, HashSet<PeerId>)> = armed.drain().collect();
        for (participant, providers) in to_withdraw {
            let filter = voice_filter(room_fp, &participant);
            for provider in providers {
                let _ = bus.unsubscribe_via(filter.clone(), provider).await;
            }
        }
        return;
    }

    let peers = bus.current_peers().await;
    let relay = sole_primary(&peers);

    for participant in roster {
        let desired = desired_providers(
            participant,
            &peers,
            &relay,
            direct_live.contains(participant),
        );
        if desired.is_empty() {
            // No reachable provider this tick (no relay and no direct link).
            // Leave any existing arm untouched; the next event with a
            // provider present converges it. Withdrawing here would only
            // churn an entry we are about to re-create.
            continue;
        }
        // Clone the current arm set so we can diff and await without holding
        // a borrow into `armed` across the routing calls.
        let current = armed.get(participant).cloned().unwrap_or_default();
        if current == desired {
            continue;
        }
        let filter = voice_filter(room_fp, participant);
        // Withdraw providers that are no longer desired (e.g. the relay once
        // the direct path is proven live)...
        for provider in current.difference(&desired) {
            let _ = bus.unsubscribe_via(filter.clone(), provider.clone()).await;
        }
        // ...and arm newly-desired ones (e.g. the direct peer when its link
        // appears, while the relay stays armed until direct is confirmed).
        for provider in desired.difference(&current) {
            let _ = bus
                .subscribe_via(
                    filter.clone(),
                    provider.clone(),
                    SubscriptionPolicy::store_data(),
                )
                .await;
        }
        armed.insert(participant.clone(), desired);
    }
}

/// `desired_providers(A)`: the set of providers to pull `A`'s voice from.
///
/// - The direct peer `A` is included whenever a `Secondary` link to it
///   exists (so `A` forwards its own voice once it arms our interest).
/// - The relay (the sole `Primary` peer) is included whenever it is
///   connected **and** the direct path is not yet proven live — keeping the
///   working fallback in place until a direct path actually delivers, then
///   dropping it to save relay egress. `direct_live` is the observed-delivery
///   signal (`FrameVia::Direct` traffic seen recently), not link presence.
///
/// Empty only when neither a relay nor a direct link is available this tick.
fn desired_providers(
    participant: &PeerId,
    peers: &[(PeerId, TransportKind)],
    relay: &Option<PeerId>,
    direct_live: bool,
) -> HashSet<PeerId> {
    let direct = peers
        .iter()
        .any(|(p, kind)| p == participant && *kind == TransportKind::Secondary);
    let mut providers = HashSet::new();
    if direct {
        providers.insert(participant.clone());
    }
    if let Some(relay) = relay {
        // Drop the relay only once the direct path is *proven* delivering.
        if !(direct && direct_live) {
            providers.insert(relay.clone());
        }
    }
    providers
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

#[cfg(test)]
mod tests {
    use super::*;
    use sunset_store::VerifyingKey;

    fn peer(label: &[u8]) -> PeerId {
        PeerId(VerifyingKey::new(Bytes::copy_from_slice(label)))
    }

    /// No direct link and no relay connected: nothing to arm.
    #[test]
    fn empty_when_no_relay_and_no_direct() {
        let a = peer(b"alice");
        let got = desired_providers(&a, &[], &None, false);
        assert!(got.is_empty());
    }

    /// Relay-only reachability: pull `A` via the relay.
    #[test]
    fn relay_only_when_no_direct_link() {
        let a = peer(b"alice");
        let relay = peer(b"relay");
        let peers = vec![(relay.clone(), TransportKind::Primary)];
        let got = desired_providers(&a, &peers, &Some(relay.clone()), false);
        assert_eq!(got, HashSet::from([relay]));
    }

    /// Direct link present but direct delivery not yet confirmed: arm the
    /// direct peer *and* keep the relay — this is the co-armed overlap that
    /// closes the dark window.
    #[test]
    fn co_arms_direct_and_relay_until_direct_proven() {
        let a = peer(b"alice");
        let relay = peer(b"relay");
        let peers = vec![
            (relay.clone(), TransportKind::Primary),
            (a.clone(), TransportKind::Secondary),
        ];
        let got = desired_providers(&a, &peers, &Some(relay.clone()), false);
        assert_eq!(got, HashSet::from([a, relay]));
    }

    /// Direct link present and direct traffic confirmed live: the relay is
    /// redundant and dropped — only the direct peer remains.
    #[test]
    fn direct_only_once_direct_proven_live() {
        let a = peer(b"alice");
        let relay = peer(b"relay");
        let peers = vec![
            (relay.clone(), TransportKind::Primary),
            (a.clone(), TransportKind::Secondary),
        ];
        let got = desired_providers(&a, &peers, &Some(relay), true);
        assert_eq!(got, HashSet::from([a]));
    }

    /// A direct link with no relay connected: just the direct peer,
    /// regardless of whether direct delivery is confirmed yet (there is no
    /// fallback to keep).
    #[test]
    fn direct_only_when_no_relay() {
        let a = peer(b"alice");
        let peers = vec![(a.clone(), TransportKind::Secondary)];
        assert_eq!(
            desired_providers(&a, &peers, &None, false),
            HashSet::from([a.clone()])
        );
        assert_eq!(
            desired_providers(&a, &peers, &None, true),
            HashSet::from([a])
        );
    }
}
