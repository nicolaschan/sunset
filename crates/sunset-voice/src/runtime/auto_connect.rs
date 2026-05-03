//! Auto-connect FSM: per-peer Unknown → Dialing → (eventually Gone via
//! membership_liveness Stale → back to Unknown).
//!
//! Notifications come from the subscribe loop on every heartbeat.
//! Liveness Stale events come from the membership_liveness subscribe.

use std::rc::Weak;

use futures::{FutureExt, StreamExt};

use sunset_core::liveness::LivenessState;

use super::state::{AutoConnectState, RuntimeInner};

pub(crate) fn spawn(weak: Weak<RuntimeInner>) -> futures::future::LocalBoxFuture<'static, ()> {
    async move {
        let Some(inner) = weak.upgrade() else {
            return;
        };
        let mut hb_rx = inner
            .auto_connect_chan
            .rx
            .borrow_mut()
            .take()
            .expect("auto_connect rx taken once");
        let membership_arc = inner.membership_liveness.clone();
        drop(inner);
        let mut life_sub = membership_arc.subscribe().await;

        loop {
            tokio::select! {
                Some(peer) = hb_rx.recv() => {
                    let dialer_to_call = {
                        let Some(inner) = weak.upgrade() else { return; };
                        let mut state = inner.auto_connect_state.borrow_mut();
                        let entry = state.entry(peer.clone()).or_insert(AutoConnectState::Unknown);
                        if *entry == AutoConnectState::Unknown {
                            *entry = AutoConnectState::Dialing;
                            Some(inner.dialer.clone())
                        } else {
                            None
                        }
                    };
                    if let Some(dialer) = dialer_to_call {
                        dialer.ensure_direct(peer).await;
                    }
                }
                Some(ev) = life_sub.next() => {
                    if ev.state == LivenessState::Stale {
                        let Some(inner) = weak.upgrade() else { return; };
                        let mut state = inner.auto_connect_state.borrow_mut();
                        state.insert(ev.peer.clone(), AutoConnectState::Unknown);
                        drop(state);
                        // Drop per-peer playback resources.
                        inner.frame_sink.drop_peer(&ev.peer);
                        // Drop per-peer jitter buffer so re-entry starts fresh.
                        inner.jitter.borrow_mut().remove(&ev.peer);
                        inner.last_delivered.borrow_mut().remove(&ev.peer);
                    }
                }
                else => return,
            }
        }
    }
    .boxed_local()
}
