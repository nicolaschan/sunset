//! NoiseTransport's accept path must spawn per-connection responders
//! so a stalled Noise initiator doesn't block other peers.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use sunset_noise::{NoiseIdentity, NoiseTransport};
use sunset_sync::{
    Error as SyncError, PeerAddr, RawConnection, RawTransport, Result as SyncResult, Transport,
};
use tokio::sync::Mutex;
use zeroize::Zeroizing;

// A test transport whose accept either returns an immediately-handshakeable
// raw connection or a connection whose first read hangs forever.
struct TestRawTransport {
    queue: Arc<Mutex<Vec<TestConn>>>,
}

#[derive(Clone)]
enum TestConn {
    Healthy,
    Stalled,
}

#[async_trait(?Send)]
impl RawTransport for TestRawTransport {
    type Connection = StubRawConn;
    async fn connect(&self, _: PeerAddr) -> SyncResult<Self::Connection> {
        unreachable!()
    }
    async fn accept(&self) -> SyncResult<Self::Connection> {
        let next = {
            let mut q = self.queue.lock().await;
            q.pop()
        };
        match next {
            Some(TestConn::Healthy) => Ok(StubRawConn { stalled: false }),
            Some(TestConn::Stalled) => Ok(StubRawConn { stalled: true }),
            None => {
                std::future::pending::<()>().await;
                unreachable!()
            }
        }
    }
}

struct StubRawConn {
    stalled: bool,
}

#[async_trait(?Send)]
impl RawConnection for StubRawConn {
    async fn send_reliable(&self, _: Bytes) -> SyncResult<()> {
        Ok(())
    }
    async fn recv_reliable(&self) -> SyncResult<Bytes> {
        if self.stalled {
            std::future::pending::<()>().await;
        }
        // Returning an error makes Noise responder fail fast on healthy
        // path too; that's fine — we just want to exercise the worker
        // pool layout, not the actual handshake bytes.
        Err(SyncError::Transport("stub".into()))
    }
    async fn send_unreliable(&self, _: Bytes) -> SyncResult<()> {
        Ok(())
    }
    async fn recv_unreliable(&self) -> SyncResult<Bytes> {
        std::future::pending::<()>().await;
        unreachable!()
    }
    async fn close(&self) -> SyncResult<()> {
        Ok(())
    }
}

struct StubIdentity;

impl NoiseIdentity for StubIdentity {
    fn ed25519_public(&self) -> [u8; 32] {
        [0u8; 32]
    }
    fn ed25519_secret_seed(&self) -> Zeroizing<[u8; 32]> {
        Zeroizing::new([0u8; 32])
    }
}

#[tokio::test(flavor = "current_thread")]
async fn stalled_noise_initiator_does_not_block_others() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let queue = Arc::new(Mutex::new(vec![
                TestConn::Healthy,
                TestConn::Healthy,
                TestConn::Stalled,
                TestConn::Stalled,
                TestConn::Stalled,
            ]));
            let raw = TestRawTransport {
                queue: queue.clone(),
            };
            let noise = NoiseTransport::new_with_worker(
                raw,
                Arc::new(StubIdentity),
                Duration::from_millis(200),
                8,
            );

            // Drain 5 attempts. Stalled ones should error after timeout;
            // the test passes if all 5 attempts complete in well under
            // (5 × 200ms) — i.e. they were running concurrently.
            let start = tokio::time::Instant::now();
            for _ in 0..5 {
                let _ = noise.accept().await;
            }
            let elapsed = start.elapsed();
            assert!(
                elapsed < Duration::from_millis(800),
                "five accepts took {elapsed:?} — was Noise serialized?"
            );
        })
        .await;
}
