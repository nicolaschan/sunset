//! Combines the three `Liveness` streams (frames, ephemeral
//! heartbeats, durable voice-presence) into `VoicePeerState`, debounced
//! so a peer's emission is suppressed when its observable projection
//! doesn't change. Each arm records one source fact via
//! `RuntimeInner::apply`, which owns the projection and debounce.

use std::rc::Weak;

use futures::{FutureExt, StreamExt};

use sunset_core::liveness::LivenessState;

use super::state::RuntimeInner;

pub(crate) fn spawn(weak: Weak<RuntimeInner>) -> futures::future::LocalBoxFuture<'static, ()> {
    async move {
        let Some(inner) = weak.upgrade() else {
            return;
        };
        let frame_arc = inner.frame_liveness.clone();
        let membership_arc = inner.membership_liveness.clone();
        let presence_arc = inner.voice_presence_liveness.clone();
        drop(inner);

        let mut frame_sub = frame_arc.subscribe().await;
        let mut membership_sub = membership_arc.subscribe().await;
        let mut presence_sub = presence_arc.subscribe().await;

        loop {
            tokio::select! {
                Some(ev) = frame_sub.next() => {
                    let Some(inner) = weak.upgrade() else { return; };
                    let alive = ev.state == LivenessState::Live;
                    inner.apply(ev.peer, |s| { s.frame_alive = alive; s.talking = alive; });
                }
                Some(ev) = membership_sub.next() => {
                    let Some(inner) = weak.upgrade() else { return; };
                    let alive = ev.state == LivenessState::Live;
                    inner.apply(ev.peer, |s| s.membership_alive = alive);
                }
                Some(ev) = presence_sub.next() => {
                    let Some(inner) = weak.upgrade() else { return; };
                    let alive = ev.state == LivenessState::Live;
                    inner.apply(ev.peer, |s| s.presence_alive = alive);
                }
                else => return,
            }
        }
    }
    .boxed_local()
}
