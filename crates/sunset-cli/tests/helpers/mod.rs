//! Shared test helpers: spin up an in-process relay; build a CLI
//! Client connected to it.
//!
//! All tests run under a single-threaded `LocalSet` (the engine is
//! `?Send`).

use std::time::Duration;

use sunset_cli::client::Client;
use sunset_relay::config::InterestFilter;
use sunset_relay::{Config as RelayConfig, Relay};
use tempfile::TempDir;

#[allow(dead_code)]
pub struct TestRelay {
    pub dial_url: String,
    pub data_dir: TempDir,
    pub engine_task: tokio::task::JoinHandle<sunset_sync::Result<()>>,
}

#[allow(dead_code)]
pub async fn spawn_relay() -> TestRelay {
    let data_dir = tempfile::tempdir().expect("tempdir");
    let identity_secret_path = data_dir.path().join("identity.bin");
    let cfg = RelayConfig {
        listen_addr: "127.0.0.1:0".parse().unwrap(),
        data_dir: data_dir.path().to_path_buf(),
        identity_secret_path,
        peers: Vec::new(),
        accept_handshake_timeout_secs: 30,
        interest_filter: InterestFilter::All,
    };
    let mut handle = Relay::start(cfg).await.expect("relay start");
    let dial_url = handle.dial_address();
    let engine_task = handle.run_for_test().await.expect("relay run_for_test");
    TestRelay {
        dial_url,
        data_dir,
        engine_task,
    }
}

#[allow(dead_code)]
pub fn fresh_client() -> std::rc::Rc<Client> {
    let mut seed = [0u8; 32];
    use rand_core::RngCore;
    rand_core::OsRng.fill_bytes(&mut seed);
    let identity = sunset_core::Identity::from_secret_bytes(&seed);
    Client::start(identity)
}

/// Wait for a closure to return `Some` within `deadline`. Polls
/// every 25 ms. Used for "eventually" assertions in integration
/// tests. Timeouts encode UX bars per CLAUDE.md — do NOT raise the
/// deadline to mask a slow path.
#[allow(dead_code)]
pub async fn eventually<F, T>(deadline: Duration, mut f: F) -> Option<T>
where
    F: FnMut() -> Option<T>,
{
    let start = std::time::Instant::now();
    loop {
        if let Some(v) = f() {
            return Some(v);
        }
        if start.elapsed() >= deadline {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}
