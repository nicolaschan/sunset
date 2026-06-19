//! Regression: a `SubscriptionEntry::Withdrawn` is scoped to the provider it
//! names, exactly like `Active`.
//!
//! A receiver may subscribe to the SAME filter via two providers at once —
//! the relay-audio-fallback co-arm subscribes `voice/<sender>` via the direct
//! peer AND via the relay so the relay keeps delivering until the direct path
//! is proven. Withdrawing one of those subscriptions must not disturb the
//! interest armed by the other.
//!
//! The hazard is structural: the per-peer interest map is keyed by
//! `FilterHash` (provider-agnostic), `Active` is only honoured when it names
//! this engine as provider (`provider == self.local_peer`), but `Withdrawn`
//! carries no provider in its value — so without reading the provider from the
//! entry *name*, an engine would tear down its interest on a `Withdrawn` meant
//! for a *different* provider. Since a self-authored `Withdrawn` is broadcast
//! to every connected peer, the direct provider sees the relay's withdrawal
//! and (pre-fix) wrongly dropped the receiver's still-active direct interest —
//! silently cutting off forwarding (the "B can't hear A" voice bug).

#![cfg(feature = "test-helpers")]

use std::time::Duration;

use bytes::Bytes;
use sunset_store::Filter;
use sunset_sync::routing::SubscriptionPolicy;
use sunset_sync::test_helpers::{TestPeer, vk};
use sunset_sync::test_transport::TestNetwork;
use sunset_sync::{PeerId, TrustSet};

#[tokio::test(flavor = "current_thread")]
async fn withdraw_for_other_provider_keeps_this_providers_interest() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let net = TestNetwork::new();
            let alice = TestPeer::spawn(&net, b"alice"); // the direct provider
            let bob = TestPeer::spawn(&net, b"bob"); // the receiver
            alice.engine.set_trust(TrustSet::All).await.unwrap();
            bob.engine.set_trust(TrustSet::All).await.unwrap();

            bob.engine.add_peer(alice.addr.clone()).await.unwrap();

            let filter = Filter::NamePrefix(Bytes::from_static(b"voice/room/sender"));
            // A second provider the receiver also subscribes through. It is
            // never connected — only its identity matters, because it appears
            // in the subscription entry name.
            let relay_id = PeerId(vk(b"relay"));

            // Co-arm: subscribe to `filter` via alice (direct) AND via relay.
            bob.engine
                .subscribe_via(
                    filter.clone(),
                    alice.id.clone(),
                    SubscriptionPolicy::store_data(),
                )
                .await
                .unwrap();
            bob.engine
                .subscribe_via(
                    filter.clone(),
                    relay_id.clone(),
                    SubscriptionPolicy::store_data(),
                )
                .await
                .unwrap();

            // Alice arms bob's interest from the direct (provider=alice) sub.
            assert!(
                alice
                    .engine
                    .wait_for_peer_interest(&bob.id, &filter, Duration::from_secs(2))
                    .await,
                "alice did not arm bob's interest from the direct subscription"
            );

            // The receiver withdraws ONLY the relay subscription.
            bob.engine
                .unsubscribe_via(filter.clone(), relay_id.clone())
                .await
                .unwrap();

            // Fence: a fresh subscription via alice, published AFTER the relay
            // withdrawal over the same FIFO connection. Once alice arms it, the
            // relay withdrawal has necessarily already been processed — so the
            // assertion below cannot race ahead of the withdrawal it guards.
            let fence = Filter::NamePrefix(Bytes::from_static(b"voice/room/fence"));
            bob.engine
                .subscribe_via(
                    fence.clone(),
                    alice.id.clone(),
                    SubscriptionPolicy::store_data(),
                )
                .await
                .unwrap();
            assert!(
                alice
                    .engine
                    .wait_for_peer_interest(&bob.id, &fence, Duration::from_secs(2))
                    .await,
                "fence subscription never armed — cannot conclude the withdrawal was processed"
            );

            // The direct interest must survive: a withdrawal naming a DIFFERENT
            // provider must not tear down the interest alice armed for itself.
            let subs = alice.engine.subscriptions_snapshot().await;
            assert!(
                subs.iter().any(|(p, f)| *p == bob.id && *f == filter),
                "alice dropped bob's direct interest after a withdrawal for a \
                 different provider (the relay); subscriptions = {subs:?}"
            );
        })
        .await;
}
