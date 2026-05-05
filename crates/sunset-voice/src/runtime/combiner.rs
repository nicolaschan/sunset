//! Combines the two `Liveness` streams into `VoicePeerState`. Debounces
//! by suppressing emissions when (in_call, talking, is_muted) doesn't
//! change for a peer.
//!
//! `in_call = frame_alive || membership_alive`. Both signals must be tracked
//! independently because a peer can register in `last_emitted` via frames
//! before any heartbeat arrives (or vice versa). If we computed `in_call`
//! from only the most-recent event, a hard departure that happened before
//! the first heartbeat reached us would leave `in_call` stuck at true: frame
//! Stale would drop `talking` but couldn't safely flip `in_call` (the peer
//! might still be heartbeating), and no membership Stale would ever fire
//! because membership_liveness has no entry to time out for that peer.

use std::rc::Weak;

use futures::{FutureExt, StreamExt};

use sunset_core::liveness::LivenessState;

use super::state::{EmittedState, RuntimeInner};
use super::traits::VoicePeerState;

pub(crate) fn spawn(weak: Weak<RuntimeInner>) -> futures::future::LocalBoxFuture<'static, ()> {
    async move {
        let Some(inner) = weak.upgrade() else {
            return;
        };
        let frame_arc = inner.frame_liveness.clone();
        let membership_arc = inner.membership_liveness.clone();
        drop(inner);

        let mut frame_sub = frame_arc.subscribe().await;
        let mut membership_sub = membership_arc.subscribe().await;

        loop {
            tokio::select! {
                Some(ev) = frame_sub.next() => {
                    let Some(inner) = weak.upgrade() else { return; };
                    let alive = ev.state == LivenessState::Live;
                    let mut last = inner.last_emitted.borrow_mut();
                    let entry = last.entry(ev.peer.clone()).or_insert(EmittedState {
                        in_call: false, talking: false, is_muted: false,
                        frame_alive: false, membership_alive: false,
                    });
                    let mut new = *entry;
                    new.frame_alive = alive;
                    new.talking = alive;
                    new.in_call = new.frame_alive || new.membership_alive;
                    if new != *entry {
                        *entry = new;
                        let state = VoicePeerState {
                            peer: ev.peer.clone(),
                            in_call: new.in_call,
                            talking: new.talking,
                            is_muted: new.is_muted,
                        };
                        let sink = inner.peer_state_sink.clone();
                        drop(last);
                        sink.emit(&state);
                    }
                }
                Some(ev) = membership_sub.next() => {
                    let Some(inner) = weak.upgrade() else { return; };
                    let alive = ev.state == LivenessState::Live;
                    let mut last = inner.last_emitted.borrow_mut();
                    let entry = last.entry(ev.peer.clone()).or_insert(EmittedState {
                        in_call: false, talking: false, is_muted: false,
                        frame_alive: false, membership_alive: false,
                    });
                    let mut new = *entry;
                    new.membership_alive = alive;
                    new.in_call = new.frame_alive || new.membership_alive;
                    if new != *entry {
                        *entry = new;
                        let state = VoicePeerState {
                            peer: ev.peer.clone(),
                            in_call: new.in_call,
                            talking: new.talking,
                            is_muted: new.is_muted,
                        };
                        let sink = inner.peer_state_sink.clone();
                        drop(last);
                        sink.emit(&state);
                    }
                }
                else => return,
            }
        }
    }
    .boxed_local()
}
