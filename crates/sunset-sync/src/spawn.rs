//! `spawn_local` shim that works on both native (tokio LocalSet) and
//! `wasm32-unknown-unknown` (browser microtask queue).
//!
//! On native: `JoinHandle<T>` is exactly `tokio::task::JoinHandle<T>` so
//! callers can `.await` it directly.
//!
//! On wasm: `JoinHandle<T>` is a placeholder unit struct — wasm-bindgen-
//! futures doesn't surface a join handle, and browser microtasks can't be
//! cancelled or awaited individually. Wasm callers should not `.await`
//! the handle (it doesn't impl Future); callers that need to abort can
//! call `.abort()` which is a no-op on wasm.

use std::future::Future;

#[cfg(not(target_arch = "wasm32"))]
pub type JoinHandle<T> = tokio::task::JoinHandle<T>;

#[cfg(target_arch = "wasm32")]
pub struct JoinHandle<T> {
    _marker: std::marker::PhantomData<T>,
}

#[cfg(target_arch = "wasm32")]
impl<T> JoinHandle<T> {
    /// No-op on wasm — browser microtasks can't be cancelled.
    pub fn abort(&self) {}
}

#[cfg(not(target_arch = "wasm32"))]
pub fn spawn_local<F>(future: F) -> JoinHandle<F::Output>
where
    F: Future + 'static,
{
    tokio::task::spawn_local(future)
}

#[cfg(target_arch = "wasm32")]
pub fn spawn_local<F>(future: F) -> JoinHandle<F::Output>
where
    F: Future<Output = ()> + 'static,
{
    wasm_bindgen_futures::spawn_local(future);
    JoinHandle {
        _marker: std::marker::PhantomData,
    }
}
