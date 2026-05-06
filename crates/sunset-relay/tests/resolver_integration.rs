//! End-to-end: resolve a bare `host:port` against a real running
//! relay over real HTTP, and confirm the canonical PeerAddr matches
//! what the relay reports as its `dial_address()`.

use std::time::Duration;

use sunset_relay::{Config, Relay};
use sunset_relay_resolver::Resolver;

mod adapter {
    // Re-implement the adapter inline so the test doesn't need to
    // pierce sunset-relay's private module boundary. This is a
    // 25-line copy by design — the production adapter and the test
    // path use the same trait.
    use async_trait::async_trait;
    use sunset_relay_resolver::{Error, HttpFetch, Result};

    pub struct ReqwestFetch(reqwest::Client);
    impl ReqwestFetch {
        pub fn new() -> Self {
            Self(reqwest::Client::new())
        }
    }
    #[async_trait(?Send)]
    impl HttpFetch for ReqwestFetch {
        async fn get(&self, url: &str) -> Result<String> {
            let r = self
                .0
                .get(url)
                .send()
                .await
                .map_err(|e| Error::Http(format!("{e}")))?;
            if !r.status().is_success() {
                return Err(Error::Http(format!("status {}", r.status())));
            }
            r.text().await.map_err(|e| Error::Http(format!("{e}")))
        }
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

#[tokio::test(flavor = "current_thread")]
async fn resolves_loopback_host_port_to_canonical_peeraddr() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let dir = tempfile::tempdir().unwrap();
            let config = relay_config(dir.path(), "127.0.0.1:0");
            let mut relay = Relay::start(config).await.expect("relay new");
            // `dial_address()` is the legacy WS form (`ws://…#x25519=…`).
            // After WebTransport became the preferred path, the resolver
            // returns the WT URL when the relay advertises one — so we
            // can't compare to dial_address() directly. We do compare
            // the host:port and the x25519 fragment.
            let ws_canonical = relay.dial_address();
            let _engine = relay.run_for_test().await.expect("relay run");

            // Pull the bound host:port out of the canonical form so we
            // can feed it back through the resolver.
            let host_port = ws_canonical
                .strip_prefix("ws://")
                .unwrap()
                .split('#')
                .next()
                .unwrap()
                .to_string();

            // Give the listener a moment to be ready.
            tokio::time::sleep(Duration::from_millis(50)).await;

            let resolver = Resolver::new(adapter::ReqwestFetch::new());
            let resolved = resolver
                .resolve_with_fallback(&host_port)
                .await
                .expect("resolve");

            // Fallback URL must always be the legacy WS form, byte-for-byte.
            assert_eq!(resolved.fallback, ws_canonical);
            // Primary should be the WT URL when the relay successfully
            // bound UDP — it does on 127.0.0.1 in tests.
            assert!(
                resolved.primary.starts_with("wt://"),
                "expected WT URL as primary, got: {}",
                resolved.primary,
            );
            assert!(
                resolved.primary.contains("cert-sha256="),
                "primary lacks cert-sha256 fragment: {}",
                resolved.primary,
            );
        })
        .await;
}
