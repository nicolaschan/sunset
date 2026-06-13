//! Relay re-forwards ephemeral datagrams across a star topology where the
//! two leaves never connect directly. A — R — B: A and B each connect only
//! to R. B subscribes to A's voice stream *via R*; A publishes one ephemeral
//! datagram; B receives it because R re-forwarded it (Layer-1 re-forward),
//! and R's `ephemeral_forwarded` counter proves the relay actually carried
//! it. The A–B leg is never wired, so a direct delivery is impossible.
//!
//! The star is wired by the shared `relay_star_publish_one` helper; this
//! test asserts the receive-side outcome.

#![cfg(feature = "test-helpers")]

use std::time::Duration;

use sunset_sync::FrameVia;
use sunset_sync::test_helpers::relay_star_publish_one;
use sunset_sync::test_transport::TestNetwork;

#[tokio::test(flavor = "current_thread")]
async fn relay_reforwards_ephemeral_to_indirect_peer() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let net = TestNetwork::new();
            let mut star = relay_star_publish_one(&net).await;

            // B receives the datagram — only possible via R's re-forward. It
            // arrived over B's session to R, so its provenance is Relay.
            let (got, via) = tokio::time::timeout(Duration::from_secs(2), star.b_sub.recv())
                .await
                .expect("ephemeral arrived in time")
                .expect("subscription open");
            assert_eq!(got, star.datagram);
            assert_eq!(via, FrameVia::Relay);

            // A is not directly connected to B (R may be present).
            assert!(
                !star
                    .a
                    .engine
                    .current_peers()
                    .await
                    .iter()
                    .any(|(p, _)| *p == star.b.id),
                "A must not be directly connected to B"
            );

            // R actually re-forwarded at least once.
            assert!(
                star.r.engine.ephemeral_forwarded().await >= 1,
                "relay must have re-forwarded the datagram"
            );
        })
        .await;
}
