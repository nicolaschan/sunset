# Hostname-only relay dialing — design

## Goal

Let users dial a relay by typing just `relay.sunset.chat` (or `relay.sunset.chat:8443`) instead of the current verbose form `wss://relay.sunset.chat/#x25519=<64-hex>`. The Noise IK static-pubkey requirement does not change — we just learn it on the client side by hitting the relay's existing `GET /` JSON endpoint before the handshake.

## Non-goals

- DNS-record-based discovery (`_sunset._tcp` SRV) — separate spec if we want it.
- TOFU pinning of `(hostname → x25519)` on the client. Not needed for v1: Noise IK already fails closed if the relay presents a different static key than the one we resolved.
- Retry / backoff policy on resolution failure. Caller can re-call.
- Graceful handling of relays that don't yet serve the `GET /` JSON. The endpoint is on `master`; relays older than that won't be dialable by hostname.

## Architecture

```
user input ("relay.sunset.chat[:port]" or "wss://host/#x25519=hex")
        │
        ▼
sunset-relay-resolver::Resolver::resolve()
        ├── parse_input() — pure, branches:
        │     • already canonical (has #x25519=…) → return as-is
        │     • host[:port] / wss://… without fragment → ResolveTarget
        ▼
fetch GET /<scheme>://host[:port]/  via HttpFetch trait
        ▼
extract_x25519_from_json() — pure scan over the response body
        ▼
canonical PeerAddr string: "wss://host[:port]#x25519=<hex>"
                           ("ws://" for loopback)
```

The resolver is a thin bridge between user input and the existing `PeerAddr` contract. Once it returns, `sunset-noise::parse_addr_x25519` and the underlying WS transports work exactly as today.

## New crate: `sunset-relay-resolver`

A new workspace crate with no HTTP implementation. Ships:

- `parse_input(&str) -> ParsedInput` — pure. `ParsedInput` is one of `Canonical(String)` (input already had `#x25519=…`, pass through) or `Lookup(LookupTarget)` (needs HTTP).
- `LookupTarget { http_url: String, ws_url: String }` — pre-computed strings so the resolver doesn't re-derive them. Loopback heuristic (`127.0.0.1`, `::1`, `localhost`) chooses `http`/`ws`; everything else `https`/`wss`.
- `extract_x25519_from_json(&str) -> Result<[u8; 32]>` — pure. Looks for the `"x25519":"<64 hex>"` pair, hex-decodes, returns `[u8; 32]`. Tolerant of whitespace and field ordering; rejects anything that isn't 64 lowercase hex chars.
- `trait HttpFetch { async fn get(&self, url: &str) -> Result<String>; }` — one method, returns the body as a string.
- `struct Resolver<F: HttpFetch>` with `pub async fn resolve(&self, input: &str) -> Result<String>` returning the canonical PeerAddr string.
- `Error` enum: `MalformedInput(String)`, `Http(String)`, `BadJson(String)`, `BadX25519(String)`.

**WASM-compat:** the crate itself has no native-only deps. `HttpFetch` is `#[async_trait(?Send)]` to match the codebase's WASM convention.

**Cargo.toml:** depends on `hex`, `thiserror`, `async-trait`. No `serde_json` (the JSON shape is fixed and tiny).

## Consumers

### Native: `sunset-relay`

`sunset-relay` adds:

- `reqwest = { default-features = false, features = ["rustls-tls"] }` — brand-new workspace dep. rustls (not native-tls) so we don't grow an OpenSSL system dependency, per the CLAUDE.md hermeticity rule. Add to `[workspace.dependencies]` in the root `Cargo.toml`.
- A `ReqwestFetch` adapter (~20 lines) implementing `HttpFetch`.
- In `Relay::new`, before calling `engine.add_peer(addr)` for each `peers[i]` from config, run it through `Resolver::resolve`. Both already-canonical and bare-hostname inputs work.
- The `[peers]` entries in `relay.toml` continue to accept the old explicit form. Bare hostnames are now also valid; the relay resolves them at startup.

### Browser: `sunset-web-wasm`

`sunset-web-wasm` adds:

- A `WebSysFetch` adapter implementing `HttpFetch` via `web_sys::window().fetch_with_str()` + `Response::text()`. We already pull in `web-sys`, `wasm-bindgen-futures`, etc.; no new deps.
- The `add_relay(url)` JS API resolves first, then calls `engine.add_peer(canonical_addr)`. Failures surface to the JS callback as the same error type already used for connect failures (string).

## Wire-up details

- The Gleam UI does not change. Users were already typing freeform strings into the relay input; the bridge layer is where we accept the new form.
- The resolver runs once per `add_peer`. The hostname is not stored separately; once resolved, the canonical PeerAddr is the source of truth.
- The relay's startup banner (already showing `identity:  http://<bound>/`) needs no change.

## Error handling

All resolver errors flow through the existing transport-error path:

| Resolver error | Symptom | Caller surface |
|---|---|---|
| `MalformedInput` | input didn't parse as `host[:port]` or URL | `add_peer` rejects synchronously |
| `Http` (non-200, network failure, TLS error) | relay unreachable / wrong cert / wrong port | "couldn't reach relay" string |
| `BadJson` (no `x25519` field, malformed) | not a sunset relay (or older than master) | "not a recognised relay" string |
| `BadX25519` (not 64 hex chars) | corrupted / spoofed response | same |

If the resolved x25519 is wrong (active MITM on plain http for a loopback dev setup, or some misconfiguration), the subsequent Noise IK handshake will fail when the relay's actual static key doesn't match. Handshake error is already plumbed through, so the user sees a connection failure. No silent fallback.

## Testing

**Unit tests in `sunset-relay-resolver`:**

- `parse_input` for every shape in scope (B): `"relay.sunset.chat"`, `"relay.sunset.chat:8443"`, `"wss://relay.sunset.chat/#x25519=…"` (canonical short-circuit), and rejection cases (empty string, `ftp://…`, embedded path, etc.).
- Loopback heuristic: `127.0.0.1`, `::1`, `localhost`, `[::1]:8443` all yield `http`/`ws`. Everything else yields `https`/`wss`.
- `extract_x25519_from_json` against the known good shape, against arbitrary whitespace, against an extra trailing field, against a too-short hex value, against missing field.
- `Resolver::resolve` against a fake `HttpFetch` that returns a canned good body, an HTTP error, malformed JSON.

**Integration test in `sunset-relay`:** boot a real relay (existing `relay_config` helper), construct a `Resolver` with `ReqwestFetch`, resolve `127.0.0.1:<bound_port>`, assert the output canonical PeerAddr matches what `relay.dial_address()` returns. This validates the full HTTP path end-to-end against the real `GET /` handler.

**Integration test in browser:** out of scope for this plan — the existing Playwright e2e harness boots a relay and exercises the chat path; once the bridge is wired, those tests automatically cover the resolver because we'll switch them to bare-hostname URLs.

**Backwards compatibility:** the existing `multi_relay.rs` tests use canonical PeerAddrs. They keep passing because canonical inputs short-circuit the resolver. No change needed.

## Out-of-scope follow-ups

- Resolver result cache (today every `add_peer` re-fetches). Add when a measured cost justifies it.
- A CLI flag on `sunset-relay` for "verify identity but don't pin" — useful for ops but separable.
- Surfacing the relay's ed25519 in the UI (we now know it post-resolve; could be shown next to "connected" badge).
