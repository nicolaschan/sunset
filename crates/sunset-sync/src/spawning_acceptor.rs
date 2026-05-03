//! `SpawningAcceptor` — a `Transport` decorator that runs each inbound
//! connection's promotion (the slow per-connection work between a raw
//! socket and a usable, authenticated connection) on its own task.
//!
//! This is the structural fix for the inbound-pipeline serialization
//! that affects engine accept loops. Without this wrapper, a single
//! slow client at any post-upgrade stage (Noise IK responder, future
//! TLS termination, anti-DoS challenge, etc.) holds the engine's accept
//! arm captive; with it, each promotion runs on its own `spawn_local`'d
//! task and successes land on a channel that `accept()` drains.
//!
//! The wrapper is generic over the promotion callback so it doesn't
//! depend on any specific cryptography. The caller wires up the
//! callback (e.g. the relay binary passes
//! `sunset_noise::do_handshake_responder`).

use std::future::Future;
use std::marker::PhantomData;
use std::rc::Rc;
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::{Mutex, mpsc};

use crate::error::{Error, Result};
use crate::spawn::{JoinHandle, spawn_local};
use crate::transport::{RawTransport, Transport, TransportConnection};
use crate::types::PeerAddr;

/// Wraps a server-side `RawTransport`, a connector `Transport` (used for
/// outbound `connect()` calls), and a "promote" callback that turns a
/// `RawConnection` into an authenticated `Connection`. On construction
/// it eagerly spawns a pump task: every successful `raw.accept()` is
/// handed to a fresh `spawn_local`'d task that runs `promote` under a
/// per-task timeout. Successes go to an internal mpsc; `Transport::accept()`
/// drains that mpsc.
///
/// The connector and the raw acceptor can — but need not — wrap the same
/// underlying machinery. In the relay's typical wiring the connector is
/// `NoiseTransport<WebSocketRawTransport::dial_only>` and the raw side is
/// `WebSocketRawTransport::serving()`.
pub struct SpawningAcceptor<R, T, F, Fut, C>
where
    R: RawTransport + 'static,
    R::Connection: 'static,
    T: Transport<Connection = C> + 'static,
    F: Fn(R::Connection) -> Fut + 'static,
    Fut: Future<Output = Result<C>> + 'static,
    C: TransportConnection + 'static,
{
    connector: Rc<T>,
    auth_rx: Mutex<mpsc::UnboundedReceiver<C>>,
    /// Held so the pump task can be aborted when SpawningAcceptor drops.
    /// Without an explicit abort the pump would detach (tokio's default)
    /// and continue calling raw.accept() forever; the manual Drop impl
    /// below ensures RAII cleanup.
    _pump: JoinHandle<()>,
    _markers: PhantomData<(R, F, Fut)>,
}

impl<R, T, F, Fut, C> SpawningAcceptor<R, T, F, Fut, C>
where
    R: RawTransport + 'static,
    R::Connection: 'static,
    T: Transport<Connection = C> + 'static,
    F: Fn(R::Connection) -> Fut + 'static,
    Fut: Future<Output = Result<C>> + 'static,
    C: TransportConnection + 'static,
{
    /// Construct + start the pump. `handshake_timeout` bounds each
    /// individual `promote` future; on timeout the in-flight raw
    /// connection is dropped (closing its underlying socket).
    pub fn new(raw: R, connector: T, promote: F, handshake_timeout: Duration) -> Self {
        let raw = Rc::new(raw);
        let promote = Rc::new(promote);
        let (auth_tx, auth_rx) = mpsc::unbounded_channel::<C>();
        let pump = spawn_local(pump_loop(raw, promote, auth_tx, handshake_timeout));
        Self {
            connector: Rc::new(connector),
            auth_rx: Mutex::new(auth_rx),
            _pump: pump,
            _markers: PhantomData,
        }
    }
}

impl<R, T, F, Fut, C> Drop for SpawningAcceptor<R, T, F, Fut, C>
where
    R: RawTransport + 'static,
    R::Connection: 'static,
    T: Transport<Connection = C> + 'static,
    F: Fn(R::Connection) -> Fut + 'static,
    Fut: Future<Output = Result<C>> + 'static,
    C: TransportConnection + 'static,
{
    fn drop(&mut self) {
        self._pump.abort();
    }
}

