//! Shared test helpers for sunset-sync integration tests and downstream
//! crates that drive the engine end-to-end.
//!
//! Gated by the `test-helpers` feature so production builds don't pull
//! these in.

use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use tokio::sync::mpsc;

use sunset_store::{ContentBlock, Filter, SignedDatagram, SignedKvEntry, Store as _, VerifyingKey};
use sunset_store_memory::MemoryStore;

use crate::routing::{SubscriptionPolicy, relay_broad_filter};
use crate::spawn::{JoinHandle, spawn_local};
use crate::test_transport::{TestNetwork, TestTransport};
use crate::transport::FrameVia;
use crate::types::{PeerAddr, PeerId, SyncConfig};
use crate::{Signer, SyncEngine, TrustSet};

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

/// Build a `VerifyingKey` from a byte label. Useful for tests that mint
/// peer ids and writer-keys from short literals like `b"alice"`.
pub fn vk(b: &[u8]) -> VerifyingKey {
    VerifyingKey::new(Bytes::copy_from_slice(b))
}

/// A `Signer` that returns a 64-byte zero "signature". Receivers must use
/// `AcceptAllVerifier` for entries it signs to be admitted; the actual
/// signing scheme is not exercised. Adequate for sync-layer tests that
/// only care about replication, not signature verification.
pub struct StubSigner {
    vk: VerifyingKey,
}

impl StubSigner {
    pub fn new(vk: VerifyingKey) -> Self {
        Self { vk }
    }
}

impl Signer for StubSigner {
    fn verifying_key(&self) -> VerifyingKey {
        self.vk.clone()
    }
    fn sign(&self, _payload: &[u8]) -> Bytes {
        Bytes::from_static(&[0u8; 64])
    }
}

/// One end-to-end-wired peer for a multi-peer integration test:
/// `MemoryStore` (accept-all verifier), `StubSigner` (zero signature),
/// `TestTransport` registered on the supplied `TestNetwork`, and a
/// `SyncEngine` whose `run()` loop is already polling on the current
/// `LocalSet`.
///
/// Dropping `TestPeer` aborts the engine task — tests don't need to
/// `abort()` the run handle manually.
pub struct TestPeer {
    pub id: PeerId,
    pub addr: PeerAddr,
    pub engine: Rc<SyncEngine<MemoryStore, TestTransport>>,
    pub store: Arc<MemoryStore>,
    run_handle: JoinHandle<crate::Result<()>>,
}

impl TestPeer {
    /// Spawn a peer with identity and address derived from `label`. The
    /// peer's `PeerId` is `vk(label)`; its `PeerAddr` is `label` bytes.
    ///
    /// Must be called inside a `LocalSet` — internally calls
    /// `spawn_local` to drive `engine.run()`.
    pub fn spawn(net: &TestNetwork, label: &[u8]) -> Self {
        Self::spawn_with_config(net, label, SyncConfig::default())
    }

    /// Same as `spawn`, but with a caller-supplied `SyncConfig` (e.g.
    /// for tests that tune heartbeat intervals).
    pub fn spawn_with_config(net: &TestNetwork, label: &[u8], config: SyncConfig) -> Self {
        let id = PeerId(vk(label));
        let addr = PeerAddr::new(Bytes::copy_from_slice(label));
        let store = Arc::new(MemoryStore::with_accept_all());
        let transport = net.transport(id.clone(), addr.clone());
        let signer: Arc<dyn Signer> = Arc::new(StubSigner::new(id.0.clone()));
        let engine = Rc::new(SyncEngine::new(
            store.clone(),
            transport,
            config,
            id.clone(),
            signer,
        ));
        let run_handle = spawn_local({
            let engine = engine.clone();
            async move { engine.run().await }
        });
        Self {
            id,
            addr,
            engine,
            store,
            run_handle,
        }
    }
}

impl Drop for TestPeer {
    fn drop(&mut self) {
        self.run_handle.abort();
    }
}

/// Build a `(SignedKvEntry, ContentBlock)` pair for a single value.
///
/// The entry's `value_hash` is set to `block.hash()`, `priority` to the
/// supplied value, no expiry, and an empty signature (callers should be
/// inserting against `AcceptAllVerifier`).
pub fn make_entry(
    writer: &VerifyingKey,
    name: &[u8],
    value: &[u8],
    priority: u64,
) -> (SignedKvEntry, ContentBlock) {
    let block = ContentBlock {
        data: Bytes::copy_from_slice(value),
        references: vec![],
    };
    let entry = SignedKvEntry {
        verifying_key: writer.clone(),
        name: Bytes::copy_from_slice(name),
        value_hash: block.hash(),
        priority,
        expires_at: None,
        signature: Bytes::new(),
    };
    (entry, block)
}

