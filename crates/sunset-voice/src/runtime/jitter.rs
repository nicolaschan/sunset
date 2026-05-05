//! Per-peer jitter pump. Every 20 ms, for every peer with a non-empty
//! buffer, pop one frame and call `FrameSink::deliver`. The runtime is
//! codec-agnostic — `(payload, codec_id)` flows through unchanged.
//!
//! Underrun policy: deliver nothing. The codec moved to the host
//! (browser WebCodecs) and re-feeding a stateful decoder the same Opus
//! packet has undefined output; the host's playback worklet pads
//! silence on its own underflow, which covers the gap acceptably.

use std::rc::Weak;

use futures::FutureExt;

use super::JITTER_PUMP_INTERVAL;
use super::state::RuntimeInner;

pub(crate) fn spawn(weak: Weak<RuntimeInner>) -> futures::future::LocalBoxFuture<'static, ()> {
    async move {
        loop {
            sleep(JITTER_PUMP_INTERVAL).await;
            let Some(inner) = weak.upgrade() else {
                return;
            };
            if *inner.deafened.borrow() {
                // Still drain so when un-deafened we don't burst stale frames.
                let mut jitter = inner.jitter.borrow_mut();
                for q in jitter.values_mut() {
                    let _ = q.pop_front();
                }
                continue;
            }
            // Snapshot frames to deliver, then deliver outside the borrow.
            let mut to_deliver: Vec<(sunset_sync::PeerId, Vec<u8>, String)> = Vec::new();
            {
                let mut jitter = inner.jitter.borrow_mut();
                for (peer, q) in jitter.iter_mut() {
                    if let Some((payload, codec_id)) = q.pop_front() {
                        to_deliver.push((peer.clone(), payload, codec_id));
                    }
                }
            }
            let sink = inner.frame_sink.borrow().clone();
            for (peer, payload, codec_id) in to_deliver {
                sink.deliver(&peer, &payload, &codec_id);
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
