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
            let mut relay = Relay::new(config).await.expect("relay new");
            let canonical_expected = relay.dial_address(); // ws://127.0.0.1:<port>#x25519=<hex>
            let _engine = relay.run_for_test().await.expect("relay run");

            // Pull the bound host:port out of the canonical form so we
            // can feed it back through the resolver.
            let host_port = canonical_expected
                .strip_prefix("ws://")
                .unwrap()
                .split('#')
                .next()
                .unwrap()
                .to_string();

            // Give the listener a moment to be ready.
            tokio::time::sleep(Duration::from_millis(50)).await;

            let resolver = Resolver::new(adapter::ReqwestFetch::new());
            let resolved = resolver.resolve(&host_port).await.expect("resolve");

            assert_eq!(resolved, canonical_expected);
        })
        .await;
}
