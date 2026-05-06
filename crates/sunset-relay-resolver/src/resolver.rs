//! Orchestrates parse → fetch → extract. The HTTP transport is
//! abstracted via [`HttpFetch`] so this crate stays platform-neutral
//! and unit-testable; consumers (`sunset-relay`, `sunset-web-wasm`)
//! supply concrete `reqwest` / `web-sys::fetch` implementations.

use std::rc::Rc;

use async_trait::async_trait;

use crate::error::Result;
use crate::json::{extract_string_field, extract_x25519_from_json};
use crate::parse::{ParsedInput, parse_input};

/// Returns the body of `GET <url>` as a string, or an [`Error::Http`]
/// describing the failure.
///
/// `?Send` matches the codebase's WASM convention; backends are
/// single-threaded.
#[async_trait(?Send)]
pub trait HttpFetch {
    async fn get(&self, url: &str) -> Result<String>;
}

/// Blanket forwarding impl so callers can hand a shared `Rc<dyn HttpFetch>`
/// to [`Resolver::new`] (the supervisor's `Connectable::Resolving` variant
/// stores the fetcher as `Rc<dyn HttpFetch>` and clones it per dial).
#[async_trait(?Send)]
impl HttpFetch for Rc<dyn HttpFetch> {
    async fn get(&self, url: &str) -> Result<String> {
        (**self).get(url).await
    }
}

/// Output of [`Resolver::resolve_with_fallback`]. The `primary` URL is
/// preferred by callers; if absent or unreachable they may dial
/// `fallback` (always WS, present whenever the relay advertises any
/// reachable address).
#[derive(Clone, Debug)]
pub struct ResolvedAddress {
    /// Preferred URL. When the relay's identity descriptor contains a
    /// `webtransport_address` field, this is the WT URL (with
    /// `cert-sha256=` fragment). Otherwise this is the same as
    /// `fallback` and callers can ignore it.
    pub primary: String,
    /// WebSocket URL — always present. Used when the relay didn't
    /// advertise WT, or when the WT dial fails and the caller wants to
    /// fall back.
    pub fallback: String,
}

pub struct Resolver<F: HttpFetch> {
    fetch: F,
}

impl<F: HttpFetch> Resolver<F> {
    pub fn new(fetch: F) -> Self {
        Self { fetch }
    }

    /// Resolve a user-typed input string into a canonical
    /// `wss://host[:port]#x25519=<hex>` PeerAddr string. Inputs that
    /// already carry an `#x25519=…` fragment are returned unchanged
    /// without an HTTP fetch.
    ///
    /// This is the legacy single-output API used by callers that don't
    /// participate in the WT/WS fallback flow (the relay's federated
    /// dialer, integration tests). Browser / Client code should call
    /// [`Self::resolve_with_fallback`] instead.
    pub async fn resolve(&self, input: &str) -> Result<String> {
        Ok(self.resolve_with_fallback(input).await?.primary)
    }

    /// Resolve into both a primary URL (WT-preferred) and a fallback
    /// URL (WS). Inputs already in canonical `wss://…#x25519=…` form
    /// are returned with `primary == fallback` and no HTTP fetch.
    ///
    /// The WT URL is built from the user-typed authority (matching how
    /// the WS URL has always been built from `target.ws_url`), with the
    /// cert SPKI hash supplied by the descriptor's
    /// `webtransport_cert_sha256` field. This mirrors the WS path's
    /// long-standing discipline: the descriptor authenticates the
    /// destination but doesn't *direct* traffic, because the relay has
    /// no reliable way to know its own public hostname (it could be
    /// behind any number of proxies, and indeed binds `0.0.0.0` in
    /// production).
    pub async fn resolve_with_fallback(&self, input: &str) -> Result<ResolvedAddress> {
        match parse_input(input)? {
            ParsedInput::Canonical(s) => Ok(ResolvedAddress {
                primary: s.clone(),
                fallback: s,
            }),
            ParsedInput::Lookup(target) => {
                let body = self.fetch.get(&target.http_url).await?;
                let x_hex = hex::encode(extract_x25519_from_json(&body)?);
                let ws_url = format!("{}#x25519={x_hex}", target.ws_url);
                let primary = match extract_string_field(&body, "webtransport_cert_sha256")? {
                    Some(cert_hex) => {
                        let wt_authority = wt_url_from_ws_url(&target.ws_url);
                        format!("{wt_authority}#x25519={x_hex}&cert-sha256={cert_hex}")
                    }
                    None => ws_url.clone(),
                };
                Ok(ResolvedAddress {
                    primary,
                    fallback: ws_url,
                })
            }
        }
    }
}

