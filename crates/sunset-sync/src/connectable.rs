//! What a supervisor-managed intent dials.
//!
//! `Direct(addr)` is a canonical `PeerAddr` (already carries
//! `#x25519=<hex>`) — no pre-dial work; `resolve_addr` returns a clone.
//!
//! `Resolving { input, fetch }` carries a user-typed string
//! (`relay.sunset.chat`, `wss://host:port`, …) plus an `HttpFetch`
//! impl. Each dial attempt runs the resolver to learn the relay's
//! x25519 key — re-resolving every attempt covers a relay that
//! rotates identity between deploys.

use std::rc::Rc;

use bytes::Bytes;
use sunset_relay_resolver::{HttpFetch, Resolver};

use crate::types::PeerAddr;

#[derive(Clone)]
pub enum Connectable {
    Direct(PeerAddr),
    Resolving {
        input: String,
        fetch: Rc<dyn HttpFetch>,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum ResolveErr {
    /// Permanent — the input string can't be parsed at all. The
    /// supervisor cancels the intent on this.
    #[error("parse error: {0}")]
    Parse(String),
    /// Transient — HTTP fetch / JSON / hex / I/O failed. The
    /// supervisor backs off and retries.
    #[error("transient resolve failure: {0}")]
    Transient(String),
}

impl From<crate::error::Error> for ResolveErr {
    fn from(e: crate::error::Error) -> Self {
        ResolveErr::Transient(format!("{e}"))
    }
}

impl Connectable {
    /// A short string that identifies this intent for UI display
    /// before a `peer_id` is known. For `Direct`, the canonical URL
    /// (which the user pasted themselves); for `Resolving`, the input.
    pub fn label(&self) -> String {
        match self {
            Connectable::Direct(addr) => String::from_utf8_lossy(addr.as_bytes()).into_owned(),
            Connectable::Resolving { input, .. } => input.clone(),
        }
    }

    /// Produce the canonical `PeerAddr` to dial. For `Direct`, returns
    /// a clone immediately. For `Resolving`, runs the resolver via the
    /// supplied `HttpFetch`. `ResolveErr::Parse` is permanent;
    /// `ResolveErr::Transient` is retried.
    pub async fn resolve_addr(&self) -> Result<PeerAddr, ResolveErr> {
        match self {
            Connectable::Direct(addr) => Ok(addr.clone()),
            Connectable::Resolving { input, fetch } => {
                let resolver = Resolver::new(fetch.clone());
                match resolver.resolve(input).await {
                    Ok(canonical) => Ok(PeerAddr::new(Bytes::from(canonical))),
                    Err(sunset_relay_resolver::Error::MalformedInput(e)) => {
                        Err(ResolveErr::Parse(e))
                    }
                    Err(e) => Err(ResolveErr::Transient(format!("{e}"))),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::cell::RefCell;
    use sunset_relay_resolver::Result as ResolverResult;

    /// Fake fetcher that returns a pre-canned (url -> body) mapping
    /// per call, so tests can assert how many attempts have happened.
    struct FakeFetch {
        responses: RefCell<Vec<std::result::Result<String, sunset_relay_resolver::Error>>>,
        seen_count: RefCell<usize>,
    }

    impl FakeFetch {
        fn new(
            responses: Vec<std::result::Result<String, sunset_relay_resolver::Error>>,
        ) -> Rc<Self> {
            Rc::new(Self {
                responses: RefCell::new(responses),
                seen_count: RefCell::new(0),
            })
        }
    }

    #[async_trait(?Send)]
    impl HttpFetch for FakeFetch {
        async fn get(&self, _url: &str) -> ResolverResult<String> {
            *self.seen_count.borrow_mut() += 1;
            self.responses.borrow_mut().remove(0)
        }
    }

    fn good_body(hex: &str) -> String {
        format!(
            "{{\"ed25519\":\"{}\",\"x25519\":\"{}\",\"address\":\"ws://x:1\"}}",
            "11".repeat(32),
            hex,
        )
    }

    #[tokio::test]
    async fn direct_returns_addr_clone() {
        let addr = PeerAddr::new(Bytes::from_static(b"wss://example#x25519=00"));
        let c = Connectable::Direct(addr.clone());
        let resolved = c.resolve_addr().await.unwrap();
        assert_eq!(resolved, addr);
    }

    #[tokio::test]
    async fn resolving_calls_fetcher_and_returns_canonical() {
        let hex = "ab".repeat(32);
        let body = good_body(&hex);
        let fetch = FakeFetch::new(vec![Ok(body)]);
        let c = Connectable::Resolving {
            input: "relay.example.com".into(),
            fetch: fetch.clone(),
        };
        let resolved = c.resolve_addr().await.unwrap();
        assert_eq!(*fetch.seen_count.borrow(), 1);
        let s = String::from_utf8(resolved.as_bytes().to_vec()).unwrap();
        assert!(s.starts_with("wss://relay.example.com#x25519="));
        assert!(s.ends_with(&hex));
    }

    #[tokio::test]
    async fn resolving_parse_error_is_permanent() {
        // Empty string is unparseable per `parse_input`.
        let fetch = FakeFetch::new(vec![]);
        let c = Connectable::Resolving {
            input: "".into(),
            fetch,
        };
        let err = c.resolve_addr().await.unwrap_err();
        assert!(matches!(err, ResolveErr::Parse(_)), "got {err:?}");
    }

    #[tokio::test]
    async fn resolving_http_error_is_transient() {
        let fetch = FakeFetch::new(vec![Err(sunset_relay_resolver::Error::Http(
            "status 503".into(),
        ))]);
        let c = Connectable::Resolving {
            input: "relay.example.com".into(),
            fetch,
        };
        let err = c.resolve_addr().await.unwrap_err();
        assert!(matches!(err, ResolveErr::Transient(_)), "got {err:?}");
    }

    #[test]
    fn label_for_direct_is_the_addr_string() {
        let addr = PeerAddr::new(Bytes::from_static(b"wss://h:1#x25519=00"));
        let c = Connectable::Direct(addr);
        assert_eq!(c.label(), "wss://h:1#x25519=00");
    }

    #[test]
    fn label_for_resolving_is_the_input() {
        let fetch = FakeFetch::new(vec![]);
        let c = Connectable::Resolving {
            input: "relay.sunset.chat".into(),
            fetch,
        };
        assert_eq!(c.label(), "relay.sunset.chat");
    }
}
