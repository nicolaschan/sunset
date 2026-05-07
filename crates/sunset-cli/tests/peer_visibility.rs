//! After both clients join the same room over WS, each should see
//! the other in `members` with `connection_mode == "via_relay"`.
//! No native WebRTC means peers cannot upgrade to "direct".

mod helpers;

use std::time::Duration;

use helpers::{eventually, fresh_client, spawn_relay};

#[tokio::test(flavor = "current_thread")]
async fn members_show_via_relay() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let relay = spawn_relay().await;
            let url = relay.dial_url.clone();

            let alice = fresh_client();
            let bob = fresh_client();
            alice.set_self_name("alice");
            bob.set_self_name("bob");

            alice.add_relay(url.clone()).await.unwrap();
            bob.add_relay(url.clone()).await.unwrap();

            alice.join_room("alpha").await.unwrap();
            bob.join_room("alpha").await.unwrap();

            let bob_pk = bob.identity.public().as_bytes();

            let saw = eventually(Duration::from_secs(10), || {
                let v = alice.snapshot_room("alpha")?;
                let row = v.members.iter().find(|m| m.pubkey == bob_pk)?;
                if row.connection_mode == "via_relay" {
                    Some(())
                } else {
                    None
                }
            })
            .await;
            assert!(saw.is_some(), "alice never saw bob with via_relay mode");
        })
        .await;
}
