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
