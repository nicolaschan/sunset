//! `PeerAddr` parser for WebTransport URLs.
//!
//! Accepted forms:
//! - `wt://host:port` — insecure (test/loopback only). Translated to
//!   `https://host:port` for the wtransport client; cert pinning
//!   requirements still apply.
//! - `wts://host[:port]` — secure (production). Same translation.
//!
//! Optional fragment carries pinned cert hashes:
//!   `…#x25519=<hex>&cert-sha256=<hex>[&cert-sha256=<hex>…]`
//!
//! Extra fragment keys (e.g. `x25519=…` consumed by the Noise wrapper
//! above us) are preserved untouched in the parsed output but ignored
//! for connection establishment — the WT transport only reads
//! `cert-sha256=`.

use sunset_sync::{Error as SyncError, PeerAddr, Result as SyncResult};
use wtransport::tls::Sha256Digest;

use crate::cert::parse_cert_hash_hex;

/// Parsed WebTransport address.
#[derive(Clone, Debug)]
pub struct ParsedWebTransportAddr {
    /// Scheme without trailing `://`. `"wt"` or `"wts"`.
    pub scheme: String,
    /// `host:port` (or just `host` when port absent — wtransport's
    /// `connect()` then uses 443).
    pub authority: String,
    /// Pinned cert hashes from `#cert-sha256=…` fragment(s). Empty
    /// when relying on system CA roots.
    pub cert_hashes: Vec<Sha256Digest>,
}

impl ParsedWebTransportAddr {
    /// HTTPS URL the wtransport client expects, e.g. `https://host:port`.
    pub fn https_url(&self) -> String {
        format!("https://{}", self.authority)
    }
}

/// Parse a `PeerAddr` whose URL begins with `wt://` or `wts://`. Returns
/// [`SyncError::Transport`] on any error.
pub fn parse_addr(addr: &PeerAddr) -> SyncResult<ParsedWebTransportAddr> {
    let s = std::str::from_utf8(addr.as_bytes())
        .map_err(|e| SyncError::Transport(format!("wt addr not utf-8: {e}")))?;
    let (head, fragment) = match s.split_once('#') {
        Some((h, f)) => (h, Some(f)),
        None => (s, None),
    };
    let (scheme, authority) = if let Some(rest) = head.strip_prefix("wt://") {
        ("wt", rest)
    } else if let Some(rest) = head.strip_prefix("wts://") {
        ("wts", rest)
    } else {
        return Err(SyncError::Transport(format!(
            "wt addr: unsupported scheme in {head} (expected wt:// or wts://)"
        )));
    };
    if authority.is_empty() {
        return Err(SyncError::Transport(format!(
            "wt addr: empty authority in {s}"
        )));
    }

    let mut cert_hashes = Vec::new();
    if let Some(fragment) = fragment {
        for part in fragment.split('&') {
            if let Some(hex) = part.strip_prefix("cert-sha256=") {
                cert_hashes.push(
                    parse_cert_hash_hex(hex).map_err(|e| {
                        SyncError::Transport(format!("wt addr: bad cert-sha256: {e}"))
                    })?,
                );
            }
        }
    }

    Ok(ParsedWebTransportAddr {
        scheme: scheme.into(),
        authority: authority.into(),
        cert_hashes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;

    #[test]
    fn parses_wt_url() {
        let addr = PeerAddr::new(Bytes::from("wt://127.0.0.1:8443"));
        let p = parse_addr(&addr).unwrap();
        assert_eq!(p.scheme, "wt");
        assert_eq!(p.authority, "127.0.0.1:8443");
        assert!(p.cert_hashes.is_empty());
        assert_eq!(p.https_url(), "https://127.0.0.1:8443");
    }

    #[test]
    fn parses_wts_with_cert_hash() {
        let hex = "ab".repeat(32);
        let addr = PeerAddr::new(Bytes::from(format!(
            "wts://relay.example.com#cert-sha256={hex}"
        )));
        let p = parse_addr(&addr).unwrap();
        assert_eq!(p.scheme, "wts");
        assert_eq!(p.authority, "relay.example.com");
        assert_eq!(p.cert_hashes.len(), 1);
    }

    #[test]
    fn ignores_unrelated_fragment_keys() {
        let cert_hex = "cd".repeat(32);
        let x25519_hex = "11".repeat(32);
        let addr = PeerAddr::new(Bytes::from(format!(
            "wt://127.0.0.1:8443#x25519={x25519_hex}&cert-sha256={cert_hex}"
        )));
        let p = parse_addr(&addr).unwrap();
        assert_eq!(p.cert_hashes.len(), 1);
    }

    #[test]
    fn rejects_unknown_scheme() {
        let addr = PeerAddr::new(Bytes::from("ws://relay.example.com"));
        let err = parse_addr(&addr).unwrap_err();
        assert!(format!("{err}").contains("unsupported scheme"));
    }

    #[test]
    fn rejects_bad_cert_hex() {
        let addr = PeerAddr::new(Bytes::from("wt://127.0.0.1:8443#cert-sha256=zz"));
        let err = parse_addr(&addr).unwrap_err();
        assert!(format!("{err}").contains("cert-sha256"));
    }
}
