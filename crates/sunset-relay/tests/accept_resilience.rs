//! Resilience of the relay's WS accept loop.
//!
//! Background: the relay's listener spawns a router task that peeks
//! incoming TCP, classifies, and forwards WS-classified streams onto a
//! bounded `mpsc::Sender<TcpStream>`. The engine's run loop drains the
//! receiver via `WebSocketRawTransport::accept`, which runs
//! `tokio_tungstenite::accept_async` to complete the upgrade.
//!
//! Failure mode (regression target): a peek-passing but
//! handshake-invalid request (e.g. `Upgrade: websocket` without a
//! `Sec-WebSocket-Key`) used to make `accept_async` return Err, which
//! `SyncEngine::run` then propagated upward, exiting the engine
//! entirely. After that the receiver was orphaned, the dispatch
//! channel filled, and every later WS upgrade hung forever — yet
//! plain GET kept working because it's served inline by the router.
//! That matches what a single misbehaving public-internet probe can
//! do to a freshly-restarted relay.

use std::time::Duration;

use rand_core::OsRng;
use std::rc::Rc;
use std::sync::Arc;

use bytes::Bytes;
use sunset_core::{Ed25519Verifier, Identity};
use sunset_noise::{NoiseIdentity, NoiseTransport};
use sunset_relay::{Config, Relay};
use sunset_store_memory::MemoryStore;
use sunset_sync::{PeerAddr, PeerId, Signer, SyncConfig, SyncEngine};
use sunset_sync_ws_native::WebSocketRawTransport;
use tokio::io::AsyncWriteExt;
use zeroize::Zeroizing;

struct IdentityAdapter(Identity);

impl NoiseIdentity for IdentityAdapter {
    fn ed25519_public(&self) -> [u8; 32] {
        self.0.public().as_bytes()
    }
    fn ed25519_secret_seed(&self) -> Zeroizing<[u8; 32]> {
        Zeroizing::new(self.0.secret_bytes())
    }
}

fn relay_config(data_dir: &std::path::Path) -> Config {
    Config::from_toml(&format!(
        r#"
        listen_addr = "127.0.0.1:0"
        data_dir = "{}"
        interest_filter = "all"
        identity_secret = "auto"
        peers = []
        "#,
        data_dir.display()
    ))
    .unwrap()
}

fn extract_host_port(dial_addr: &str) -> String {
    // ws://127.0.0.1:PORT#x25519=hex → 127.0.0.1:PORT
    dial_addr
        .strip_prefix("ws://")
        .unwrap()
        .split(['#', '/'])
        .next()
        .unwrap()
        .to_string()
}

#[tokio::test(flavor = "current_thread")]
async fn relay_accept_loop_survives_a_ws_client_that_skips_noise() {
    // Failure mode: a client completes the WS upgrade (gets 101 from
    // the relay) but then goes silent — never sends the Noise IK
    // initiator message. The engine's `transport.accept()` chains
    // raw WS accept → Noise responder, and the responder
    // (`do_handshake_responder`) blocks indefinitely on
    // `recv_reliable` waiting for the first Noise message. While
    // it's blocked, *no other connection on this engine can be
    // accepted*. After enough such probes accumulate, every later
    // dial hangs — exactly what the production relay exhibits some
    // time after a fresh restart.
    //
    // The fix: bound `transport.accept()` with a per-handshake
    // timeout in the engine's run loop (and log+continue on timeout
    // just like any other accept Err — fix #1).
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let dir = tempfile::tempdir().unwrap();
            let mut relay = Relay::start(relay_config(dir.path()))
                .await
                .expect("relay new");
            let dial_addr = relay.dial_address();
            let host_port = extract_host_port(&dial_addr);
            let _engine_task = relay.run_for_test().await.expect("relay run");

            // Rude WS client: complete the upgrade, then sit silent.
            // tokio_tungstenite::connect_async does the WS-level
            // handshake; the resulting stream is held open without
            // any Noise traffic.
            let bad_url = format!("ws://{host_port}/");
            let (_bad_ws, _resp) = tokio_tungstenite::connect_async(&bad_url)
                .await
                .expect("bad-client WS upgrade should still succeed");

            tokio::time::sleep(Duration::from_millis(200)).await;

            // Healthy client should still be able to dial within a
            // generous bound. With the bug, the relay's engine is
            // stuck inside the Noise responder for the rude client
            // and this hits the outer timeout.
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

            // Outer bound = production-default handshake timeout
            // (15 s) plus a few seconds of slack for the second
            // accept after the rude probe times out.
            let dial_result = tokio::time::timeout(
                Duration::from_secs(20),
                engine.add_peer(PeerAddr::new(Bytes::from(dial_addr))),
            )
            .await;

            match dial_result {
                Err(_) => panic!(
                    "healthy dial timed out — the relay's accept loop is wedged inside the \
                     Noise responder for a rude WS client. A per-handshake timeout is missing."
                ),
                Ok(Err(e)) => panic!("healthy dial returned err: {e:?}"),
                Ok(Ok(_)) => {}
            }
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn relay_accept_loop_survives_a_failed_ws_handshake() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            // Boot a relay on a random localhost port.
            let dir = tempfile::tempdir().unwrap();
            let mut relay = Relay::start(relay_config(dir.path()))
                .await
                .expect("relay new");
            let dial_addr = relay.dial_address();
            let host_port = extract_host_port(&dial_addr);
            let _engine_task = relay.run_for_test().await.expect("relay run");

            // Misbehaving probe: writes a request that satisfies the
            // router's peek (has `Upgrade: websocket` + `\r\n\r\n`)
            // but is rejected by `accept_async` for missing
            // `Sec-WebSocket-Key` / `Sec-WebSocket-Version`. This is
            // the path that used to take down the engine's run loop.
            {
                let mut bad = tokio::net::TcpStream::connect(&host_port)
                    .await
                    .expect("bad-client tcp connect");
                bad.write_all(
                    b"GET / HTTP/1.1\r\n\
                      Host: x\r\n\
                      Upgrade: websocket\r\n\
                      Connection: Upgrade\r\n\
                      \r\n",
                )
                .await
                .expect("bad-client write");
                // Drop closes the connection; relay's accept_async
                // returns Err immediately on the malformed headers.
            }

            // Brief settle: let the relay's accept loop process the
            // failed handshake before we test the next client.
            tokio::time::sleep(Duration::from_millis(200)).await;

            // Healthy client: a normal Noise+Hello dial should still
            // succeed promptly. With the bug present, this hangs
            // because the engine's run loop has exited and the WS
            // dispatch channel is no longer being drained.
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
                Duration::from_secs(5),
                engine.add_peer(PeerAddr::new(Bytes::from(dial_addr))),
            )
            .await;

            match dial_result {
                Err(_) => panic!(
                    "healthy client dial timed out after 5s — relay's WS accept loop is wedged \
                     by the prior failed handshake"
                ),
                Ok(Err(e)) => panic!("healthy client dial returned err: {e:?}"),
                Ok(Ok(_)) => {}
            }
        })
        .await;
}
