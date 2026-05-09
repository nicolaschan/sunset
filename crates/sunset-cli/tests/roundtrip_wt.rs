//! Same shape as roundtrip_ws but the clients dial via wt://. The
//! relay has both a TCP/WS listener and a UDP/WT listener; the wt
//! URL routes through FallbackTransport's primary half. If the
//! relay's WT cert init fails (no UDP, container restrictions,
//! etc.), the test self-skips by returning early — matching the
//! relay's "WS-only fallback" behavior.

mod helpers;

use std::time::Duration;

use helpers::{eventually, fresh_client, spawn_relay};

#[tokio::test(flavor = "current_thread")]
async fn wt_roundtrip_two_clients() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let relay = spawn_relay().await;
            let host_port = relay
                .dial_url
                .strip_prefix("ws://")
                .expect("ws:// prefix on dial_url")
                .split('#')
                .next()
                .unwrap();
            let descriptor_url = format!("http://{host_port}/");
            let body = match reqwest::get(&descriptor_url).await {
                Ok(r) => r.text().await.unwrap_or_default(),
                Err(_) => return,
            };
            let cert_hex = match extract_field(&body, "webtransport_cert_sha256") {
                Some(s) => s,
                None => return,
            };
            let x25519_hex = relay
                .dial_url
                .split("x25519=")
                .nth(1)
                .expect("x25519 fragment")
                .to_owned();
            let wt_url = format!("wt://{host_port}#x25519={x25519_hex}&cert-sha256={cert_hex}");

            let alice = fresh_client();
            let bob = fresh_client();
            alice.set_self_name("alice");
            bob.set_self_name("bob");

            alice.add_relay(wt_url.clone()).await.unwrap();
            bob.add_relay(wt_url.clone()).await.unwrap();

            alice.join_room("alpha").await.unwrap();
            bob.join_room("alpha").await.unwrap();

            alice.send_text("hello over wt".to_owned()).await.unwrap();
            let saw = eventually(Duration::from_secs(10), || {
                let v = bob.snapshot_room("alpha")?;
                if v.messages.iter().any(|m| m.body == "hello over wt") {
                    Some(())
                } else {
                    None
                }
            })
            .await;
            assert!(saw.is_some(), "wt path: bob never saw alice's message");
        })
        .await;
}

/// Cheap JSON field extractor (descriptor body shape is stable).
fn extract_field(body: &str, key: &str) -> Option<String> {
    let pat = format!("\"{key}\":\"");
    let i = body.find(&pat)?;
    let rest = &body[i + pat.len()..];
    let end = rest.find('"')?;
    Some(rest[..end].to_owned())
}
