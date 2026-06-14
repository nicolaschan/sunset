//! End-to-end ephemeral delivery between two real engines connected
//! via TestTransport. Verifies the wire path: subscriber publishes
//! filter → publisher's engine routes EphemeralDelivery via
//! unreliable channel → subscriber's engine verifies signature +
//! dispatches to local subscribe_ephemeral receiver.

#![cfg(feature = "test-helpers")]

use std::time::Duration;

use bytes::Bytes;

use sunset_store::{Filter, SignedDatagram};
use sunset_sync::routing::SubscriptionPolicy;
use sunset_sync::test_helpers::TestPeer;
use sunset_sync::test_transport::TestNetwork;
use sunset_sync::{FrameVia, TrustSet};

#[tokio::test(flavor = "current_thread")]
async fn ephemeral_routes_subscriber_match() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let net = TestNetwork::new();
            let alice = TestPeer::spawn(&net, b"alice");
            let bob = TestPeer::spawn(&net, b"bob");

            alice.engine.set_trust(TrustSet::All).await.unwrap();
            bob.engine.set_trust(TrustSet::All).await.unwrap();

            // Bob subscribes to voice/ FIRST so the per-(filter,
            // provider=alice) SubscriptionEntry is in Bob's store before
            // alice connects. After PeerHello, sync replicates that entry to
            // alice; alice's engine arms the forward path for (bob, voice).
            let voice_filter = Filter::NamePrefix(Bytes::from_static(b"voice/"));
            bob.engine
                .subscribe(voice_filter.clone(), SubscriptionPolicy::store_data())
                .await
                .unwrap();
            let mut bob_sub = bob.engine.subscribe_ephemeral(voice_filter.clone()).await;

            // Connect alice → bob (triggers PeerHello + bootstrap digest).
            alice.engine.add_peer(bob.addr.clone()).await.unwrap();

            assert!(
                alice
                    .engine
                    .wait_for_peer_interest(&bob.id, &voice_filter, Duration::from_secs(2))
                    .await,
                "alice did not arm bob's interest"
            );

            // Alice publishes an ephemeral datagram on voice/. The accept-all
            // verifier admits the stub (zero) signature, matching the
            // established sync-test signing model.
            let datagram = SignedDatagram {
                verifying_key: alice.id.0.clone(),
                name: Bytes::from_static(b"voice/alice/0001"),
                payload: Bytes::from_static(b"opus-frame-bytes"),
                seq: 0,
                signature: Bytes::from_static(&[0u8; 64]),
            };
            alice
                .engine
                .publish_ephemeral(datagram.clone())
                .await
                .unwrap();

            // Bob's subscriber should receive within a reasonable window. The
            // frame crossed the (Unknown-kind TestTransport) inbound peer
            // session, so its provenance is Relay — only a Secondary session
            // maps to Direct.
            let (got, via) = tokio::time::timeout(Duration::from_millis(500), bob_sub.recv())
                .await
                .expect("ephemeral arrived in time")
                .expect("subscription open");
            assert_eq!(got, datagram);
            assert_eq!(via, FrameVia::Relay);
        })
        .await;
}
