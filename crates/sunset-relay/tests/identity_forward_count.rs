//! `build_identity_snapshot` must source `ephemeral_forwarded` from the
//! live engine, not a constant. We drive a real relay re-forward across a
//! star topology (A — R — B, A–B never wired): R re-forwards A's ephemeral
//! datagram to B, bumping R's `ephemeral_forwarded` to 1, and assert the
//! identity snapshot the JSON `/` route is built from carries that exact
//! count. The star is wired by the shared `relay_star_publish_one` helper.

#![cfg(feature = "test-helpers")]

use std::rc::Rc;
use std::time::Duration;

use sunset_relay::snapshot::build_identity_snapshot;
use sunset_store_memory::MemoryStore;
use sunset_sync::SyncEngine;
use sunset_sync::test_helpers::{TestPeer, relay_star_publish_one};
use sunset_sync::test_transport::{TestNetwork, TestTransport};

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

            // A fresh relay reports zero forwards: the snapshot reads the
            // live counter (which starts at 0), not a hardcoded constant.
            let fresh = TestPeer::spawn(&net, b"fresh-relay");
            assert_eq!(
                identity_of(&fresh.engine).await,
                0,
                "fresh relay forwards nothing"
            );

            // Drive a real A—R—B re-forward.
            let mut star = relay_star_publish_one(&net).await;

            // B receiving the datagram proves R re-forwarded it; that is the
            // user-observable event the snapshot's counter records.
            let (_got, via) = tokio::time::timeout(Duration::from_secs(2), star.b_sub.recv())
                .await
                .expect("ephemeral arrived in time")
                .expect("subscription open");
            assert_eq!(via, sunset_sync::FrameVia::Relay);

            // The snapshot must mirror the relay's live forward count, and it
            // must have actually risen off zero.
            let live = star.r.engine.ephemeral_forwarded().await;
            let snapshot_count = identity_of(&star.r.engine).await;
            assert_eq!(
                snapshot_count, live,
                "snapshot must mirror the engine's live forward count, not a constant",
            );
            assert!(
                snapshot_count >= 1,
                "snapshot should reflect the real re-forward (got {snapshot_count})",
            );
        })
        .await;
}
