# sunset-webtransport — implementation plan

Date: 2026-05-04 · Author: Claude Opus 4.7 · Spec: `docs/superpowers/specs/2026-05-04-sunset-webtransport-design.md`

## Goal

Browser → relay communication uses WebTransport as the primary transport, falls back to WebSocket on failure. Reliable + unreliable channels both work end-to-end. Comprehensive Playwright e2e test asserting the WT path. CI green.

## Out of scope (this PR)

- Voice routing through relay (wire path only — application layer is follow-up).
- Cross-relay federation over WT (the dial path works; multi-relay e2e is follow-up).
- Surfacing WT-vs-WS in the connected-peers UI pill.

## Sequencing

Each step ends with `cargo test --workspace --all-features` + `cargo clippy --workspace --all-features --all-targets -- -D warnings` + `cargo fmt --all --check` clean. Browser-only steps additionally run their wasm tests.

### Step 1 — Cert generation utility (relay-internal)

**Files:** `crates/sunset-relay/src/cert.rs` + `Cargo.toml` deps.

**Adds:** `rcgen` workspace dep. `cert::WtCert::load_or_generate(data_dir)` returns `(rustls_cert_chain, rustls_signing_key, sha256_spki_hex)`. Cert is ECDSA-P256, 13-day validity, SAN includes hostnames passed in. Cert + key persisted under `<data_dir>/wt-cert.pem` and `wt-key.pem`. If existing files have <24 h remaining, regenerate. Mode 0600 on the key file.

**Tests:** Generated cert SPKI matches reported hash. SAN entries present. Re-load from disk works. <24h-remaining triggers regen. Round-trip a TLS handshake against the cert in-memory using rustls.

### Step 2 — `sunset-sync-webtransport-native` skeleton + reliable channel

**Files:** new crate `crates/sunset-sync-webtransport-native/{Cargo.toml,src/lib.rs}`.

**Adds:** `wtransport` workspace dep. `WebTransportRawTransport` (`dial_only()` + `serving()`), `WebTransportRawConnection`. Reliable channel uses one persistent bidirectional QUIC stream with 4-byte big-endian length prefix. `send_unreliable`/`recv_unreliable` are stubbed (return `Err("not implemented yet")`).

**Tests:** Round-trip with `wtransport`-server bound to `127.0.0.1:0`, length-prefix framing handles small + large messages.

### Step 3 — Datagram (unreliable) channel for native

**Adds to:** `sunset-sync-webtransport-native`.

**Adds:** `send_unreliable` writes a single QUIC datagram; `recv_unreliable` reads one. Hard-cap at 1200 bytes per datagram (returns `Err` if larger). Lossy semantics: caller drops `Err` returns silently per `RawConnection` docs.

**Tests:** Round-trip a datagram. Oversized datagram returns `Err`. Lost datagrams (configured loss in test fixture not feasible — instead: assert datagram delivered when the session is healthy).

### Step 4 — Wire WT into the relay

**Files:** `crates/sunset-relay/src/{relay.rs,config.rs,render.rs}`.

**Adds:**
- New `Config` field `webtransport: WebTransportMode { Auto | Off | CustomCert(...) }`. Default = Auto.
- `Relay::start` binds `wtransport::Endpoint::server` on the **same** `listen_addr` (UDP), generates / loads cert via `cert.rs`, and spawns an accept loop that pushes sessions into a serving `WebTransportRawTransport`.
- A second `SpawningAcceptor` runs Noise IK over inbound WT sessions.
- The engine sees a *single* combined inbound transport that races WS-acceptor and WT-acceptor (a tiny new `RaceAccept<A, B>` adapter — does for `accept` what the existing `MultiTransport::accept` does for two outbound transports, but for raw inbound).
- `IdentitySnapshot` gains `webtransport_address: Option<String>`. `render_identity` emits the new JSON field.
- Startup banner gains a "wt: …" line with the WT URL.

**Tests:**
- `tests/dual_listener.rs`: start the relay, dial reliable+unreliable via WT native client, assert chat message + datagram round-trip.
- `render_identity` test updated to assert the new JSON shape (with and without WT enabled).
- Existing tests stay green.

### Step 5 — `FallbackTransport<P, F>` adapter

**Files:** `crates/sunset-sync/src/fallback_transport.rs` + `lib.rs` re-export.

**Adds:** Generic `Transport` adapter. `connect`: try `P` with bounded deadline (3 s default, configurable); on any error, rewrite the URL scheme (`wt://`→`ws://`, `wts://`→`wss://`) and try `F`. `accept`: forwards to `P` only (relays initiate, not accept, on the fallback). `MultiConnection`-style enum for the connection type.

**Tests:** Primary-success returns Primary. Primary-fail-secondary-success returns Secondary. Both-fail surfaces primary error. Deadline-exceeded counts as primary fail. Unknown URL scheme errors out cleanly.

### Step 6 — `sunset-sync-webtransport-browser` (WASM client)

**Files:** new crate `crates/sunset-sync-webtransport-browser/{Cargo.toml,src/lib.rs,src/wasm.rs,src/stub.rs}`.

