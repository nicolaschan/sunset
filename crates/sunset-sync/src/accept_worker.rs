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
use std::time::Duration;

use futures::stream::{Stream, StreamExt};
use tokio::sync::mpsc;

use crate::error::{Error, Result};
use crate::spawn::spawn_local;

async fn with_timeout<F: Future>(timeout: Duration, fut: F) -> Option<F::Output> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        tokio::time::timeout(timeout, fut).await.ok()
    }
    #[cfg(target_arch = "wasm32")]
    {
        wasmtimer::tokio::timeout(timeout, fut).await.ok()
    }
}

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
    let semaphore = std::sync::Arc::new(tokio::sync::Semaphore::new(max_inflight));

    spawn_local(async move {
        futures::pin_mut!(inbound);
        while let Some(item) = inbound.next().await {
            let fut = handshake_fn(item);
            let out_tx = out_tx.clone();
            let permit = match semaphore.clone().acquire_owned().await {
                Ok(p) => p,
                Err(_) => break, // semaphore closed; shutting down
            };
            spawn_local(async move {
                let _permit = permit;
                let result = match with_timeout(timeout, fut).await {
                    Some(r) => r,
                    None => Err(Error::Transport(format!(
                        "inbound handshake exceeded {timeout:?}"
                    ))),
                };
                let _ = out_tx.send(result);
            });
        }
    });

    out_rx
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::rc::Rc;
    use tokio_stream::wrappers::UnboundedReceiverStream;

    #[tokio::test(flavor = "current_thread")]
    async fn passes_one_item_through() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let (tx, rx) = mpsc::unbounded_channel::<u32>();
                let inbound = UnboundedReceiverStream::new(rx);
                let mut out =
                    spawn_accept_worker(inbound, Duration::from_secs(5), 16, |n: u32| async move {
                        Ok::<u32, Error>(n * 2)
                    });
                tx.send(7).unwrap();
                drop(tx);
                let got = out.recv().await.expect("one result").expect("ok");
                assert_eq!(got, 14);
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn n_slow_items_complete_concurrently() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let (tx, rx) = mpsc::unbounded_channel::<u32>();
                let inbound = UnboundedReceiverStream::new(rx);
                let per_item = Duration::from_millis(200);
                let mut out = spawn_accept_worker(
                    inbound,
                    Duration::from_secs(5),
                    16,
                    move |n: u32| async move {
                        tokio::time::sleep(per_item).await;
                        Ok::<u32, Error>(n)
                    },
                );

                let n = 8u32;
                for i in 0..n {
                    tx.send(i).unwrap();
                }
                drop(tx);

                let start = tokio::time::Instant::now();
                let mut received = 0u32;
                while received < n {
                    out.recv().await.expect("more results").expect("ok");
                    received += 1;
                }
                let elapsed = start.elapsed();

                // Sequentially this would take >=N*per_item = 1.6s.
                // With concurrent spawning it should finish near per_item.
                assert!(
                    elapsed < per_item * 3,
                    "expected concurrent execution; took {elapsed:?} for {n} items at {per_item:?} each"
                );
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn timeout_drops_a_stuck_task() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let (tx, rx) = mpsc::unbounded_channel::<()>();
                let inbound = UnboundedReceiverStream::new(rx);
                let mut out =
                    spawn_accept_worker(inbound, Duration::from_millis(50), 16, |()| async move {
                        // Never completes.
                        std::future::pending::<()>().await;
                        unreachable!();
                        #[allow(unreachable_code)]
                        Ok::<(), Error>(())
                    });
                tx.send(()).unwrap();
                let start = tokio::time::Instant::now();
                let got = out
                    .recv()
                    .await
                    .expect("a result")
                    .expect_err("expected timeout err");
                let elapsed = start.elapsed();
                assert!(matches!(got, Error::Transport(_)), "got: {got:?}");
                assert!(
                    elapsed < Duration::from_secs(1),
                    "timeout should have fired well under 1s, took {elapsed:?}"
                );
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn semaphore_caps_inflight() {
        use std::cell::Cell;
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let cap = 2usize;
                let inflight: Rc<Cell<usize>> = Rc::new(Cell::new(0));
                let max_seen: Rc<Cell<usize>> = Rc::new(Cell::new(0));

                let (tx, rx) = mpsc::unbounded_channel::<u32>();
                let inbound = UnboundedReceiverStream::new(rx);

                let inflight_for_fn = inflight.clone();
                let max_for_fn = max_seen.clone();
                let mut out =
                    spawn_accept_worker(inbound, Duration::from_secs(5), cap, move |_n: u32| {
                        let inflight = inflight_for_fn.clone();
                        let max_seen = max_for_fn.clone();
                        async move {
                            inflight.set(inflight.get() + 1);
                            let now = inflight.get();
                            if now > max_seen.get() {
                                max_seen.set(now);
                            }
                            // Hold long enough that bursts pile up.
                            tokio::time::sleep(Duration::from_millis(50)).await;
                            inflight.set(inflight.get() - 1);
                            Ok::<(), Error>(())
                        }
                    });

                for i in 0..10u32 {
                    tx.send(i).unwrap();
                }
                drop(tx);

                let mut received = 0;
                while received < 10 {
                    out.recv().await.expect("more").expect("ok");
                    received += 1;
                }

                assert!(
                    max_seen.get() <= cap,
                    "inflight peaked at {} exceeding cap {cap}",
                    max_seen.get()
                );
                assert!(
                    max_seen.get() >= 1,
                    "expected at least 1 inflight; saw {}",
                    max_seen.get()
                );
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn source_close_terminates_output_when_drained() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let (tx, rx) = mpsc::unbounded_channel::<u32>();
                let inbound = UnboundedReceiverStream::new(rx);
                let mut out =
                    spawn_accept_worker(inbound, Duration::from_secs(5), 16, |n: u32| async move {
                        Ok::<u32, Error>(n)
                    });
                for i in 0..3u32 {
                    tx.send(i).unwrap();
                }
                drop(tx);
                for _ in 0..3 {
                    out.recv().await.expect("one of three").expect("ok");
                }
                let extra = out.recv().await;
                assert!(
                    extra.is_none(),
                    "expected None after source closed; got {extra:?}"
                );
            })
            .await;
    }
}
