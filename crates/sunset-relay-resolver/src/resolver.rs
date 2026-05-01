//! Orchestrates parse → fetch → extract. The HTTP transport is
//! abstracted via [`HttpFetch`] so this crate stays platform-neutral
//! and unit-testable; consumers (`sunset-relay`, `sunset-web-wasm`)
//! supply concrete `reqwest` / `web-sys::fetch` implementations.

use async_trait::async_trait;

use crate::error::Result;
use crate::json::extract_x25519_from_json;
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
    pub async fn resolve(&self, input: &str) -> Result<String> {
        match parse_input(input)? {
            ParsedInput::Canonical(s) => Ok(s),
            ParsedInput::Lookup(target) => {
                let body = self.fetch.get(&target.http_url).await?;
                let x = extract_x25519_from_json(&body)?;
                Ok(format!("{}#x25519={}", target.ws_url, hex::encode(x)))
            }
        }
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
        let canonical =
            "wss://relay.example.com:443#x25519=0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
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
}
