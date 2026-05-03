//! Regression test for the inbound-pipeline concurrency property.
//!
//! Spec: docs/superpowers/specs/2026-05-02-relay-axum-and-concurrent-handshakes-design.md
//!
//! With a small per-acceptor handshake timeout, launch N rude WS clients
//! that complete the upgrade and then stall (never send the Noise IK
//! initiator message). Then launch one healthy client. Assert the healthy
//! client completes its full Noise+Hello within ~3 s — i.e., not roughly
//! N × handshake_timeout.

use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use rand_core::OsRng;
use zeroize::Zeroizing;

use sunset_core::{Ed25519Verifier, Identity};
use sunset_noise::{NoiseIdentity, NoiseTransport};
use sunset_relay::{Config, Relay};
use sunset_store_memory::MemoryStore;
use sunset_sync::{PeerAddr, PeerId, Signer, SyncConfig, SyncEngine};
use sunset_sync_ws_native::WebSocketRawTransport;

struct IdentityAdapter(Identity);

impl NoiseIdentity for IdentityAdapter {
    fn ed25519_public(&self) -> [u8; 32] {
        self.0.public().as_bytes()
    }
    fn ed25519_secret_seed(&self) -> Zeroizing<[u8; 32]> {
        Zeroizing::new(self.0.secret_bytes())
    }
}

fn relay_config(data_dir: &std::path::Path, handshake_timeout_secs: u64) -> Config {
    Config::from_toml(&format!(
        r#"
        listen_addr = "127.0.0.1:0"
        data_dir = "{}"
        interest_filter = "all"
        identity_secret = "auto"
        peers = []
        accept_handshake_timeout_secs = {handshake_timeout_secs}
        "#,
        data_dir.display(),
    ))
    .unwrap()
}

fn extract_host_port(dial_addr: &str) -> String {
    dial_addr
        .strip_prefix("ws://")
        .unwrap()
        .split(['#', '/'])
        .next()
        .unwrap()
        .to_string()
}

#[tokio::test(flavor = "current_thread")]
async fn rude_clients_do_not_serialize_a_healthy_dial() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            // Short timeout so the test runs fast even when rude clients
            // eventually time out.
            let dir = tempfile::tempdir().unwrap();
            let mut relay = Relay::start(relay_config(dir.path(), 1))
                .await
                .expect("relay new");
            let dial_addr = relay.dial_address();
            let host_port = extract_host_port(&dial_addr);
            let _engine_task = relay.run_for_test().await.expect("relay run");

            // Launch N=8 rude WS clients in parallel — each completes the
            // WS upgrade then sits silent. The relay spawns one promote
            // task per upgrade; with concurrent_acceptor, none of them
            // block the others.
            let mut rude_handles = Vec::new();
            for _ in 0..8 {
                let url = format!("ws://{host_port}/");
                rude_handles.push(tokio::task::spawn_local(async move {
                    let (_ws, _resp) = tokio_tungstenite::connect_async(&url)
                        .await
                        .expect("rude WS upgrade");
                    // Hold the connection open without sending Noise.
                    tokio::time::sleep(Duration::from_secs(10)).await;
                }));
            }

            // Tiny settle so the relay has accepted the rude upgrades.
            tokio::time::sleep(Duration::from_millis(200)).await;

            // Healthy client: a normal Noise+Hello dial. Under the new
            // wiring, this completes within ~RTT regardless of how many
            // rude clients are stalled.
            let alice = Identity::generate(&mut OsRng);
            let store = Arc::new(MemoryStore::new(Arc::new(Ed25519Verifier)));
            let raw = WebSocketRawTransport::dial_only();
            let noise = NoiseTransport::new(raw, Arc::new(IdentityAdapter(alice.clone())));
            let signer: Arc<dyn Signer> = Arc::new(alice.clone());
            let engine = Rc::new(SyncEngine::new(
                store,
                noise,
                SyncConfig::default(),
                PeerId(alice.store_verifying_key()),
                signer,
            ));
            let engine_clone = engine.clone();
            tokio::task::spawn_local(async move {
                let _ = engine_clone.run().await;
            });

            let dial_result = tokio::time::timeout(
                Duration::from_secs(3),
                engine.add_peer(PeerAddr::new(Bytes::from(dial_addr))),
            )
            .await;

            for h in rude_handles {
                h.abort();
            }

            match dial_result {
                Err(_) => panic!(
                    "healthy dial did not complete within 3 s — \
                     8 rude clients are serializing the inbound pipeline. \
                     SpawningAcceptor's spawn-per-conn property has regressed."
                ),
                Ok(Err(e)) => panic!("healthy dial returned err: {e:?}"),
                Ok(Ok(_)) => {}
            }
        })
        .await;
}
