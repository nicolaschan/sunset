//! `spawn_local` shim that works on both native (tokio LocalSet) and
//! `wasm32-unknown-unknown` (browser microtask queue).
//!
//! On wasm: uses `wasm_bindgen_futures::spawn_local`, which schedules the
//! future on the JS microtask queue. The returned `JoinHandle` is a no-op
//! placeholder — wasm-bindgen-futures doesn't surface a join handle.
//!
//! On native: uses `tokio::task::spawn_local`, which requires a
//! `tokio::task::LocalSet` to be running. Returns the real JoinHandle so
//! callers that want to abort the task can do so.

use std::future::Future;

/// Opaque handle. On native this wraps a tokio JoinHandle; on wasm it's a
/// unit struct (the future runs to completion or until the page navigates).
pub struct JoinHandle<T> {
    #[cfg(not(target_arch = "wasm32"))]
    inner: tokio::task::JoinHandle<T>,
    #[cfg(target_arch = "wasm32")]
    _marker: std::marker::PhantomData<T>,
}

impl<T> JoinHandle<T> {
    /// Abort the task. Native: aborts the tokio task. Wasm: no-op (browser
    /// microtasks can't be cancelled once scheduled).
    pub fn abort(&self) {
        #[cfg(not(target_arch = "wasm32"))]
        self.inner.abort();
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub fn spawn_local<F>(future: F) -> JoinHandle<F::Output>
where
    F: Future + 'static,
{
    JoinHandle {
        inner: tokio::task::spawn_local(future),
    }
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
