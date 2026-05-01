# Hostname-Only Relay Dialing Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let users dial a relay by typing just `relay.sunset.chat` (or `host:port`) by querying the relay's existing `GET /` JSON endpoint to learn its x25519 static key, and substituting it into a canonical `wss://host[:port]#x25519=<hex>` PeerAddr before the Noise IK handshake.

**Architecture:** Add a new `sunset-relay-resolver` crate with a pure parser (`parse_input`), a JSON extractor (`extract_x25519_from_json`), and a `Resolver<F: HttpFetch>` that orchestrates them via a `HttpFetch` trait. The crate ships no HTTP implementation — `sunset-relay` supplies a `reqwest`-backed adapter, `sunset-web-wasm` supplies a `web-sys::fetch`-backed adapter. Each consumer routes user-typed peer addresses through the resolver before calling `engine.add_peer`. Inputs that already carry an `#x25519=…` fragment short-circuit and skip the HTTP fetch — the existing tests and on-disk configs keep working unchanged.

**Tech Stack:** Rust 2024 edition, `async-trait` (?Send for WASM compat), `hex`, `thiserror`, `reqwest` (rustls-tls — new workspace dep), `web-sys::fetch` (already a dep of sunset-web-wasm).

---

## File structure

**New crate:**
- `crates/sunset-relay-resolver/Cargo.toml` — package metadata; deps: `async-trait`, `hex`, `thiserror`.
- `crates/sunset-relay-resolver/src/lib.rs` — re-exports + module declarations.
- `crates/sunset-relay-resolver/src/error.rs` — `Error` enum + `Result` alias.
- `crates/sunset-relay-resolver/src/parse.rs` — `parse_input`, `ParsedInput`, `LookupTarget`, loopback heuristic. Inline unit tests.
- `crates/sunset-relay-resolver/src/json.rs` — `extract_x25519_from_json`. Inline unit tests.
- `crates/sunset-relay-resolver/src/resolver.rs` — `HttpFetch` trait + `Resolver<F>` struct. Inline unit tests with a fake fetcher.

**Workspace root:**
- `Cargo.toml` — register `sunset-relay-resolver` as a member, register `sunset-relay-resolver = { path = "..." }` as a workspace dep, add `reqwest = { default-features = false, features = ["rustls-tls"] }` as a workspace dep.

**Native consumer (`sunset-relay`):**
- `crates/sunset-relay/Cargo.toml` — depend on `sunset-relay-resolver` and `reqwest`.
- `crates/sunset-relay/src/resolver_adapter.rs` — `ReqwestFetch` adapter (~30 lines).
- `crates/sunset-relay/src/lib.rs` — `mod resolver_adapter;`.
- `crates/sunset-relay/src/relay.rs` — call resolver inside the federated-peers loop in both `run` and `run_for_test`.
- `crates/sunset-relay/tests/resolver_integration.rs` — boot a real relay, point `Resolver` at its bound address with bare `host:port`, assert canonical PeerAddr matches `dial_address()`.

**Browser consumer (`sunset-web-wasm`):**
- `crates/sunset-web-wasm/Cargo.toml` — depend on `sunset-relay-resolver`.
- `crates/sunset-web-wasm/src/resolver_adapter.rs` — `WebSysFetch` adapter.
- `crates/sunset-web-wasm/src/lib.rs` — `mod resolver_adapter;`.
- `crates/sunset-web-wasm/src/client.rs:124-137` — call resolver in `add_relay` before `engine.add_peer`.

---

## Task 1: Scaffold the resolver crate

**Files:**
- Create: `crates/sunset-relay-resolver/Cargo.toml`
- Create: `crates/sunset-relay-resolver/src/lib.rs`
- Create: `crates/sunset-relay-resolver/src/error.rs`
- Modify: `Cargo.toml` (workspace root)

- [ ] **Step 1: Add workspace member + dep**

Edit the workspace root `Cargo.toml`. In `[workspace] members`, append `"crates/sunset-relay-resolver"`. In `[workspace.dependencies]`, append:

