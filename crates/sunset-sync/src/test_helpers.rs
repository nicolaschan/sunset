//! Shared test helpers for sunset-sync integration tests and downstream
//! crates that drive the engine end-to-end.
//!
//! Gated by the `test-helpers` feature so production builds don't pull
//! these in.

use std::time::Duration;

/// Poll `condition` until it returns `true` or the deadline elapses.
///
/// Returns `true` if `condition` returned `true` within `deadline`, and
/// `false` if the deadline elapsed first. Between attempts, sleeps for
/// `interval`. The condition is awaited on each iteration, so it may
/// perform async work (e.g. acquiring a store snapshot).
pub async fn wait_for<F, Fut>(deadline: Duration, interval: Duration, mut condition: F) -> bool
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let start = tokio::time::Instant::now();
    while start.elapsed() < deadline {
        if condition().await {
            return true;
        }
        tokio::time::sleep(interval).await;
    }
    false
}