/// Poll `store` until an entry under `(writer, name)` exists, or the
/// deadline elapses. Returns `true` on success.
pub async fn wait_for_entry(
    store: &MemoryStore,
    writer: &VerifyingKey,
    name: &[u8],
    deadline: Duration,
) -> bool {
    wait_for(deadline, Duration::from_millis(20), || async {
        store.get_entry(writer, name).await.unwrap().is_some()
    })
    .await
}

/// A wired-up relay-fallback star (A — R — B) with one ephemeral datagram
/// already re-forwarded from A to B through R. The leaves never connect to
/// each other, so B's only path to A's voice is R's re-forward.
///
/// Holds the three `TestPeer`s so their engine run-loops stay alive for the
/// life of the value; drop it (end of test) to tear the star down.
pub struct RelayStar {
    pub a: TestPeer,
    pub r: TestPeer,
    pub b: TestPeer,
    /// B's ephemeral subscription to A's voice stream (armed via R). Recv to
    /// observe the re-forwarded datagram and its `FrameVia` provenance.
    pub b_sub: mpsc::UnboundedReceiver<(SignedDatagram, FrameVia)>,
    /// The exact datagram A published, for an equality check on receipt.
    pub datagram: SignedDatagram,
}

/// Build an A — R — B star where the two leaves connect only to R, arm R as
/// a broad relay and B's interest in A's voice stream *via R*, wait until
/// both forwarding legs are armed, then publish one ephemeral datagram from
/// A. A — B is never wired, so B receiving the datagram proves R re-forwarded
/// it. Used by the relay-ephemeral re-forward test and the relay's
/// identity-snapshot forward-count test.
///
/// Must run inside a `LocalSet`. Panics if a forwarding leg fails to arm
/// within two seconds — that is a harness wiring failure, not the behaviour
/// under test.
pub async fn relay_star_publish_one(net: &TestNetwork) -> RelayStar {
    let a = TestPeer::spawn(net, b"alice");
    let r = TestPeer::spawn(net, b"relay");
    let b = TestPeer::spawn(net, b"bob");

    a.engine.set_trust(TrustSet::All).await.unwrap();
    r.engine.set_trust(TrustSet::All).await.unwrap();
    b.engine.set_trust(TrustSet::All).await.unwrap();

    // A's voice stream lives under voice/{A_hex}/...
    let a_hex = hex::encode(a.id.0.as_bytes());
    let a_voice_prefix = Filter::NamePrefix(Bytes::from(format!("voice/{a_hex}")));

    // R broad-subscribes (acts as the relay): every peer it connects to arms
    // R's "everything" interest, so A forwards its ephemeral to R.
    r.engine
        .subscribe(relay_broad_filter(), SubscriptionPolicy::relay_broad())
        .await
        .unwrap();
    // B subscribes to A's voice stream via R, so R re-forwards A's datagrams.
    b.engine
        .subscribe_via(
            a_voice_prefix.clone(),
            r.id.clone(),
            SubscriptionPolicy::store_data(),
        )
        .await
        .unwrap();
    let b_sub = b.engine.subscribe_ephemeral(a_voice_prefix.clone()).await;

    // Star topology: A—R and B—R only; A—B is never wired.
    a.engine.add_peer(r.addr.clone()).await.unwrap();
    b.engine.add_peer(r.addr.clone()).await.unwrap();

    // Wait until R has armed both legs: A's broad interest (so A forwards to
    // R) and B's voice interest (so R forwards to B).
    assert!(
        a.engine
            .wait_for_peer_interest(&r.id, &relay_broad_filter(), Duration::from_secs(2))
            .await,
        "A never armed R's broad interest"
    );
    assert!(
        r.engine
            .wait_for_peer_interest(&b.id, &a_voice_prefix, Duration::from_secs(2))
            .await,
        "R never armed B's voice interest"
    );

    // A publishes one ephemeral datagram on its voice stream. The accept-all
    // verifier admits the stub (zero) signature, matching the established
    // sync-test signing model.
    let datagram = SignedDatagram {
        verifying_key: a.id.0.clone(),
        name: Bytes::from(format!("voice/{a_hex}/0001")),
        payload: Bytes::from_static(b"opus-frame-bytes"),
        seq: 0,
        signature: Bytes::from_static(&[0u8; 64]),
    };
    a.engine.publish_ephemeral(datagram.clone()).await.unwrap();

    RelayStar {
        a,
        r,
        b,
        b_sub,
        datagram,
    }
}