```toml
sunset-relay-resolver = { path = "crates/sunset-relay-resolver" }
reqwest = { version = "0.12", default-features = false, features = ["rustls-tls"] }
```

- [ ] **Step 2: Create `crates/sunset-relay-resolver/Cargo.toml`**

```toml
[package]
name = "sunset-relay-resolver"
version.workspace = true
edition.workspace = true
license.workspace = true
rust-version.workspace = true

[lints]
workspace = true

[dependencies]
async-trait.workspace = true
hex.workspace = true
thiserror.workspace = true

[dev-dependencies]
tokio = { workspace = true, features = ["macros", "rt"] }
```

- [ ] **Step 3: Create `crates/sunset-relay-resolver/src/lib.rs`**

```rust
//! Resolves a user-typed relay address (e.g. `relay.sunset.chat:8443`)
//! into the canonical `wss://host[:port]#x25519=<hex>` PeerAddr the
//! Noise IK handshake expects, by querying the relay's `GET /` JSON
//! identity endpoint.
//!
//! This crate ships no HTTP implementation: callers supply an
//! [`HttpFetch`] impl. `sunset-relay` uses a `reqwest`-based one;
//! `sunset-web-wasm` uses a `web-sys::fetch`-based one. The pure
//! parsing / JSON-extraction code is unit-testable without any HTTP
//! dependency.

pub mod error;
pub mod json;
pub mod parse;
pub mod resolver;

pub use error::{Error, Result};
```

(Re-exports for the public types are added by later tasks as those types come into existence — keeping this file build-clean at every commit.)

- [ ] **Step 4: Create `crates/sunset-relay-resolver/src/error.rs`**

```rust
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("malformed input: {0}")]
    MalformedInput(String),
    #[error("http error: {0}")]
    Http(String),
    #[error("bad json: {0}")]
    BadJson(String),
    #[error("bad x25519: {0}")]
    BadX25519(String),
}

pub type Result<T> = std::result::Result<T, Error>;
```

- [ ] **Step 5: Create empty placeholder files so `cargo check` passes**

Create `crates/sunset-relay-resolver/src/parse.rs`:

```rust
//! Parses user-typed relay addresses. Filled in by Task 2.
```

Create `crates/sunset-relay-resolver/src/json.rs`:

```rust
//! Extracts the x25519 hex from the relay's identity JSON. Filled in by Task 3.
```

Create `crates/sunset-relay-resolver/src/resolver.rs`:

```rust
//! Orchestrates parse → fetch → extract. Filled in by Task 4.
```

- [ ] **Step 6: Build the new crate**

Run: `nix develop --command cargo build -p sunset-relay-resolver`
Expected: builds clean, no warnings about the empty modules (they're just doc comments).

- [ ] **Step 7: Commit**

```bash
git add Cargo.toml crates/sunset-relay-resolver/
git commit -m "Scaffold sunset-relay-resolver crate"
```

---

## Task 2: `parse_input` + `LookupTarget`

**Files:**
- Modify: `crates/sunset-relay-resolver/src/parse.rs`

- [ ] **Step 1: Write the failing tests**

Replace the contents of `crates/sunset-relay-resolver/src/parse.rs` with:

```rust
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

