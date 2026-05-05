//! `FallbackTransport<P, F>` — try a primary transport first, fall back
//! to a secondary if the primary fails.
//!
//! Designed for the browser's relay path: dial the relay over WebTransport
//! (`wt://`/`wts://`) when the descriptor advertises one, and on connect
//! failure (cert mismatch, UDP blocked, browser doesn't support WT) fall
//! back to WebSocket (`ws://`/`wss://`) on the same host:port. The
//! semantic guarantee is "if the relay is reachable at all, we connect."
//!
//! The primary URL's scheme is rewritten by [`fallback_url_for`]:
//!   `wt://X` → `ws://X`,  `wts://X` → `wss://X`. The URL fragment
//! retains the `x25519=` key (Noise IK still needs the static pubkey)
//! and drops `cert-sha256=` (WS doesn't pin certs).
//!
//! `accept()` forwards to the primary only — the relay-side dual
//! listener already has its own combinator (`DualInboundTransport` in
//! `sunset-relay`); browsers / native CLIs never accept inbound on
//! either half.

use async_trait::async_trait;
use bytes::Bytes;

use crate::error::{Error, Result};
use crate::transport::{Transport, TransportConnection, TransportKind};
use crate::types::{PeerAddr, PeerId};

pub struct FallbackTransport<P: Transport, F: Transport> {
    primary: P,
    fallback: F,
}

impl<P: Transport, F: Transport> FallbackTransport<P, F> {
    pub fn new(primary: P, fallback: F) -> Self {
        Self { primary, fallback }
    }
}

#[async_trait(?Send)]
impl<P, F> Transport for FallbackTransport<P, F>
where
    P: Transport,
    P::Connection: 'static,
    F: Transport,
    F::Connection: 'static,
{
    type Connection = FallbackConnection<P::Connection, F::Connection>;

    async fn connect(&self, addr: PeerAddr) -> Result<Self::Connection> {
        let s = std::str::from_utf8(addr.as_bytes())
            .map_err(|e| Error::Transport(format!("fallback: addr not utf-8: {e}")))?;
        if s.starts_with("wt://") || s.starts_with("wts://") {
            // Primary scheme — try WT, then WS on failure.
            let primary_err = match self.primary.connect(addr.clone()).await {
                Ok(c) => return Ok(FallbackConnection::Primary(c)),
                Err(e) => e,
            };
            let fallback_addr = fallback_addr_for(&addr).map_err(|e| {
                Error::Transport(format!(
                    "fallback: primary failed ({primary_err}) and fallback addr derivation failed: {e}"
                ))
            })?;
            match self.fallback.connect(fallback_addr).await {
                Ok(c) => Ok(FallbackConnection::Fallback(c)),
                Err(fb_err) => Err(Error::Transport(format!(
                    "fallback: primary failed ({primary_err}); fallback also failed ({fb_err})"
                ))),
            }
        } else if s.starts_with("ws://") || s.starts_with("wss://") {
            // No primary URL at all — relay didn't advertise WT. Just
            // dial the fallback (WS) directly.
            self.fallback
                .connect(addr)
                .await
                .map(FallbackConnection::Fallback)
        } else {
            Err(Error::Transport(format!(
                "fallback: unknown scheme in {s} (expected wt:// wts:// ws:// or wss://)"
            )))
        }
    }

    async fn accept(&self) -> Result<Self::Connection> {
        // Browsers / native CLI peers don't accept on the relay path —
        // accept on the primary only and surface its result. (Dial-only
        // raw transports return a never-completing future from
        // `accept()` per the trait's docs, so this resolves only on
        // genuine errors.)
        self.primary.accept().await.map(FallbackConnection::Primary)
    }
}

/// Rewrite a `wt://` / `wts://` URL into the equivalent `ws://` / `wss://`
/// URL on the same host:port. The fragment is filtered to drop
/// `cert-sha256=` keys (WS doesn't need cert pinning) but retain
/// `x25519=` and any other unknown keys (forwards-compatibility).
pub fn fallback_addr_for(addr: &PeerAddr) -> std::result::Result<PeerAddr, String> {
    let s = std::str::from_utf8(addr.as_bytes()).map_err(|e| format!("not utf-8: {e}"))?;
    let (head, fragment) = match s.split_once('#') {
        Some((h, f)) => (h, Some(f)),
        None => (s, None),
    };
    let new_head = if let Some(rest) = head.strip_prefix("wt://") {
        format!("ws://{rest}")
    } else if let Some(rest) = head.strip_prefix("wts://") {
        format!("wss://{rest}")
    } else {
        return Err(format!("not a wt/wts URL: {head}"));
    };
    let new_url = match fragment {
        None => new_head,
        Some(f) => {
            let kept: Vec<&str> = f
                .split('&')
                .filter(|p| !p.starts_with("cert-sha256="))
                .filter(|p| !p.is_empty())
                .collect();
            if kept.is_empty() {
                new_head
            } else {
                format!("{new_head}#{}", kept.join("&"))
            }
        }
    };
    Ok(PeerAddr::new(Bytes::from(new_url)))
}

