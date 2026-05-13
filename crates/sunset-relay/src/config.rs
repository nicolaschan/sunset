//! TOML config parsing + defaults.
//!
//! See the spec at `docs/superpowers/specs/2026-04-27-sunset-relay-design.md`
//! § "Configuration".

use std::net::SocketAddr;
use std::path::PathBuf;

use serde::Deserialize;

use crate::error::{Error, Result};

/// Fully-resolved relay config (defaults applied; ready to use).
#[derive(Clone, Debug)]
pub struct Config {
    pub listen_addr: SocketAddr,
    pub data_dir: PathBuf,
    pub interest_filter: InterestFilter,
    pub identity_secret_path: PathBuf,
    pub peers: Vec<String>,
    pub accept_handshake_timeout_secs: u64,
    /// Subject Alternative Names baked into the relay's self-signed
    /// WebTransport cert. The browser dials
    /// `https://<webtransport_san[i]>:<port>/` and Chrome's
    /// WebTransport implementation (despite the W3C spec saying
    /// `serverCertificateHashes` *replaces* chain validation) still
    /// requires the dialed hostname to appear in the cert's SAN.
    /// Defaults are `["127.0.0.1", "localhost"]` — fine for tests and
    /// loopback dev. Public deployments MUST add their public
    /// hostname (e.g. `["relay.example.com", "127.0.0.1", "localhost"]`),
    /// and after a SAN list change the persisted PEM files at
    /// `<data_dir>/wt-cert.pem` + `wt-key.pem` must be deleted to force
    /// a fresh cert on next startup.
    pub webtransport_san: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InterestFilter {
    /// Subscribe to everything (NamePrefix("")).
    All,
}

/// Raw on-disk shape — every field is optional so partial configs are accepted.
#[derive(Debug, Default, Deserialize)]
struct RawConfig {
    listen_addr: Option<String>,
    data_dir: Option<String>,
    interest_filter: Option<String>,
    identity_secret: Option<String>,
    #[serde(default)]
    peers: Vec<String>,
    accept_handshake_timeout_secs: Option<u64>,
    webtransport_san: Option<Vec<String>>,
}

impl Config {
    /// Resolve from a TOML string (used by both file-loaded and embedded configs).
    pub fn from_toml(text: &str) -> Result<Self> {
        let raw: RawConfig = toml::from_str(text).map_err(|e| Error::Toml(e.to_string()))?;
        Self::from_raw(raw)
    }

    /// Resolve when no config file is present: pure defaults.
    pub fn defaults() -> Result<Self> {
        Self::from_raw(RawConfig::default())
    }

    fn from_raw(raw: RawConfig) -> Result<Self> {
        let listen_addr: SocketAddr = raw
            .listen_addr
            .as_deref()
            .unwrap_or("0.0.0.0:8443")
            .parse()
            .map_err(|e| Error::Config(format!("listen_addr parse: {e}")))?;

        let data_dir = PathBuf::from(raw.data_dir.unwrap_or_else(|| "./data".to_owned()));

        let interest_filter = match raw.interest_filter.as_deref().unwrap_or("all") {
            "all" => InterestFilter::All,
            other => {
                return Err(Error::Config(format!(
                    "interest_filter: unknown value `{other}` (only `all` supported in v0)"
                )));
            }
        };

        let identity_secret_path = match raw.identity_secret.as_deref() {
            None | Some("auto") => data_dir.join("identity.key"),
            Some(path) => PathBuf::from(path),
        };

        let accept_handshake_timeout_secs = raw.accept_handshake_timeout_secs.unwrap_or(15);

        let webtransport_san = raw
            .webtransport_san
            .unwrap_or_else(|| vec!["127.0.0.1".to_string(), "localhost".to_string()]);

        Ok(Config {
            listen_addr,
            data_dir,
            interest_filter,
            identity_secret_path,
            peers: raw.peers,
            accept_handshake_timeout_secs,
            webtransport_san,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_resolve() {
        let c = Config::defaults().unwrap();
        assert_eq!(c.listen_addr.to_string(), "0.0.0.0:8443");
        assert_eq!(c.data_dir, PathBuf::from("./data"));
        assert_eq!(c.interest_filter, InterestFilter::All);
        assert_eq!(c.identity_secret_path, PathBuf::from("./data/identity.key"));
        assert!(c.peers.is_empty());
    }

    #[test]
    fn full_toml_parses() {
        let toml = r#"
            listen_addr = "127.0.0.1:9000"
            data_dir = "/var/lib/sunset-relay"
            interest_filter = "all"
            identity_secret = "/etc/sunset/relay.key"
            peers = ["ws://other:8443#x25519=ab"]
        "#;
        let c = Config::from_toml(toml).unwrap();
        assert_eq!(c.listen_addr.to_string(), "127.0.0.1:9000");
        assert_eq!(c.data_dir, PathBuf::from("/var/lib/sunset-relay"));
        assert_eq!(
            c.identity_secret_path,
            PathBuf::from("/etc/sunset/relay.key")
        );
        assert_eq!(c.peers.len(), 1);
    }

    #[test]
    fn auto_identity_resolves_under_data_dir() {
        let toml = r#"
            listen_addr = "0.0.0.0:8443"
            data_dir = "/tmp/relay"
            identity_secret = "auto"
        "#;
        let c = Config::from_toml(toml).unwrap();
        assert_eq!(
            c.identity_secret_path,
            PathBuf::from("/tmp/relay/identity.key")
        );
    }

    #[test]
    fn rejects_unknown_interest_filter() {
        let toml = r#"
            listen_addr = "0.0.0.0:8443"
            interest_filter = "room/general"
        "#;
        let err = Config::from_toml(toml).unwrap_err();
        assert!(matches!(err, Error::Config(_)));
    }

    #[test]
    fn accept_handshake_timeout_defaults_to_15s() {
        let c = Config::defaults().unwrap();
        assert_eq!(c.accept_handshake_timeout_secs, 15);
    }

    #[test]
    fn accept_handshake_timeout_parses_from_toml() {
        let toml = r#"
            listen_addr = "0.0.0.0:8443"
            accept_handshake_timeout_secs = 1
        "#;
        let c = Config::from_toml(toml).unwrap();
        assert_eq!(c.accept_handshake_timeout_secs, 1);
    }

    #[test]
    fn webtransport_san_defaults_include_loopback() {
        let c = Config::defaults().unwrap();
        // Out of the box, the WT cert is valid for loopback dialing
        // — sufficient for tests and same-host dev. Operators behind
        // a public hostname must set `webtransport_san` explicitly.
        assert!(c.webtransport_san.iter().any(|s| s == "127.0.0.1"));
        assert!(c.webtransport_san.iter().any(|s| s == "localhost"));
    }

    #[test]
    fn webtransport_san_overridable_from_toml() {
        // Production-shaped config: the relay binds 0.0.0.0 and is
        // reached at a public hostname. Without setting this, the
        // self-signed WT cert wouldn't carry the public hostname in
        // its SAN, and Chrome's WebTransport (with hash pinning)
        // still requires SAN match — so dials to
        // `https://relay.example.com:8443/` would fail.
        let toml = r#"
            listen_addr = "0.0.0.0:8443"
            webtransport_san = ["relay.example.com", "127.0.0.1", "localhost"]
        "#;
        let c = Config::from_toml(toml).unwrap();
        assert_eq!(
            c.webtransport_san,
            vec![
                "relay.example.com".to_string(),
                "127.0.0.1".to_string(),
                "localhost".to_string()
            ]
        );
    }
}
