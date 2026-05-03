//! HTTP-side integration tests for the relay's single-port router:
//!
//!   * `GET /` (no upgrade) → JSON identity descriptor.
//!   * `GET /` with `Upgrade: websocket` → still routes to the WS
//!     transport (the WS test isn't here — `multi_relay.rs` already
//!     exercises real WS clients end-to-end. This file just smoke-
//!     tests the new HTTP handler.)

use std::time::Duration;

use sunset_relay::{Config, Relay};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

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

/// Strip the `ws://` prefix and any URL fragment to get a `host:port`
/// suitable for `TcpStream::connect`. The relay's `dial_address()`
/// returns `ws://<bound>#x25519=<hex>`.
fn host_port_from_dial(dial: &str) -> String {
    let no_scheme = dial.strip_prefix("ws://").unwrap_or(dial);
    no_scheme.split('#').next().unwrap_or(no_scheme).to_string()
}

#[tokio::test(flavor = "current_thread")]
async fn get_root_returns_identity_json() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let dir = tempfile::tempdir().unwrap();
            let config = relay_config(dir.path(), "127.0.0.1:0");
            let mut relay = Relay::start(config).await.expect("relay new");
            let dial = relay.dial_address();
            let ed_hex = hex::encode(relay.ed25519_public);
            let x_hex = hex::encode(relay.x25519_public);
            let _engine_task = relay.run_for_test().await.expect("relay run");

            let target = host_port_from_dial(&dial);
            let mut sock = TcpStream::connect(&target).await.expect("connect");
            sock.write_all(
                format!("GET / HTTP/1.1\r\nHost: {target}\r\nConnection: close\r\n\r\n").as_bytes(),
            )
            .await
            .expect("write request");

            let mut buf = Vec::new();
            tokio::time::timeout(Duration::from_secs(2), sock.read_to_end(&mut buf))
                .await
                .expect("read timeout")
                .expect("read");
            let response = String::from_utf8(buf).expect("utf8 response");

            let response_lower = response.to_ascii_lowercase();
            assert!(
                response.starts_with("HTTP/1.1 200"),
                "expected 200, got: {response}"
            );
            assert!(
                response_lower.contains("content-type: application/json"),
                "expected json content-type: {response}"
            );
            // Browsers fetching this from a different origin (the
            // sunset-web client served from elsewhere) must be able to
            // read the body. The response is public identity info; no
            // credentials are honored.
            assert!(
                response_lower.contains("access-control-allow-origin: *"),
                "expected CORS header on identity response: {response}"
            );
            // Body is after the blank line.
            let body = response
                .split_once("\r\n\r\n")
                .map(|(_, b)| b)
                .unwrap_or(&response);
            assert!(
                body.contains(&format!("\"ed25519\":\"{ed_hex}\"")),
                "ed25519 field missing/wrong: {body}"
            );
            assert!(
                body.contains(&format!("\"x25519\":\"{x_hex}\"")),
                "x25519 field missing/wrong: {body}"
            );
            assert!(
                body.contains("\"address\":\"ws://"),
                "address field missing: {body}"
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn get_unknown_path_is_404() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let dir = tempfile::tempdir().unwrap();
            let config = relay_config(dir.path(), "127.0.0.1:0");
            let mut relay = Relay::start(config).await.expect("relay new");
            let dial = relay.dial_address();
            let _engine_task = relay.run_for_test().await.expect("relay run");

            let target = host_port_from_dial(&dial);
            let mut sock = TcpStream::connect(&target).await.expect("connect");
            sock.write_all(
                format!("GET /nope HTTP/1.1\r\nHost: {target}\r\nConnection: close\r\n\r\n")
                    .as_bytes(),
            )
            .await
            .expect("write request");

            let mut buf = Vec::new();
            tokio::time::timeout(Duration::from_secs(2), sock.read_to_end(&mut buf))
                .await
                .expect("read timeout")
                .expect("read");
            let response = String::from_utf8(buf).expect("utf8 response");

            assert!(
                response.starts_with("HTTP/1.1 404"),
                "expected 404, got: {response}"
            );
        })
        .await;
}
