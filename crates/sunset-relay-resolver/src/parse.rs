//! Parses user-typed relay addresses into either a canonical
//! `wss://host[:port]#x25519=<hex>` (pass-through) or a [`LookupTarget`]
//! that points at the http endpoint we'll fetch the identity from and
//! the ws endpoint we'll dial once we have the x25519.
//!
//! Loopback hosts (`127.0.0.1`, `::1`, `localhost`) default to plain
//! `http`/`ws`; everything else defaults to TLS (`https`/`wss`). An
//! explicit `ws://` / `http://` prefix overrides the loopback heuristic.

use crate::error::{Error, Result};

#[derive(Debug, PartialEq, Eq)]
pub enum ParsedInput {
    /// Input already carries an `#x25519=…` fragment; pass through unchanged.
    Canonical(String),
    /// Need to fetch the relay's identity JSON to learn x25519.
    Lookup(LookupTarget),
}

#[derive(Debug, PartialEq, Eq)]
pub struct LookupTarget {
    /// `https://host[:port]/` (or `http://` for loopback / explicit ws).
    pub http_url: String,
    /// `wss://host[:port]` (or `ws://` for loopback / explicit ws).
    /// The `#x25519=<hex>` fragment is appended by the resolver after
    /// the fetch succeeds.
    pub ws_url: String,
}

pub fn parse_input(input: &str) -> Result<ParsedInput> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(Error::MalformedInput("empty".into()));
    }

    // Already canonical?
    if let Some((url, fragment)) = trimmed.split_once('#') {
        if fragment.starts_with("x25519=") {
            // Pass through. We don't validate the hex here — that's
            // the noise crate's job, and a stricter check here would
            // duplicate it.
            let _ = url; // explicit: we're returning the whole trimmed input.
            return Ok(ParsedInput::Canonical(trimmed.to_string()));
        }
        return Err(Error::MalformedInput(format!(
            "fragment is not x25519=…: {trimmed}"
        )));
    }

    // No fragment: extract host[:port] and explicit scheme (if any).
    let (host_port, explicit) = if let Some(rest) = trimmed.strip_prefix("wss://") {
        (rest, Some(Scheme::Wss))
    } else if let Some(rest) = trimmed.strip_prefix("ws://") {
        (rest, Some(Scheme::Ws))
    } else if let Some(rest) = trimmed.strip_prefix("https://") {
        (rest, Some(Scheme::Wss))
    } else if let Some(rest) = trimmed.strip_prefix("http://") {
        (rest, Some(Scheme::Ws))
    } else if trimmed.contains("://") {
        return Err(Error::MalformedInput(format!(
            "unsupported scheme: {trimmed}"
        )));
    } else {
        (trimmed, None)
    };

    // Strip a single trailing '/' (e.g. "wss://host/"), reject any
    // path-like input.
    let host_port = host_port.strip_suffix('/').unwrap_or(host_port);
    if host_port.contains('/') {
        return Err(Error::MalformedInput(format!(
            "path components not supported: {trimmed}"
        )));
    }
    if host_port.is_empty() {
        return Err(Error::MalformedInput("empty host".into()));
    }

    let scheme = explicit.unwrap_or_else(|| {
        if is_loopback_host(host_without_port(host_port)) {
            Scheme::Ws
        } else {
            Scheme::Wss
        }
    });

    let (http_scheme, ws_scheme) = match scheme {
        Scheme::Ws => ("http", "ws"),
        Scheme::Wss => ("https", "wss"),
    };

    Ok(ParsedInput::Lookup(LookupTarget {
        http_url: format!("{http_scheme}://{host_port}/"),
        ws_url: format!("{ws_scheme}://{host_port}"),
    }))
}

#[derive(Copy, Clone)]
enum Scheme {
    Ws,
    Wss,
}

fn host_without_port(host_port: &str) -> &str {
    if host_port.starts_with('[') {
        if let Some(close) = host_port.rfind(']') {
            return &host_port[..=close];
        }
    }
    host_port.rsplit_once(':').map(|(h, _)| h).unwrap_or(host_port)
}