async fn pump_loop<R, F, Fut, C>(
    raw: Rc<R>,
    promote: Rc<F>,
    auth_tx: mpsc::UnboundedSender<C>,
    handshake_timeout: Duration,
) where
    R: RawTransport + 'static,
    R::Connection: 'static,
    F: Fn(R::Connection) -> Fut + 'static,
    Fut: Future<Output = Result<C>> + 'static,
    C: TransportConnection + 'static,
{
    /// Pump exits after this many back-to-back accept errors. With
    /// the 100 ms backoff per error, that's ~1.6 seconds of tight
    /// failure before we conclude the underlying transport is dead.
    const MAX_CONSECUTIVE_ACCEPT_ERRORS: u32 = 16;

    let mut consecutive_errors: u32 = 0;
    loop {
        match raw.accept().await {
            Ok(rc) => {
                consecutive_errors = 0;
                let auth_tx = auth_tx.clone();
                let promote = promote.clone();
                spawn_local(async move {
                    match with_timeout(handshake_timeout, promote(rc)).await {
                        Some(Ok(conn)) => {
                            if auth_tx.send(conn).is_err() {
                                eprintln!(
                                    "sunset-sync: accepted connection arrived after acceptor was dropped; discarding"
                                );
                            }
                        }
                        Some(Err(e)) => {
                            eprintln!("sunset-sync: promote failed: {e}");
                        }
                        None => {
                            eprintln!(
                                "sunset-sync: promote timed out after {:?}; dropping",
                                handshake_timeout,
                            );
                        }
                    }
                });
            }
            Err(e) => {
                consecutive_errors += 1;
                eprintln!(
                    "sunset-sync: raw accept failed: {e} (consecutive: {consecutive_errors}); continuing"
                );
                if consecutive_errors >= MAX_CONSECUTIVE_ACCEPT_ERRORS {
                    eprintln!("sunset-sync: too many consecutive accept errors; pump exiting");
                    return;
                }
                with_sleep(Duration::from_millis(100)).await;
            }
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
async fn with_timeout<F: Future>(d: Duration, f: F) -> Option<F::Output> {
    tokio::time::timeout(d, f).await.ok()
}

#[cfg(target_arch = "wasm32")]
async fn with_timeout<F: Future>(d: Duration, f: F) -> Option<F::Output> {
    wasmtimer::tokio::timeout(d, f).await.ok()
}

#[cfg(not(target_arch = "wasm32"))]
async fn with_sleep(d: Duration) {
    tokio::time::sleep(d).await
}

#[cfg(target_arch = "wasm32")]
async fn with_sleep(d: Duration) {
    wasmtimer::tokio::sleep(d).await
}

#[async_trait(?Send)]
impl<R, T, F, Fut, C> Transport for SpawningAcceptor<R, T, F, Fut, C>
where
    R: RawTransport + 'static,
    R::Connection: 'static,
    T: Transport<Connection = C> + 'static,
    F: Fn(R::Connection) -> Fut + 'static,
    Fut: Future<Output = Result<C>> + 'static,
    C: TransportConnection + 'static,
{
    type Connection = C;

    async fn connect(&self, addr: PeerAddr) -> Result<Self::Connection> {
        self.connector.connect(addr).await
    }

    async fn accept(&self) -> Result<Self::Connection> {
        let mut rx = self.auth_rx.lock().await;
        rx.recv()
            .await
            .ok_or_else(|| Error::Transport("acceptor channel closed".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::time::Duration;
    use tokio::sync::{Mutex as AsyncMutex, mpsc};

    use crate::transport::{RawConnection, TransportKind};
    use crate::types::PeerId;

    // ---- synthetic raw transport / connection ----

    /// A `RawConnection` we never read from — it just exists. Promote
    /// closures inspect a per-conn id to decide how to behave (fast,
    /// hang forever, fail).
    struct StubRawConn {
        id: usize,
    }

    #[async_trait(?Send)]
    impl RawConnection for StubRawConn {
        async fn send_reliable(&self, _: Bytes) -> Result<()> {
            Ok(())
        }
        async fn recv_reliable(&self) -> Result<Bytes> {
            std::future::pending().await
        }
        async fn send_unreliable(&self, _: Bytes) -> Result<()> {
            Ok(())
        }
        async fn recv_unreliable(&self) -> Result<Bytes> {
            std::future::pending().await
        }
        async fn close(&self) -> Result<()> {
            Ok(())
        }
    }

    /// A `RawTransport` whose `accept()` yields `StubRawConn`s from a
    /// pre-loaded queue. After the queue is drained, `accept()` blocks
    /// forever (matching real-world "no more connections coming right
    /// now" behavior).
    struct StubRawTransport {
        queue: AsyncMutex<mpsc::UnboundedReceiver<StubRawConn>>,
    }

    impl StubRawTransport {
        fn with_ids(ids: &[usize]) -> Self {
            let (tx, rx) = mpsc::unbounded_channel();
            for &id in ids {
                tx.send(StubRawConn { id }).unwrap();
            }
            Self {
                queue: AsyncMutex::new(rx),
            }
        }
    }

    #[async_trait(?Send)]
    impl RawTransport for StubRawTransport {
        type Connection = StubRawConn;
        async fn connect(&self, _: PeerAddr) -> Result<StubRawConn> {
            Err(Error::Transport("connect not used in these tests".into()))
        }
        async fn accept(&self) -> Result<StubRawConn> {
            let mut q = self.queue.lock().await;
            q.recv()
                .await
                .ok_or_else(|| Error::Transport("queue closed".into()))
        }
    }

    // ---- a `Transport` connection that the promote produces ----

    struct StubAuthConn {
        id: usize,
    }

    #[async_trait(?Send)]
    impl TransportConnection for StubAuthConn {
        async fn send_reliable(&self, _: Bytes) -> Result<()> {
            Ok(())
        }
        async fn recv_reliable(&self) -> Result<Bytes> {
            std::future::pending().await
        }
        async fn send_unreliable(&self, _: Bytes) -> Result<()> {
            Ok(())
        }
        async fn recv_unreliable(&self) -> Result<Bytes> {
            std::future::pending().await
        }
        fn peer_id(&self) -> PeerId {
            PeerId(sunset_store::VerifyingKey::new(Bytes::from(format!(
                "stub-{}",
                self.id
            ))))
        }
        fn kind(&self) -> TransportKind {
            TransportKind::Unknown
        }
        async fn close(&self) -> Result<()> {
            Ok(())
        }
    }

    /// A connector whose `connect()` is unused in these tests.
    struct UnusedConnector;

    #[async_trait(?Send)]
    impl Transport for UnusedConnector {
        type Connection = StubAuthConn;
        async fn connect(&self, _: PeerAddr) -> Result<StubAuthConn> {
            Err(Error::Transport("connector unused in these tests".into()))
        }
        async fn accept(&self) -> Result<StubAuthConn> {
            std::future::pending().await
        }
    }

    // ---- tests ----

    /// Two slow promotes never complete; the third's promote completes
    /// promptly. Acceptor.accept() must return the third without waiting
    /// on the slow ones.
    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn slow_promotes_do_not_block_a_fast_one() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let raw = StubRawTransport::with_ids(&[1, 2, 3]);
                let connector = UnusedConnector;
                let promote = move |rc: StubRawConn| async move {
                    if rc.id == 3 {
                        // Fast.
                        Ok(StubAuthConn { id: rc.id })
                    } else {
                        // Stall forever.
                        std::future::pending::<()>().await;
                        unreachable!()
                    }
                };
                let acceptor =
                    SpawningAcceptor::new(raw, connector, promote, Duration::from_secs(60));

                // The acceptor's pump fires the three accept()s as separate
                // promote tasks. Tasks 1 and 2 hang; task 3 completes.
                // accept() should return task 3's connection.
                let conn = tokio::time::timeout(Duration::from_secs(5), acceptor.accept())
                    .await
                    .expect("accept did not return within 5 s — slow promotes blocked the fast one")
                    .expect("accept errored");
                assert_eq!(conn.id, 3);
            })
            .await;
    }

    /// Per-task timeout fires independently. With a 1 s timeout and three
    /// stalled promotes, all three tasks complete (drop+log) within ~1 s
    /// rather than serializing into 3 s.
    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn per_task_timeout_fires_independently() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let raw = StubRawTransport::with_ids(&[1, 2, 3]);
                let connector = UnusedConnector;
                let counter = Rc::new(RefCell::new(0usize));
                let counter_clone = counter.clone();
                let promote = move |_rc: StubRawConn| {
                    let counter_clone = counter_clone.clone();
                    async move {
                        // Stall, then the timeout will cancel us. The Drop
                        // of this future increments the counter.
                        struct Counter(Rc<RefCell<usize>>);
                        impl Drop for Counter {
                            fn drop(&mut self) {
                                *self.0.borrow_mut() += 1;
                            }
                        }
                        let _g = Counter(counter_clone);
                        std::future::pending::<()>().await;
                        Ok(StubAuthConn { id: 0 })
                    }
                };
                let _acceptor =
                    SpawningAcceptor::new(raw, connector, promote, Duration::from_secs(1));

                // Yield enough times for the pump to drain the 3-item queue
                // and spawn all three promote tasks (each with a 1 s timeout).
                // The pump accept()s synchronously from the in-memory queue
                // so a handful of yields is sufficient.
                for _ in 0..10 {
                    tokio::task::yield_now().await;
                }

                // Now advance the paused clock past the timeout window. All three
                // promote tasks were registered at t=0 with a 1 s deadline, so
                // advancing to 1.5 s fires all three timeouts and drops their guards.
                tokio::time::advance(Duration::from_millis(1_500)).await;
                tokio::task::yield_now().await;

                // We expect 3 drops; allow up to 5 s of real-time padding for
                // the spawn-and-cancel ladder to settle (this is paused-clock
                // mode, so the real-time bound is just a safety net).
                let start = tokio::time::Instant::now();
                while *counter.borrow() < 3 && start.elapsed() < Duration::from_secs(5) {
                    tokio::task::yield_now().await;
                }
                assert_eq!(
                    *counter.borrow(),
                    3,
                    "expected 3 promote-task drops on timeout, saw {}",
                    counter.borrow(),
                );
            })
            .await;
    }
}