**Adds:**
- WASM-only `WebTransportRawTransport::dial_only()`. Native `stub.rs` mirrors `sunset-sync-ws-browser`.
- `WebTransportRawConnection` holds `web_sys::WebTransport` + bidi stream reader/writer + datagram reader/writer.
- Cert pinning: parses `cert-sha256=<hex>` from URL fragment, passes as `serverCertificateHashes`.
- Length-prefix reliable channel; one-datagram-per-message unreliable.
- `web-sys` features added: `WebTransport`, `WebTransportOptions`, `WebTransportHash`, `WebTransportBidirectionalStream`, `WebTransportDatagramDuplexStream`, `ReadableStreamDefaultReader`, `WritableStreamDefaultWriter`, `ReadableStream`, `WritableStream`.

**Tests:** WASM tests skipped (no Chromium in cargo wasm tests); covered by Playwright e2e.

### Step 7 — Browser resolver + Client wiring

**Files:** `crates/sunset-relay-resolver/src/{json.rs,resolver.rs}`, `crates/sunset-web-wasm/src/{client.rs,resolver_adapter.rs}`.

**Adds:**
- `extract_webtransport_address_from_json` — returns `Option<String>` for the new field.
- `Resolver::resolve` retains existing single-address signature for backward compat. New method `resolve_dual` returns `(primary_url, fallback_url_opt)`. Primary is `wt`/`wts` URL when available, else falls back to ws.
- `Client::new` builds the primary half as `FallbackTransport<NoiseTransport<WT>, NoiseTransport<WS>>` instead of bare WS.
- `Client::add_relay` uses `resolve_dual`; passes the dual URL through `Connectable`.
- New `Connectable::ResolvingDual` variant or a simpler approach: keep `Connectable::Resolving` returning a *single* string but in a `wt://...|fallback=ws://...` joined form that `FallbackTransport` parses. Decision in this step (cleaner if not joined: extend `Connectable`).

**Tests:** Resolver test for new JSON shape. (`Client::new` + supervisor wiring is exercised by Playwright.)

### Step 8 — Test hook for transport-kind verification

**Files:** `crates/sunset-web-wasm/src/client.rs`, plumbed through `sunset-sync` if needed.

**Adds:** Behind `feature = "test-hooks"`: `Client::transport_kinds()` → `Vec<{intent_id, kind}>` where `kind ∈ { "webtransport", "websocket", "webrtc" }`. Implementation reads each connected peer's `TransportKind` plus a new sub-discriminator on the Primary half (added in `FallbackTransport::Connection::kind_label`).

**Tests:** Covered by Playwright assertions.

### Step 9 — Playwright e2e

**Files:** `web/e2e/webtransport_relay.spec.js`, `web/e2e/webtransport_fallback.spec.js`.

**Adds:**
- Test fixture spawns `sunset-relay` with WT enabled.
- Reads the identity descriptor, parses `webtransport_address`.
- Browser connects, asserts `transport_kinds()` reports `"webtransport"` for the relay intent within 5 s.
- Sends a chat message, asserts round-trip.
- Sends an unreliable datagram (via Bus ephemeral hook — there's a test hook for this already in `sunset-voice` style). Assert receipt.
- **Fallback test:** Spawn relay with `webtransport = "off"`. Browser tries WT (no `webtransport_address` in descriptor → skip WT entirely → use WS). Chat round-trip works.
- **Cert mismatch fallback:** Spawn relay normally; modify the descriptor's cert hash before passing it to the client (via test hook on Client). Browser tries WT, fails, falls back to WS. Chat works.

### Step 10 — flake updates

**Files:** `flake.nix`, `Cargo.toml`.

**Adds:** Flake might need `pkg-config`, `protobuf` (for some quinn deps?), or nothing extra. Verify by `nix develop --command cargo build --workspace`. The WT-related crates need to be added to the workspace `members` list. The WASM bundle's feature set may need to expose new web-sys flags — verify.

### Step 11 — PR + review loop

`gh pr create` once steps 1–10 are green locally. Address /review feedback in subsequent commits. Final acceptance: all GitHub checks green.

## Verification gate at the end of each step

```
nix develop --command cargo fmt --all --check
nix develop --command cargo clippy --workspace --all-features --all-targets -- -D warnings
nix develop --command cargo test --workspace --all-features
```

After step 9, also:
```
nix run .#web-test -- --grep webtransport
```

## Open questions to resolve during implementation

- **`wtransport` API drift.** First native crate task needs a quick `cargo add wtransport` + smoke probe. If the API has changed enough that our shape needs adjustment, revisit Step 2 inline.
- **Single-port UDP+TCP.** Some platforms might object to binding the same port for both. If `wtransport`'s endpoint binds to `0.0.0.0:8443/UDP` while axum already has `0.0.0.0:8443/TCP`, this should work — but if not, the relay defaults to WT on `listen_addr.port + 1` and that's fine. Decide in Step 4.
- **`Connectable::Resolving` extension vs. encoded URL.** I'll prototype both in Step 7 and pick the cleaner one.