/// Derive a WebTransport scheme+authority URL from a WebSocket
/// scheme+authority URL by rewriting `ws://`→`wt://` and `wss://`→`wts://`.
/// `ws_url` is expected to be a fragment-less prefix (e.g.
/// `wss://relay.example.com:443`); the caller supplies the
/// `#x25519=…&cert-sha256=…` fragment afterwards.
fn wt_url_from_ws_url(ws_url: &str) -> String {
    if let Some(rest) = ws_url.strip_prefix("wss://") {
        format!("wts://{rest}")
    } else if let Some(rest) = ws_url.strip_prefix("ws://") {
        format!("wt://{rest}")
    } else {
        // `parse_input` only ever produces ws:// or wss:// for
        // `Lookup` outputs, so this branch is unreachable in the
        // current code path. Surface a recognizable URL rather than
        // panicking — the caller will fail to dial WT (no scheme
        // match in `MultiTransport::connect`) and fall back to WS.
        format!("wt-unknown-scheme://{ws_url}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::Error;
    use std::cell::RefCell;

    /// Fake fetcher that returns a pre-canned (url -> body) mapping
    /// or an Http error when no entry matches. Single-threaded
    /// (RefCell, not Mutex) — fine because trait is ?Send.
    struct FakeFetch {
        responses: RefCell<Vec<(String, std::result::Result<String, Error>)>>,
        seen: RefCell<Vec<String>>,
    }

    impl FakeFetch {
        fn new() -> Self {
            Self {
                responses: RefCell::new(Vec::new()),
                seen: RefCell::new(Vec::new()),
            }
        }
        fn ok(self, url: &str, body: &str) -> Self {
            self.responses
                .borrow_mut()
                .push((url.into(), Ok(body.into())));
            self
        }
        fn err(self, url: &str, error: Error) -> Self {
            self.responses.borrow_mut().push((url.into(), Err(error)));
            self
        }
    }

    #[async_trait(?Send)]
    impl HttpFetch for FakeFetch {
        async fn get(&self, url: &str) -> Result<String> {
            self.seen.borrow_mut().push(url.into());
            for (u, r) in self.responses.borrow().iter() {
                if u == url {
                    return r.clone();
                }
            }
            Err(Error::Http(format!("no fake response for {url}")))
        }
    }

    fn good_body(hex: &str) -> String {
        format!(
            "{{\"ed25519\":\"{}\",\"x25519\":\"{}\",\"address\":\"ws://x:1\"}}\n",
            "11".repeat(32),
            hex,
        )
    }

    #[tokio::test]
    async fn canonical_input_does_not_fetch() {
        let canonical = "wss://relay.example.com:443#x25519=0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let fake = FakeFetch::new();
        let resolver = Resolver::new(fake);
        let out = resolver.resolve(canonical).await.unwrap();
        assert_eq!(out, canonical);
        assert!(resolver.fetch.seen.borrow().is_empty());
    }

    #[tokio::test]
    async fn loopback_host_fetches_http_returns_ws() {
        let hex = "ab".repeat(32);
        let fake = FakeFetch::new().ok("http://127.0.0.1:8443/", &good_body(&hex));
        let resolver = Resolver::new(fake);
        let out = resolver.resolve("127.0.0.1:8443").await.unwrap();
        assert_eq!(out, format!("ws://127.0.0.1:8443#x25519={hex}"));
    }

    #[tokio::test]
    async fn public_host_fetches_https_returns_wss() {
        let hex = "cd".repeat(32);
        let fake = FakeFetch::new().ok("https://relay.sunset.chat/", &good_body(&hex));
        let resolver = Resolver::new(fake);
        let out = resolver.resolve("relay.sunset.chat").await.unwrap();
        assert_eq!(out, format!("wss://relay.sunset.chat#x25519={hex}"));
    }

    #[tokio::test]
    async fn http_error_surfaces() {
        let fake = FakeFetch::new().err(
            "https://relay.sunset.chat/",
            Error::Http("status 502".into()),
        );
        let resolver = Resolver::new(fake);
        let err = resolver.resolve("relay.sunset.chat").await.unwrap_err();
        assert!(matches!(err, Error::Http(_)), "got: {err:?}");
    }

    #[tokio::test]
    async fn bad_json_surfaces() {
        let fake = FakeFetch::new().ok("https://relay.sunset.chat/", "not json");
        let resolver = Resolver::new(fake);
        let err = resolver.resolve("relay.sunset.chat").await.unwrap_err();
        assert!(matches!(err, Error::BadJson(_)), "got: {err:?}");
    }

    fn body_with_wt_cert(x_hex: &str, cert_hex: &str) -> String {
        // Descriptor format after the bug fix: exposes only the
        // `webtransport_cert_sha256` hash, not a full URL. The resolver
        // synthesizes the WT URL from the user-typed authority — matching
        // how the WS URL has always been constructed (from `target.ws_url`
        // rather than the descriptor's `address` field).
        format!(
            "{{\"ed25519\":\"{}\",\"x25519\":\"{}\",\"address\":\"ws://x:1\",\"webtransport_cert_sha256\":\"{}\"}}\n",
            "11".repeat(32),
            x_hex,
            cert_hex,
        )
    }

    #[tokio::test]
    async fn resolve_with_fallback_picks_wt_when_descriptor_advertises_cert_sha256_loopback() {
        let x_hex = "ab".repeat(32);
        let cert_hex = "ee".repeat(32);
        let fake = FakeFetch::new().ok(
            "http://127.0.0.1:8443/",
            &body_with_wt_cert(&x_hex, &cert_hex),
        );
        let resolver = Resolver::new(fake);
        let resolved = resolver
            .resolve_with_fallback("127.0.0.1:8443")
            .await
            .unwrap();
        // Loopback host => `wt://` (paralleling the `ws://` heuristic).
        assert_eq!(
            resolved.primary,
            format!("wt://127.0.0.1:8443#x25519={x_hex}&cert-sha256={cert_hex}")
        );
        assert_eq!(
            resolved.fallback,
            format!("ws://127.0.0.1:8443#x25519={x_hex}")
        );
    }

    #[tokio::test]
    async fn resolve_with_fallback_uses_user_authority_not_descriptor_bind_addr() {
        // Production regression: the relay binds `0.0.0.0:8443` and
        // (in the buggy version) advertised `webtransport_address:
        // "wt://0.0.0.0:8443#…"` which the resolver returned verbatim
        // as primary. Browser then dialed `https://0.0.0.0:8443/` and
        // got `WebTransport connection rejected`. Fix: the descriptor
        // must NOT carry a URL — only the cert hash — and the resolver
        // must build the URL from the user's typed authority. Same
        // discipline as the WS path.
        let x_hex = "cd".repeat(32);
        let cert_hex = "f3".repeat(32);
        let fake = FakeFetch::new().ok(
            "https://relay.sunset.chat/",
            &body_with_wt_cert(&x_hex, &cert_hex),
        );
        let resolver = Resolver::new(fake);
        let resolved = resolver
            .resolve_with_fallback("relay.sunset.chat")
            .await
            .unwrap();
        // Public host => `wts://` (paralleling the `wss://` heuristic).
        assert_eq!(
            resolved.primary,
            format!("wts://relay.sunset.chat#x25519={x_hex}&cert-sha256={cert_hex}")
        );
        assert_eq!(
            resolved.fallback,
            format!("wss://relay.sunset.chat#x25519={x_hex}")
        );
        // Crucially: the bind address from `webtransport_address` MUST NOT
        // appear anywhere in the resolved URLs. (The old buggy code emitted
        // `wt://0.0.0.0:8443#…` here — that's the bug we're guarding against.)
        assert!(
            !resolved.primary.contains("0.0.0.0"),
            "primary leaked bind addr: {}",
            resolved.primary
        );
        assert!(
            !resolved.fallback.contains("0.0.0.0"),
            "fallback leaked bind addr: {}",
            resolved.fallback
        );
    }

    #[tokio::test]
    async fn resolve_with_fallback_legacy_relay_returns_ws_for_both() {
        // Old relay (pre-WebTransport) that doesn't ship any
        // `webtransport_*` fields. Primary and fallback should be
        // the same WS URL.
        let hex = "cd".repeat(32);
        let fake = FakeFetch::new().ok("http://127.0.0.1:8443/", &good_body(&hex));
        let resolver = Resolver::new(fake);
        let resolved = resolver
            .resolve_with_fallback("127.0.0.1:8443")
            .await
            .unwrap();
        let expected = format!("ws://127.0.0.1:8443#x25519={hex}");
        assert_eq!(resolved.primary, expected);
        assert_eq!(resolved.fallback, expected);
    }

    #[tokio::test]
    async fn resolve_with_fallback_ignores_legacy_webtransport_address_field() {
        // Existing-deployment compatibility: relays already running with
        // the bug emit `webtransport_address: "wt://0.0.0.0:…"`. The new
        // resolver doesn't read that field at all — if `webtransport_cert_sha256`
        // is absent, the resolver returns plain WS as both primary and
        // fallback. This means a stale already-deployed buggy relay
        // gracefully falls back to WS-only on the new client (the
        // pre-PR behaviour) rather than re-tripping the bug.
        let x_hex = "ab".repeat(32);
        let bogus_url = "wt://0.0.0.0:8443#x25519=00&cert-sha256=00";
        let body = format!(
            "{{\"ed25519\":\"{}\",\"x25519\":\"{}\",\"address\":\"ws://x:1\",\"webtransport_address\":\"{bogus_url}\"}}\n",
            "11".repeat(32),
            x_hex,
        );
        let fake = FakeFetch::new().ok("http://127.0.0.1:8443/", &body);
        let resolver = Resolver::new(fake);
        let resolved = resolver
            .resolve_with_fallback("127.0.0.1:8443")
            .await
            .unwrap();
        let expected = format!("ws://127.0.0.1:8443#x25519={x_hex}");
        assert_eq!(resolved.primary, expected, "primary should be WS-only");
        assert_eq!(resolved.fallback, expected);
    }
}
