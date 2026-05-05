//! Per-peer jitter buffer pump. Every 20 ms, for every peer with a
//! non-empty buffer (or a `last_delivered`), pop one frame and call
//! FrameSink::deliver. Underrun → repeat last → silence.

use std::rc::Weak;

use futures::FutureExt;

use super::JITTER_PUMP_INTERVAL;
use super::state::{LastDelivered, RuntimeInner};
use crate::FRAME_SAMPLES;

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
            // Snapshot peers to deliver, then deliver outside the borrow.
            let mut to_deliver: Vec<(sunset_sync::PeerId, Vec<f32>)> = Vec::new();
            {
                let mut jitter = inner.jitter.borrow_mut();
                let mut last = inner.last_delivered.borrow_mut();
                for (peer, q) in jitter.iter_mut() {
                    if let Some(frame) = q.pop_front() {
                        // Real frame delivered — reset underrun counter.
                        let rec = last.entry(peer.clone()).or_insert(LastDelivered {
                            pcm: frame.clone(),
                            underruns: 0,
                        });
                        rec.pcm = frame.clone();
                        rec.underruns = 0;
                        to_deliver.push((peer.clone(), frame));
                    } else if let Some(rec) = last.get_mut(peer) {
                        // Underrun: repeat once, then silence.
                        rec.underruns = rec.underruns.saturating_add(1);
                        let pcm = if rec.underruns == 1 {
                            rec.pcm.clone()
                        } else {
                            vec![0.0_f32; FRAME_SAMPLES]
                        };
                        to_deliver.push((peer.clone(), pcm));
                    }
                }
            }
            let sink = inner.frame_sink.borrow().clone();
            for (peer, pcm) in to_deliver {
                sink.deliver(&peer, &pcm);
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
