//! End-to-end relay test for the WebTransport inbound path.
//!
//! Spins up a relay (which now binds both a TCP/WS listener and a
//! UDP/WT listener on the configured port), reads its identity
//! descriptor, and dials the relay over WebTransport from a native
//! client. The handshake completing proves: cert generation works,
//! the cert hash advertised in the descriptor matches the cert the
//! server uses, and the engine accepts WT inbound the same way it
//! accepts WS inbound.

use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use zeroize::Zeroizing;

use sunset_core::{Ed25519Verifier, Identity};
use sunset_noise::{NoiseIdentity, NoiseTransport, ed25519_seed_to_x25519_secret};
use sunset_relay::{Config, Relay};
use sunset_store_memory::MemoryStore;
use sunset_sync::{PeerAddr, PeerId, Signer, SyncConfig, SyncEngine};
use sunset_sync_webtransport_native::WebTransportRawTransport;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

struct IdentityAdapter(Identity);

impl NoiseIdentity for IdentityAdapter {
    fn ed25519_public(&self) -> [u8; 32] {
        self.0.public().as_bytes()
    }
    fn ed25519_secret_seed(&self) -> Zeroizing<[u8; 32]> {
        Zeroizing::new(self.0.secret_bytes())
    }
}

fn relay_config(data_dir: &std::path::Path, listen_addr: &str) -> Config {
    let toml = format!(
        r#"
        listen_addr = "{}"
        data_dir = "{}"
        interest_filter = "all"
        identity_secret = "auto"
        peers = []
        "#,
        listen_addr,
        data_dir.display(),
    );
    Config::from_toml(&toml).unwrap()
}

/// Fetch the relay's `GET /` JSON identity descriptor over plain HTTP.
async fn fetch_identity_json(host_port: &str) -> String {
    let mut sock = TcpStream::connect(host_port).await.expect("connect HTTP");
    sock.write_all(
        format!("GET / HTTP/1.1\r\nHost: {host_port}\r\nConnection: close\r\n\r\n").as_bytes(),
    )
    .await
    .expect("write HTTP request");
    let mut buf = Vec::new();
    tokio::time::timeout(Duration::from_secs(2), sock.read_to_end(&mut buf))
        .await
        .expect("read timeout")
        .expect("read body");
    let text = String::from_utf8(buf).expect("utf8");
    text.split_once("\r\n\r\n")
        .map(|(_, b)| b.to_string())
        .unwrap_or(text)
}

/// Pull a string-valued JSON field out of a flat object body. The
/// relay's identity body is hand-rolled JSON without escaping; this
/// helper matches its shape exactly without pulling in serde_json.
fn extract_json_str_field(body: &str, field: &str) -> Option<String> {
    let needle = format!("\"{field}\":\"");
    let start = body.find(&needle)? + needle.len();
    let rest = &body[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

#[tokio::test(flavor = "current_thread")]
async fn relay_advertises_cert_sha256_and_accepts_wt_dial() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let dir = tempfile::tempdir().unwrap();
            let config = relay_config(dir.path(), "127.0.0.1:0");
            let mut relay = Relay::start(config).await.expect("relay start");
            let dial = relay.dial_address();
            let x25519_hex = hex::encode(relay.x25519_public);
            let _engine_task = relay.run_for_test().await.expect("relay run");

            // 1. Read identity descriptor; it must include
            //    `webtransport_cert_sha256` (the SPKI hash, not a URL),
            //    and crucially it must NOT include any `webtransport_address`
            //    field — that field caused the prod-bug regression where the
            //    descriptor leaked the relay's `0.0.0.0` bind address.
            let host_port = dial
                .strip_prefix("ws://")
                .unwrap_or(&dial)
                .split('#')
                .next()
                .unwrap()
                .to_owned();
            let body = fetch_identity_json(&host_port).await;
            let cert_hex = extract_json_str_field(&body, "webtransport_cert_sha256")
                .expect("identity descriptor lacks webtransport_cert_sha256 — UDP bind should have succeeded on 127.0.0.1");
            assert_eq!(
                cert_hex.len(),
                64,
                "expected 64 hex chars (SHA-256), got {} ({cert_hex:?})",
                cert_hex.len()
            );
            assert!(
                !body.contains("webtransport_address"),
                "descriptor must not ship the WT URL form (prod-bug): {body}"
            );

            // 2. Build the WT URL the way the resolver does — from the
            //    *user-typed authority* + the descriptor's cert hash —
            //    and dial it. The Noise IK responder runs server-side;
            //    if the dial returns Ok, the WT path round-tripped
            //    through the relay's accept loop and SpawningAcceptor.
            let wt_url = format!("wt://{host_port}#x25519={x25519_hex}&cert-sha256={cert_hex}");

            let client_seed = [42u8; 32];
            let client_identity = Identity::from_secret_bytes(&client_seed);
            let store = Arc::new(MemoryStore::new(Arc::new(Ed25519Verifier)));
            let raw = WebTransportRawTransport::dial_only();
            let noise = NoiseTransport::new(
                raw,
                Arc::new(IdentityAdapter(client_identity.clone())),
            );
            let local_peer = PeerId(client_identity.store_verifying_key());
            let signer: Arc<dyn Signer> = Arc::new(client_identity.clone());
            let engine = Rc::new(SyncEngine::new(
                store.clone(),
                noise,
                SyncConfig::default(),
                local_peer,
                signer,
            ));
            let engine_clone = engine.clone();
            tokio::task::spawn_local(async move { engine_clone.run().await });

            let addr = PeerAddr::new(Bytes::from(wt_url.clone()));
            engine
                .add_peer(addr)
                .await
                .expect("WT dial + Noise IK handshake to relay");

            let _ = ed25519_seed_to_x25519_secret(&client_seed);
        })
        .await;
}
