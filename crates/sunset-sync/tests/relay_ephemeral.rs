//! Relay re-forwards ephemeral datagrams across a star topology where the
//! two leaves never connect directly. A — R — B: A and B each connect only
//! to R. B subscribes to A's voice stream *via R*; A publishes one ephemeral
//! datagram; B receives it because R re-forwarded it (Layer-1 re-forward),
//! and R's `ephemeral_forwarded` counter proves the relay actually carried
//! it. The A–B leg is never wired, so a direct delivery is impossible.

#![cfg(feature = "test-helpers")]

use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use ed25519_dalek::{Signer as _, SigningKey};

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

#[tokio::test(flavor = "current_thread")]
async fn relay_reforwards_ephemeral_to_indirect_peer() {
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

            // A's voice stream name: voice/{A_hex}/...
            let a_hex = hex::encode(a_signer.vk().0.as_ref());
            let a_voice_prefix = Filter::NamePrefix(Bytes::from(format!("voice/{a_hex}")));

            // R broad-subscribes (acts as a relay): every peer it connects to
            // arms R's "everything" interest, so A will forward its ephemeral
            // to R.
            r.subscribe(relay_broad_filter(), SubscriptionPolicy::relay_broad())
                .await
                .unwrap();

            // B subscribes to A's voice stream *via R*: R arms B's interest in
            // voice/{A_hex}, so R re-forwards A's datagrams to B.
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

            // Wait until R has armed both legs: A's broad interest (so A
            // forwards to R) and B's voice interest (so R forwards to B).
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

            // A publishes one ephemeral datagram on its voice stream.
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

            // B receives it — only possible via R's re-forward.
            let got = tokio::time::timeout(Duration::from_secs(2), b_sub.recv())
                .await
                .expect("ephemeral arrived in time")
                .expect("subscription open");
            assert_eq!(got, datagram);

            // A is not directly connected to B (R may be present).
            assert!(
                !a.current_peers().await.iter().any(|(p, _)| *p == b_pid),
                "A must not be directly connected to B"
            );

            // R actually re-forwarded at least once.
            assert!(
                r.ephemeral_forwarded().await >= 1,
                "relay must have re-forwarded the datagram"
            );

            a_run.abort();
            r_run.abort();
            b_run.abort();
        })
        .await;
}