fn is_loopback_host(host: &str) -> bool {
    matches!(host, "127.0.0.1" | "localhost" | "[::1]" | "::1")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_passes_through() {
        let input =
            "wss://relay.example.com:443#x25519=0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        assert_eq!(parse_input(input).unwrap(), ParsedInput::Canonical(input.to_string()));
    }

    #[test]
    fn canonical_with_ws_scheme_passes_through() {
        let input =
            "ws://127.0.0.1:8443#x25519=0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        assert_eq!(parse_input(input).unwrap(), ParsedInput::Canonical(input.to_string()));
    }

    #[test]
    fn bare_hostname_defaults_to_tls() {
        let parsed = parse_input("relay.sunset.chat").unwrap();
        assert_eq!(
            parsed,
            ParsedInput::Lookup(LookupTarget {
                http_url: "https://relay.sunset.chat/".into(),
                ws_url: "wss://relay.sunset.chat".into(),
            })
        );
    }

    #[test]
    fn host_with_port_defaults_to_tls() {
        let parsed = parse_input("relay.sunset.chat:8443").unwrap();
        assert_eq!(
            parsed,
            ParsedInput::Lookup(LookupTarget {
                http_url: "https://relay.sunset.chat:8443/".into(),
                ws_url: "wss://relay.sunset.chat:8443".into(),
            })
        );
    }

    #[test]
    fn loopback_127_defaults_to_plain() {
        let parsed = parse_input("127.0.0.1:8443").unwrap();
        assert_eq!(
            parsed,
            ParsedInput::Lookup(LookupTarget {
                http_url: "http://127.0.0.1:8443/".into(),
                ws_url: "ws://127.0.0.1:8443".into(),
            })
        );
    }

    #[test]
    fn loopback_localhost_defaults_to_plain() {
        let parsed = parse_input("localhost:8443").unwrap();
        assert_eq!(
            parsed,
            ParsedInput::Lookup(LookupTarget {
                http_url: "http://localhost:8443/".into(),
                ws_url: "ws://localhost:8443".into(),
            })
        );
    }

    #[test]
    fn loopback_ipv6_defaults_to_plain() {
        let parsed = parse_input("[::1]:8443").unwrap();
        assert_eq!(
            parsed,
            ParsedInput::Lookup(LookupTarget {
                http_url: "http://[::1]:8443/".into(),
                ws_url: "ws://[::1]:8443".into(),
            })
        );
    }

    #[test]
    fn explicit_wss_scheme_uses_https() {
        let parsed = parse_input("wss://relay.sunset.chat").unwrap();
        assert_eq!(
            parsed,
            ParsedInput::Lookup(LookupTarget {
                http_url: "https://relay.sunset.chat/".into(),
                ws_url: "wss://relay.sunset.chat".into(),
            })
        );
    }

    #[test]
    fn explicit_ws_overrides_remote_default() {
        // ws:// on a non-loopback host: user explicitly wants plain.
        let parsed = parse_input("ws://relay.sunset.chat:8443").unwrap();
        assert_eq!(
            parsed,
            ParsedInput::Lookup(LookupTarget {
                http_url: "http://relay.sunset.chat:8443/".into(),
                ws_url: "ws://relay.sunset.chat:8443".into(),
            })
        );
    }

    #[test]
    fn explicit_https_scheme_uses_https() {
        let parsed = parse_input("https://relay.sunset.chat:8443").unwrap();
        assert_eq!(
            parsed,
            ParsedInput::Lookup(LookupTarget {
                http_url: "https://relay.sunset.chat:8443/".into(),
                ws_url: "wss://relay.sunset.chat:8443".into(),
            })
        );
    }

    #[test]
    fn explicit_http_scheme_uses_http() {
        let parsed = parse_input("http://relay.sunset.chat:8443").unwrap();
        assert_eq!(
            parsed,
            ParsedInput::Lookup(LookupTarget {
                http_url: "http://relay.sunset.chat:8443/".into(),
                ws_url: "ws://relay.sunset.chat:8443".into(),
            })
        );
    }

    #[test]
    fn trailing_slash_is_accepted() {
        let parsed = parse_input("wss://relay.sunset.chat/").unwrap();
        assert_eq!(
            parsed,
            ParsedInput::Lookup(LookupTarget {
                http_url: "https://relay.sunset.chat/".into(),
                ws_url: "wss://relay.sunset.chat".into(),
            })
        );
    }

    #[test]
    fn empty_input_rejected() {
        assert!(matches!(parse_input(""), Err(Error::MalformedInput(_))));
        assert!(matches!(parse_input("   "), Err(Error::MalformedInput(_))));
    }

    #[test]
    fn unknown_scheme_rejected() {
        assert!(matches!(
            parse_input("ftp://relay.sunset.chat"),
            Err(Error::MalformedInput(_))
        ));
    }

    #[test]
    fn path_components_rejected() {
        assert!(matches!(
            parse_input("relay.sunset.chat/foo"),
            Err(Error::MalformedInput(_))
        ));
        assert!(matches!(
            parse_input("wss://relay.sunset.chat/some/path"),
            Err(Error::MalformedInput(_))
        ));
    }

    #[test]
    fn fragment_without_x25519_rejected() {
        assert!(matches!(
            parse_input("wss://relay.sunset.chat#something-else"),
            Err(Error::MalformedInput(_))
        ));
    }

    #[test]
    fn whitespace_is_trimmed() {
        let parsed = parse_input("  relay.sunset.chat  ").unwrap();
        assert_eq!(
            parsed,
            ParsedInput::Lookup(LookupTarget {
                http_url: "https://relay.sunset.chat/".into(),
                ws_url: "wss://relay.sunset.chat".into(),
            })
        );
    }
}