pub fn parse_input(_input: &str) -> Result<ParsedInput> {
    todo!()
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
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `nix develop --command cargo test -p sunset-relay-resolver --lib parse::tests`
Expected: all tests panic at `todo!()` in `parse_input`.

- [ ] **Step 3: Implement `parse_input`**

Replace the `pub fn parse_input` body in `crates/sunset-relay-resolver/src/parse.rs`:

```rust
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
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `nix develop --command cargo test -p sunset-relay-resolver --lib parse::tests`
Expected: all 14 tests pass.

- [ ] **Step 5: Re-export from `lib.rs`**

Open `crates/sunset-relay-resolver/src/lib.rs`, append after the existing `pub use error::{Error, Result};` line:

```rust
pub use parse::{LookupTarget, ParsedInput, parse_input};
```

Run: `nix develop --command cargo build -p sunset-relay-resolver`
Expected: builds clean.

- [ ] **Step 6: Commit**

```bash
git add crates/sunset-relay-resolver/src/parse.rs crates/sunset-relay-resolver/src/lib.rs
git commit -m "sunset-relay-resolver: parse_input + LookupTarget"
```

---

## Task 3: `extract_x25519_from_json`

**Files:**
- Modify: `crates/sunset-relay-resolver/src/json.rs`

- [ ] **Step 1: Write the failing tests**

Replace the contents of `crates/sunset-relay-resolver/src/json.rs` with:

```rust
//! Pulls the `x25519` field out of the relay's identity JSON. The
//! relay produces a fixed shape (see `sunset_relay::status::identity_json`):
//! `{"ed25519":"<hex>","x25519":"<hex>","address":"<url>"}` — three
//! fields, hex-only values, no nested objects. We hand-roll a tiny
//! scanner so this crate doesn't have to pull in serde_json.

use crate::error::{Error, Result};

pub fn extract_x25519_from_json(_body: &str) -> Result<[u8; 32]> {
    todo!()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn good_body(hex: &str) -> String {
        format!(
            "{{\"ed25519\":\"{}\",\"x25519\":\"{}\",\"address\":\"ws://x:1\"}}\n",
            "11".repeat(32),
            hex,
        )
    }

    #[test]
    fn extracts_well_formed() {
        let hex = "ab".repeat(32);
        let bytes = extract_x25519_from_json(&good_body(&hex)).unwrap();
        assert_eq!(bytes, [0xab; 32]);
    }

    #[test]
    fn handles_whitespace_around_colon() {
        let hex = "cd".repeat(32);
        let body = format!(
            "{{\n  \"ed25519\" : \"{}\",\n  \"x25519\"  :  \"{}\",\n  \"address\":\"ws://x:1\"\n}}\n",
            "11".repeat(32),
            hex,
        );
        let bytes = extract_x25519_from_json(&body).unwrap();
        assert_eq!(bytes, [0xcd; 32]);
    }

    #[test]
    fn missing_field_rejected() {
        let body = "{\"ed25519\":\"00\",\"address\":\"ws://x:1\"}";
        assert!(matches!(
            extract_x25519_from_json(body),
            Err(Error::BadJson(_))
        ));
    }

    #[test]
    fn malformed_no_quote_rejected() {
        let body = "{\"x25519\": notquoted}";
        assert!(matches!(
            extract_x25519_from_json(body),
            Err(Error::BadJson(_))
        ));
    }

    #[test]
    fn missing_colon_rejected() {
        let body = "{\"x25519\" \"abcd\"}";
        assert!(matches!(
            extract_x25519_from_json(body),
            Err(Error::BadJson(_))
        ));
    }

    #[test]
    fn unterminated_string_rejected() {
        let body = "{\"x25519\":\"deadbeef";
        assert!(matches!(
            extract_x25519_from_json(body),
            Err(Error::BadJson(_))
        ));
    }

    #[test]
    fn wrong_length_rejected() {
        let body = good_body("abcd");
        assert!(matches!(
            extract_x25519_from_json(&body),
            Err(Error::BadX25519(_))
        ));
    }

    #[test]
    fn non_hex_rejected() {
        let body = good_body(&"zz".repeat(32));
        assert!(matches!(
            extract_x25519_from_json(&body),
            Err(Error::BadX25519(_))
        ));
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `nix develop --command cargo test -p sunset-relay-resolver --lib json::tests`
Expected: all 8 tests panic at `todo!()`.

- [ ] **Step 3: Implement `extract_x25519_from_json`**

Replace the `pub fn extract_x25519_from_json` body in `crates/sunset-relay-resolver/src/json.rs`:

```rust
pub fn extract_x25519_from_json(body: &str) -> Result<[u8; 32]> {
    let key = "\"x25519\"";
    let key_start = body
        .find(key)
        .ok_or_else(|| Error::BadJson("missing \"x25519\" field".into()))?;
    let after_key = &body[key_start + key.len()..];
    let after_colon = after_key
        .trim_start()
        .strip_prefix(':')
        .ok_or_else(|| Error::BadJson("expected ':' after \"x25519\"".into()))?;
    let value = after_colon.trim_start();
    let body_quoted = value
        .strip_prefix('"')
        .ok_or_else(|| Error::BadJson("\"x25519\" value not a quoted string".into()))?;
    let close_quote = body_quoted
        .find('"')
        .ok_or_else(|| Error::BadJson("unterminated \"x25519\" string".into()))?;
    let hex_str = &body_quoted[..close_quote];
    if hex_str.len() != 64 {
        return Err(Error::BadX25519(format!(
            "expected 64 hex chars, got {}",
            hex_str.len()
        )));
    }
    if !hex_str.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(Error::BadX25519(format!(
            "non-hex chars in x25519: {hex_str}"
        )));
    }
    let bytes = hex::decode(hex_str)
        .map_err(|e| Error::BadX25519(format!("hex decode: {e}")))?;
    bytes
        .as_slice()
        .try_into()
        .map_err(|_| Error::BadX25519(format!("expected 32 bytes, got {}", bytes.len())))
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `nix develop --command cargo test -p sunset-relay-resolver --lib json::tests`
Expected: all 8 tests pass.

- [ ] **Step 5: Re-export from `lib.rs`**

Open `crates/sunset-relay-resolver/src/lib.rs`, append:

```rust
pub use json::extract_x25519_from_json;
```

Run: `nix develop --command cargo build -p sunset-relay-resolver`
Expected: builds clean.

- [ ] **Step 6: Commit**

```bash
git add crates/sunset-relay-resolver/src/json.rs crates/sunset-relay-resolver/src/lib.rs
git commit -m "sunset-relay-resolver: extract_x25519_from_json"
```

---

## Task 4: `HttpFetch` trait + `Resolver`

**Files:**
- Modify: `crates/sunset-relay-resolver/src/resolver.rs`

- [ ] **Step 1: Write the failing tests + trait/struct stubs**

Replace the contents of `crates/sunset-relay-resolver/src/resolver.rs` with:

```rust
//! Orchestrates parse → fetch → extract. The HTTP transport is
//! abstracted via [`HttpFetch`] so this crate stays platform-neutral
//! and unit-testable; consumers (`sunset-relay`, `sunset-web-wasm`)
//! supply concrete `reqwest` / `web-sys::fetch` implementations.

use async_trait::async_trait;

use crate::error::Result;

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
    pub async fn resolve(&self, _input: &str) -> Result<String> {
        let _ = &self.fetch;
        todo!()
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
                    return match r {
                        Ok(body) => Ok(body.clone()),
                        Err(e) => Err(match e {
                            Error::Http(s) => Error::Http(s.clone()),
                            Error::BadJson(s) => Error::BadJson(s.clone()),
                            Error::BadX25519(s) => Error::BadX25519(s.clone()),
                            Error::MalformedInput(s) => Error::MalformedInput(s.clone()),
                        }),
                    };
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
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `nix develop --command cargo test -p sunset-relay-resolver --lib resolver::tests`
Expected: 5 tests panic at `todo!()` in `Resolver::resolve`.

- [ ] **Step 3: Implement `Resolver::resolve`**

In `crates/sunset-relay-resolver/src/resolver.rs`, replace the `pub async fn resolve` body:

```rust
    pub async fn resolve(&self, input: &str) -> Result<String> {
        use crate::json::extract_x25519_from_json;
        use crate::parse::{ParsedInput, parse_input};
        match parse_input(input)? {
            ParsedInput::Canonical(s) => Ok(s),
            ParsedInput::Lookup(target) => {
                let body = self.fetch.get(&target.http_url).await?;
                let x = extract_x25519_from_json(&body)?;
                Ok(format!("{}#x25519={}", target.ws_url, hex::encode(x)))
            }
        }
    }
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `nix develop --command cargo test -p sunset-relay-resolver --lib resolver::tests`
Expected: 5 tests pass.

- [ ] **Step 5: Re-export from `lib.rs` and run the full crate suite**

Open `crates/sunset-relay-resolver/src/lib.rs`, append:

```rust
pub use resolver::{HttpFetch, Resolver};
```

Run: `nix develop --command cargo test -p sunset-relay-resolver`
Expected: all unit tests across `parse`, `json`, `resolver` pass (≈27 tests).

- [ ] **Step 6: Commit**

```bash
git add crates/sunset-relay-resolver/src/resolver.rs crates/sunset-relay-resolver/src/lib.rs
git commit -m "sunset-relay-resolver: HttpFetch trait + Resolver"
```

---

## Task 5: Native consumer — `ReqwestFetch` + integrate in `sunset-relay`

**Files:**
- Modify: `crates/sunset-relay/Cargo.toml`
- Create: `crates/sunset-relay/src/resolver_adapter.rs`
- Modify: `crates/sunset-relay/src/lib.rs`
- Modify: `crates/sunset-relay/src/relay.rs`
- Create: `crates/sunset-relay/tests/resolver_integration.rs`

- [ ] **Step 1: Add deps to `crates/sunset-relay/Cargo.toml`**

In the `[dependencies]` section, append:

```toml
sunset-relay-resolver.workspace = true
reqwest.workspace = true
```

Run: `nix develop --command cargo check -p sunset-relay`
Expected: compiles clean (deps available, not yet referenced).

- [ ] **Step 2: Create `crates/sunset-relay/src/resolver_adapter.rs`**

```rust
//! `reqwest`-backed [`HttpFetch`] for native callers.
//!
//! rustls-tls (workspace feature flag) so we don't grow an OpenSSL
//! system dependency — see CLAUDE.md hermeticity rule.

use async_trait::async_trait;

use sunset_relay_resolver::{Error, HttpFetch, Result};

pub struct ReqwestFetch {
    client: reqwest::Client,
}

impl ReqwestFetch {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::new(),
        }
    }
}

impl Default for ReqwestFetch {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait(?Send)]
impl HttpFetch for ReqwestFetch {
    async fn get(&self, url: &str) -> Result<String> {
        let resp = self
            .client
            .get(url)
            .send()
            .await
            .map_err(|e| Error::Http(format!("send: {e}")))?;
        if !resp.status().is_success() {
            return Err(Error::Http(format!("status {}", resp.status())));
        }
        resp.text()
            .await
            .map_err(|e| Error::Http(format!("body: {e}")))
    }
}
```

- [ ] **Step 3: Wire the module into `crates/sunset-relay/src/lib.rs`**

Open `crates/sunset-relay/src/lib.rs`, append `pub(crate) mod resolver_adapter;` next to the other `mod` declarations.

Run: `nix develop --command cargo build -p sunset-relay`
Expected: builds clean.

- [ ] **Step 4: Wire the resolver into `Relay::run` and `run_for_test`**

Open `crates/sunset-relay/src/relay.rs`. Near the top of `impl RelayHandle`, add this private async helper (above `pub async fn run`):

```rust
async fn dial_configured_peers(&self) {
    use sunset_relay_resolver::Resolver;
    let resolver = Resolver::new(crate::resolver_adapter::ReqwestFetch::default());
    for peer_url in &self.peers {
        let canonical = match resolver.resolve(peer_url).await {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(peer = %peer_url, error = %e, "peer resolve failed");
                continue;
            }
        };
        let addr = PeerAddr::new(Bytes::from(canonical));
        if let Err(e) = self.engine.add_peer(addr).await {
            tracing::warn!(peer = %peer_url, error = %e, "federated peer dial failed");
        } else {
            tracing::info!(peer = %peer_url, "federated peer dialed");
        }
    }
}
```

Then replace the existing peer-dial loop in `pub async fn run` (the `for peer_url in &self.peers { ... }` block) with a single call:

```rust
self.dial_configured_peers().await;
```

Do the same in `pub async fn run_for_test` — replace the `for peer_url in &self.peers { ... }` loop with `self.dial_configured_peers().await;`.

- [ ] **Step 5: Build and run existing tests**

Run: `nix develop --command cargo test -p sunset-relay --all-features`
Expected: all existing tests still pass (the canonical-form addresses used in `multi_relay.rs` short-circuit the resolver, so the WS handshake path is unchanged).

- [ ] **Step 6: Add integration test**

Create `crates/sunset-relay/tests/resolver_integration.rs`:

```rust
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
```

`reqwest` is already a regular dep (added in Step 1), so it's available to integration tests without further changes.

Run: `nix develop --command cargo test -p sunset-relay --test resolver_integration`
Expected: 1 test passes.

- [ ] **Step 7: Run full relay test suite**

Run: `nix develop --command cargo test -p sunset-relay --all-features`
Expected: all unit + integration + multi-relay tests pass.

- [ ] **Step 8: Commit**

```bash
git add crates/sunset-relay/Cargo.toml crates/sunset-relay/src/resolver_adapter.rs crates/sunset-relay/src/lib.rs crates/sunset-relay/src/relay.rs crates/sunset-relay/tests/resolver_integration.rs
git commit -m "sunset-relay: resolve federated peers via GET / before dialing"
```

---

## Task 6: Browser consumer — `WebSysFetch` + integrate in `sunset-web-wasm`

**Files:**
- Modify: `crates/sunset-web-wasm/Cargo.toml`
- Create: `crates/sunset-web-wasm/src/resolver_adapter.rs`
- Modify: `crates/sunset-web-wasm/src/lib.rs`
- Modify: `crates/sunset-web-wasm/src/client.rs`

- [ ] **Step 1: Add deps to `crates/sunset-web-wasm/Cargo.toml`**

In the `[dependencies]` section, append:

```toml
sunset-relay-resolver.workspace = true
```

`web-sys` is already a workspace dep but the `Request`, `RequestInit`, `Response` features are needed for fetch. Open `Cargo.toml` (workspace root) and confirm these feature names are in the `web-sys` features list. If they aren't, add them. The current list ends with the WebRTC features — add this group above the closing `]`:

```toml
  "Request",
  "RequestInit",
  "Response",
  "Window",