pub enum FallbackConnection<PC, FC> {
    Primary(PC),
    Fallback(FC),
}

#[async_trait(?Send)]
impl<PC, FC> TransportConnection for FallbackConnection<PC, FC>
where
    PC: TransportConnection,
    FC: TransportConnection,
{
    async fn send_reliable(&self, bytes: Bytes) -> Result<()> {
        match self {
            FallbackConnection::Primary(c) => c.send_reliable(bytes).await,
            FallbackConnection::Fallback(c) => c.send_reliable(bytes).await,
        }
    }
    async fn recv_reliable(&self) -> Result<Bytes> {
        match self {
            FallbackConnection::Primary(c) => c.recv_reliable().await,
            FallbackConnection::Fallback(c) => c.recv_reliable().await,
        }
    }
    async fn send_unreliable(&self, bytes: Bytes) -> Result<()> {
        match self {
            FallbackConnection::Primary(c) => c.send_unreliable(bytes).await,
            FallbackConnection::Fallback(c) => c.send_unreliable(bytes).await,
        }
    }
    async fn recv_unreliable(&self) -> Result<Bytes> {
        match self {
            FallbackConnection::Primary(c) => c.recv_unreliable().await,
            FallbackConnection::Fallback(c) => c.recv_unreliable().await,
        }
    }
    fn peer_id(&self) -> PeerId {
        match self {
            FallbackConnection::Primary(c) => c.peer_id(),
            FallbackConnection::Fallback(c) => c.peer_id(),
        }
    }
    fn kind(&self) -> TransportKind {
        // Both halves of FallbackTransport feed the *primary* slot of the
        // browser's outer MultiTransport (the secondary slot is WebRTC).
        // Returning `Primary` matches the existing engine convention —
        // see `MultiConnection::Primary` reporting `TransportKind::Primary`.
        TransportKind::Primary
    }
    async fn close(&self) -> Result<()> {
        match self {
            FallbackConnection::Primary(c) => c.close().await,
            FallbackConnection::Fallback(c) => c.close().await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewrites_wt_to_ws_and_drops_cert_hash() {
        let cert_hex = "ee".repeat(32);
        let x25519_hex = "11".repeat(32);
        let wt_url = format!("wt://127.0.0.1:8443#x25519={x25519_hex}&cert-sha256={cert_hex}");
        let wt = PeerAddr::new(Bytes::from(wt_url));
        let ws = fallback_addr_for(&wt).unwrap();
        let s = std::str::from_utf8(ws.as_bytes()).unwrap();
        assert!(s.starts_with("ws://127.0.0.1:8443#x25519="), "got: {s}");
        assert!(
            !s.contains("cert-sha256="),
            "cert hash should be stripped: {s}"
        );
    }

    #[test]
    fn rewrites_wts_to_wss() {
        let url = PeerAddr::new(Bytes::from("wts://relay.example.com#x25519=aa".to_string()));
        let ws = fallback_addr_for(&url).unwrap();
        assert_eq!(
            std::str::from_utf8(ws.as_bytes()).unwrap(),
            "wss://relay.example.com#x25519=aa"
        );
    }

    #[test]
    fn drops_fragment_when_only_cert_hash_is_present() {
        let url = PeerAddr::new(Bytes::from(format!(
            "wt://127.0.0.1:8443#cert-sha256={}",
            "ee".repeat(32)
        )));
        let ws = fallback_addr_for(&url).unwrap();
        assert_eq!(
            std::str::from_utf8(ws.as_bytes()).unwrap(),
            "ws://127.0.0.1:8443"
        );
    }

    #[test]
    fn rejects_non_wt_url() {
        let addr = PeerAddr::new(Bytes::from("ws://relay.example.com"));
        assert!(fallback_addr_for(&addr).is_err());
    }
}
