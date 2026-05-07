//! 2 CLI clients connect to one in-process relay over WS, exchange
//! a chat message both directions.

mod helpers;

use std::time::Duration;

use helpers::{eventually, fresh_client, spawn_relay};

#[tokio::test(flavor = "current_thread")]
async fn ws_roundtrip_two_clients() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let relay = spawn_relay().await;
            let url = relay.dial_url.clone();

            let alice = fresh_client();
            let bob = fresh_client();
            alice.set_self_name("alice");
            bob.set_self_name("bob");

            alice.add_relay(url.clone()).await.expect("alice add_relay");
            bob.add_relay(url.clone()).await.expect("bob add_relay");

            alice.join_room("alpha").await.expect("alice join");
            bob.join_room("alpha").await.expect("bob join");

            alice
                .send_text("hello bob".to_owned())
                .await
                .expect("alice send");

            let saw = eventually(Duration::from_secs(10), || {
                let v = bob.snapshot_room("alpha")?;
                if v.messages.iter().any(|m| m.body == "hello bob") {
                    Some(())
                } else {
                    None
                }
            })
            .await;
            assert!(saw.is_some(), "bob never saw alice's message");

            bob.send_text("hi alice".to_owned())
                .await
                .expect("bob send");
            let saw = eventually(Duration::from_secs(10), || {
                let v = alice.snapshot_room("alpha")?;
                if v.messages.iter().any(|m| m.body == "hi alice") {
                    Some(())
                } else {
                    None
                }
            })
            .await;
            assert!(saw.is_some(), "alice never saw bob's message");
        })
        .await;
}
