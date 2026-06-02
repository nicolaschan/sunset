//! `build_identity_snapshot` must source `ephemeral_forwarded` from the
//! live engine, not a constant. We drive a real relay re-forward across a
//! star topology (A — R — B, A–B never wired): R re-forwards A's ephemeral
//! datagram to B, bumping R's `ephemeral_forwarded` to 1, and assert the
//! identity snapshot the JSON `/` route is built from carries that exact
//! count. This is the harness from `sunset-sync/tests/relay_ephemeral.rs`
//! pointed at the relay's snapshot builder.

#![cfg(feature = "test-helpers")]

use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use ed25519_dalek::{Signer as _, SigningKey};

use sunset_relay::snapshot::build_identity_snapshot;
use sunset_store::{
    AcceptAllVerifier, Filter, SignedDatagram, VerifyingKey, canonical::datagram_signing_payload,
};
use sunset_store_memory::MemoryStore;
use sunset_sync::routing::{SubscriptionPolicy, relay_broad_filter};
use sunset_sync::test_transport::{TestNetwork, TestTransport};
use sunset_sync::{PeerAddr, PeerId, Signer, SyncConfig, SyncEngine, TrustSet};

/// Deterministic Ed25519 signer over a fixed seed.
struct StubSigner {
    key: SigningKey,
}

impl StubSigner {
    fn new(seed: [u8; 32]) -> Self {
        Self {
            key: SigningKey::from_bytes(&seed),
        }
    }
    fn vk(&self) -> VerifyingKey {
        VerifyingKey::new(Bytes::copy_from_slice(self.key.verifying_key().as_bytes()))
    }
    fn sign_payload(&self, payload: &[u8]) -> Bytes {
        Bytes::copy_from_slice(&self.key.sign(payload).to_bytes())
    }
}

impl Signer for StubSigner {
    fn verifying_key(&self) -> VerifyingKey {
        self.vk()
    }
    fn sign(&self, payload: &[u8]) -> Bytes {
        self.sign_payload(payload)
    }
}

fn build_engine(
    net: &TestNetwork,
    seed: [u8; 32],
    addr: &str,
) -> (Rc<SyncEngine<MemoryStore, TestTransport>>, Arc<StubSigner>) {
    let signer = Arc::new(StubSigner::new(seed));
    let local_peer = PeerId(signer.vk());
    let store = Arc::new(MemoryStore::new(Arc::new(AcceptAllVerifier)));
    let transport = net.transport(
        local_peer.clone(),
        PeerAddr::new(Bytes::copy_from_slice(addr.as_bytes())),
    );
    let engine = Rc::new(SyncEngine::new(
        store,
        transport,
        SyncConfig::default(),
        local_peer,
        signer.clone() as Arc<dyn Signer>,
    ));
    (engine, signer)
}

/// Build the identity snapshot for `engine` with throwaway identity
/// material — the field under test is `ephemeral_forwarded`.
async fn identity_of(engine: &Rc<SyncEngine<MemoryStore, TestTransport>>) -> u64 {
    build_identity_snapshot(
        engine,
        [0xab; 32],
        [0xcd; 32],
        "ws://relay.example:8443",
        None,
    )
    .await
    .ephemeral_forwarded
}

#[tokio::test(flavor = "current_thread")]
async fn identity_snapshot_reports_relay_forward_count() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let net = TestNetwork::new();
            let (a, a_signer) = build_engine(&net, [1u8; 32], "alice");
            let (r, r_signer) = build_engine(&net, [2u8; 32], "relay");
            let (b, b_signer) = build_engine(&net, [3u8; 32], "bob");

            let a_run = {
                let a = a.clone();
                tokio::task::spawn_local(async move { a.run().await })
            };
            let r_run = {
                let r = r.clone();
                tokio::task::spawn_local(async move { r.run().await })
            };
            let b_run = {
                let b = b.clone();
                tokio::task::spawn_local(async move { b.run().await })
            };

            a.set_trust(TrustSet::All).await.unwrap();
            r.set_trust(TrustSet::All).await.unwrap();
            b.set_trust(TrustSet::All).await.unwrap();

            // Before any traffic the relay's snapshot reports zero forwards.
            assert_eq!(identity_of(&r).await, 0, "fresh relay forwards nothing");

            let a_hex = hex::encode(a_signer.vk().0.as_ref());
            let a_voice_prefix = Filter::NamePrefix(Bytes::from(format!("voice/{a_hex}")));

            r.subscribe(relay_broad_filter(), SubscriptionPolicy::relay_broad())
                .await
                .unwrap();

            b.subscribe_via(
                a_voice_prefix.clone(),
                PeerId(r_signer.vk()),
                SubscriptionPolicy::store_data(),
            )
            .await
            .unwrap();
            let mut b_sub = b.subscribe_ephemeral(a_voice_prefix.clone()).await;

            // Star topology: A—R and B—R only; A—B is never wired.
            a.add_peer(PeerAddr::new(Bytes::from_static(b"relay")))
                .await
                .unwrap();
            b.add_peer(PeerAddr::new(Bytes::from_static(b"relay")))
                .await
                .unwrap();

            let b_pid = PeerId(b_signer.vk());
            assert!(
                a.wait_for_peer_interest(
                    &PeerId(r_signer.vk()),
                    &relay_broad_filter(),
                    Duration::from_secs(2),
                )
                .await,
                "A never armed R's broad interest"
            );
            assert!(
                r.wait_for_peer_interest(&b_pid, &a_voice_prefix, Duration::from_secs(2))
                    .await,
                "R never armed B's voice interest"
            );

            // A publishes one ephemeral datagram; R re-forwards it to B.
            let name = Bytes::from(format!("voice/{a_hex}/0001"));
            let payload = Bytes::from_static(b"opus-frame-bytes");
            let unsigned = SignedDatagram {
                verifying_key: a_signer.vk(),
                name: name.clone(),
                payload: payload.clone(),
                seq: 0,
                signature: Bytes::new(),
            };
            let sig = a_signer.sign_payload(&datagram_signing_payload(&unsigned));
            let datagram = SignedDatagram {
                seq: 0,
                signature: sig,
                ..unsigned
            };
            a.publish_ephemeral(datagram.clone()).await.unwrap();

            // B receiving the datagram proves R re-forwarded it; that is
            // the user-observable event the snapshot's counter records. It
            // arrived over B's session to R, so its provenance is Relay.
            let (got, via) = tokio::time::timeout(Duration::from_secs(2), b_sub.recv())
                .await
                .expect("ephemeral arrived in time")
                .expect("subscription open");
            assert_eq!(got, datagram);
            assert_eq!(via, sunset_sync::FrameVia::Relay);

            // The snapshot must mirror the relay's live forward count, and
            // it must have actually risen off zero.
            let snapshot_count = identity_of(&r).await;
            assert_eq!(
                snapshot_count,
                r.ephemeral_forwarded().await,
                "snapshot must mirror the engine's live forward count, not a constant",
            );
            assert!(
                snapshot_count >= 1,
                "snapshot should reflect the real re-forward (got {snapshot_count})",
            );

            a_run.abort();
            r_run.abort();
            b_run.abort();
        })
        .await;
}
