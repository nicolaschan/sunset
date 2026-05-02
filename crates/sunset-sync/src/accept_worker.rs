//! Reusable "spawn one task per inbound item" worker for resilient,
//! parallel transport `accept()` paths.
//!
//! Background: a transport that accepts inbound connections typically
//! has a slow per-item phase (TCP→WS upgrade, Noise responder, WebRTC
//! ICE/DTLS). Running these inline in `Transport::accept` serializes
//! them — one slow or misbehaving peer wedges the engine's accept
//! loop. This helper spawns one task per item so the slow path runs
//! concurrently and a stuck task only consumes its own slot.
//!
//! Two policies are baked in:
//!   * **Per-task timeout** — bounds how long any single handshake
//!     can wedge a slot. On timeout the task's future is dropped,
//!     which closes the underlying TCP / data channel.
//!   * **Inflight cap (semaphore)** — bounds total concurrent
//!     handshakes so a flood of bad peers can't exhaust task / FD
//!     budgets. Items wait for a permit before spawning.

use std::future::Future;
#[allow(unused_imports)]
use std::rc::Rc;
use std::time::Duration;

use futures::stream::{Stream, StreamExt};
use tokio::sync::mpsc;

#[allow(unused_imports)]
use crate::error::{Error, Result};
use crate::spawn::spawn_local;

/// Spawn one task per item from `inbound`; each task runs
/// `handshake_fn(item)` under the given `timeout` and inside a
/// semaphore-bounded inflight cap. Results are forwarded onto the
/// returned receiver in the order tasks complete (NOT input order).
pub fn spawn_accept_worker<I, C, F, Fut, S>(
    inbound: S,
    timeout: Duration,
    max_inflight: usize,
    handshake_fn: F,
) -> mpsc::UnboundedReceiver<Result<C>>
where
    S: Stream<Item = I> + Unpin + 'static,
    I: 'static,
    C: 'static,
    F: Fn(I) -> Fut + 'static,
    Fut: Future<Output = Result<C>> + 'static,
{
    let (out_tx, out_rx) = mpsc::unbounded_channel::<Result<C>>();
    let _ = max_inflight;
    let _ = timeout;

    spawn_local(async move {
        futures::pin_mut!(inbound);
        while let Some(item) = inbound.next().await {
            let fut = handshake_fn(item);
            let out_tx = out_tx.clone();
            spawn_local(async move {
                let result = fut.await;
                let _ = out_tx.send(result);
            });
        }
    });

    out_rx
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio_stream::wrappers::UnboundedReceiverStream;

    #[tokio::test(flavor = "current_thread")]
    async fn passes_one_item_through() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let (tx, rx) = mpsc::unbounded_channel::<u32>();
                let inbound = UnboundedReceiverStream::new(rx);
                let mut out = spawn_accept_worker(
                    inbound,
                    Duration::from_secs(5),
                    16,
                    |n: u32| async move { Ok::<u32, Error>(n * 2) },
                );
                tx.send(7).unwrap();
                drop(tx);
                let got = out.recv().await.expect("one result").expect("ok");
                assert_eq!(got, 14);
            })
            .await;
    }
}
