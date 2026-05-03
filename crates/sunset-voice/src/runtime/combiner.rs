//! Combines the two `Liveness` streams into `VoicePeerState`. Debounces
//! by suppressing emissions when (in_call, talking, is_muted) doesn't
//! change for a peer.

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
                    });
                    let mut new = *entry;
                    new.talking = alive;
                    // Talking implies in_call.
                    if alive {
                        new.in_call = true;
                    }
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
                    });
                    let mut new = *entry;
                    // membership Live → in_call=true; Stale → in_call depends on talking
                    new.in_call = alive || new.talking;
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