```

(`Window` is the type returned by `web_sys::window()`; check if already present and skip if so.)

Run: `nix develop --command cargo check -p sunset-web-wasm --target wasm32-unknown-unknown`
Expected: builds clean.

- [ ] **Step 2: Create `crates/sunset-web-wasm/src/resolver_adapter.rs`**

```rust
//! `web-sys::fetch`-backed [`HttpFetch`] for the browser. Mirrors the
//! native `ReqwestFetch` adapter; both implement the same trait so
//! the resolver crate stays platform-neutral.

use async_trait::async_trait;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::JsFuture;
use web_sys::{Request, RequestInit, Response};

use sunset_relay_resolver::{Error, HttpFetch, Result};

pub struct WebSysFetch;

#[async_trait(?Send)]
impl HttpFetch for WebSysFetch {
    async fn get(&self, url: &str) -> Result<String> {
        let opts = RequestInit::new();
        opts.set_method("GET");
        let req = Request::new_with_str_and_init(url, &opts)
            .map_err(|e| Error::Http(format!("Request::new: {e:?}")))?;
        let window = web_sys::window().ok_or_else(|| Error::Http("no window".into()))?;
        let resp_value = JsFuture::from(window.fetch_with_request(&req))
            .await
            .map_err(|e| Error::Http(format!("fetch: {e:?}")))?;
        let resp: Response = resp_value
            .dyn_into()
            .map_err(|_| Error::Http("not a Response".into()))?;
        if !resp.ok() {
            return Err(Error::Http(format!("status {}", resp.status())));
        }
        let text_promise = resp
            .text()
            .map_err(|e| Error::Http(format!("text(): {e:?}")))?;
        let text = JsFuture::from(text_promise)
            .await
            .map_err(|e| Error::Http(format!("await text: {e:?}")))?;
        text.as_string()
            .ok_or_else(|| Error::Http("body not a string".into()))
    }
}
```

- [ ] **Step 3: Wire the module into `crates/sunset-web-wasm/src/lib.rs`**

Open `crates/sunset-web-wasm/src/lib.rs`, append `pub(crate) mod resolver_adapter;` next to the other `mod` declarations.

Run: `nix develop --command cargo build -p sunset-web-wasm --target wasm32-unknown-unknown`
Expected: builds clean.

- [ ] **Step 4: Resolve in `Client::add_relay`**

Open `crates/sunset-web-wasm/src/client.rs`. Replace the body of `pub async fn add_relay` (currently lines 124-137) with:

```rust
pub async fn add_relay(&self, url_with_fragment: String) -> Result<(), JsError> {
    *self.relay_status.borrow_mut() = "connecting".to_owned();

    // Resolve user input (bare host, host:port, wss://, or fully
    // canonical wss://host#x25519=hex). Canonical forms short-circuit;
    // others fetch GET / from the relay to learn its x25519 key.
    let resolver = sunset_relay_resolver::Resolver::new(crate::resolver_adapter::WebSysFetch);
    let canonical = match resolver.resolve(&url_with_fragment).await {
        Ok(s) => s,
        Err(e) => {
            *self.relay_status.borrow_mut() = "error".to_owned();
            return Err(JsError::new(&format!("add_relay resolve: {e}")));
        }
    };

    let addr = sunset_sync::PeerAddr::new(Bytes::from(canonical));
    match self.engine.add_peer(addr).await {
        Ok(()) => {
            *self.relay_status.borrow_mut() = "connected".to_owned();
            Ok(())
        }
        Err(e) => {
            *self.relay_status.borrow_mut() = "error".to_owned();
            Err(JsError::new(&format!("add_relay: {e}")))
        }
    }
}
```

- [ ] **Step 5: Build the wasm bundle**

Run: `nix develop --command cargo build -p sunset-web-wasm --target wasm32-unknown-unknown`
Expected: builds clean.

Run: `nix develop --command cargo build --workspace --all-features`
Expected: native workspace builds clean.

- [ ] **Step 6: Run the full workspace test suite**

Run: `nix develop --command cargo test --workspace --all-features`
Expected: all tests pass — the new resolver tests, the integration test, and every existing test.

- [ ] **Step 7: Run clippy and fmt**

Run: `nix develop --command cargo clippy --workspace --all-features --all-targets -- -D warnings`
Expected: no warnings.

Run: `nix develop --command cargo fmt --all --check`
Expected: no diff in any file we touched. (Pre-existing fmt drift in unrelated crates is out of scope for this plan.)

- [ ] **Step 8: Commit**

```bash
git add crates/sunset-web-wasm/Cargo.toml crates/sunset-web-wasm/src/resolver_adapter.rs crates/sunset-web-wasm/src/lib.rs crates/sunset-web-wasm/src/client.rs Cargo.toml
git commit -m "sunset-web-wasm: resolve add_relay input via GET / before dialing"
```

---

## Self-review

**Spec coverage:** Spec sections map to tasks as follows:
- "Architecture" + "New crate `sunset-relay-resolver`" → Tasks 1–4.
- "Consumers / Native" → Task 5 (adapter + integration + integration test).
- "Consumers / Browser" → Task 6 (adapter + integration).
- "Wire-up details" (resolver runs once per `add_peer`, hostname not stored) → covered by Task 5/6 (no caching state introduced).
- "Error handling" table → covered by `Error` variants in Task 1 and surfaced through the `JsError`/`tracing::warn` paths in Tasks 5/6.
- "Testing" → Tasks 2/3 (unit), Task 4 (resolver with fake), Task 5 (real-relay integration).
- "Backwards compatibility" → covered by `Canonical` short-circuit in Task 2 and the unchanged `multi_relay.rs` tests in Task 5 step 7.

**Type consistency:** `ParsedInput`, `LookupTarget`, `HttpFetch`, `Resolver`, `Error`, `Result` are spelled identically across all tasks. `extract_x25519_from_json`, `parse_input`, `Resolver::new`, `Resolver::resolve`, `HttpFetch::get` signatures match. Both adapters return the same `Result<String>` shape.

**Placeholder scan:** No "TBD"/"TODO"/"similar to Task N" patterns. All code blocks contain complete code; all commands have expected output.
