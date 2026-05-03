# Relay axum + concurrent handshakes — implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the relay's hand-rolled byte-peeking HTTP/WS dispatcher with axum, and make inbound handshakes run concurrently — each new connection runs its post-upgrade work (today: Noise IK responder) on its own `spawn_local`'d task so slow clients can't block fast ones.

**Architecture:** Axum handles HTTP/WS routing; its built-in per-request task model gives the WS-upgrade stage concurrency for free. A new generic `SpawningAcceptor<R, T, F, …>` in `sunset-sync` wraps a `RawTransport` + a connector `Transport` + a "promote" callback (`Fn(RawConnection) -> Future<Connection>`); on each `raw.accept()` it `spawn_local`s the promote and pushes successes to a channel that the engine drains via `Transport::accept()`. The relay glues `WebSocketRawTransport::serving()` (new axum-fed mode) + `SpawningAcceptor` + `do_handshake_responder` together. `accept_with_timeout` and `SyncConfig::accept_handshake_timeout` are deleted — the timeout migrates into the per-task promote future. The engine's `?Send` / WASM-friendly internals stay untouched: axum tasks run via `tokio::spawn` (Send) on the same runtime, engine work via `tokio::task::spawn_local` (?Send) on a `LocalSet`.

**Tech Stack:** Rust workspace (edition 2024), `tokio` (sync, time, rt; `rt-multi-thread` for binary), `axum` 0.7 (added in this plan), `tokio-tungstenite` (existing), `snow` (existing), `nix develop` for hermetic builds per `CLAUDE.md`.

**Implementation note on runtime topology.** The spec describes the design as "two runtimes." This plan implements the equivalent using a *single* tokio runtime with a `LocalSet`: axum's per-request tasks run via `tokio::spawn` (Send-bound, multi-threaded on the binary), engine + per-peer + acceptor-pump tasks run via `tokio::task::spawn_local` on a `LocalSet` pinned to one thread. The substance of the spec (axum on multi-thread, engine on single-thread, bridge via Send mpsc channels) is preserved; we just don't need a second `tokio::runtime::Runtime` value to express it. The future "drop the engine's single-threaded restriction" change still works the same way: flip `spawn_local` → `spawn`, `Rc` → `Arc`, and the engine starts using the multi-thread workers axum already uses.

---

## File map (what gets created / modified / deleted)

### `sunset-sync-ws-native`

- **Modify:** `Cargo.toml` — add optional `axum` feature + dep.
- **Modify:** `src/lib.rs` — add `WebSocketRawConnection::Axum` variant (gated), add `WebSocketRawTransport::serving()` constructor, add the axum WS handler; later delete `listening_on`, `external_streams`, `TransportMode`, the existing internal integration test.
- **Modify:** `tests/two_peer_ws_noise.rs` — switch the listener side from `listening_on` to a real in-process axum server.

### `sunset-sync`

- **Create:** `src/spawning_acceptor.rs` — the new generic spawn-per-connection helper.
- **Modify:** `src/lib.rs` — export `SpawningAcceptor`.
- **Modify:** `src/engine.rs` — delete `accept_with_timeout`; the run-loop's accept arm becomes a plain `transport.accept()` await.
- **Modify:** `src/types.rs` — delete `SyncConfig::accept_handshake_timeout` field + default.

### `sunset-relay`

- **Create:** `src/bridge.rs` — `RelayCommand` enum, `DashboardSnapshot`/`IdentitySnapshot` Send POD types.
- **Create:** `src/snapshot.rs` — `build_dashboard_snapshot`/`build_identity_snapshot` (engine-side; takes `Rc<Engine>` + `Arc<FsStore>` etc. and produces a Send POD).
- **Create:** `src/render.rs` — `render_dashboard_html`/`render_identity_json` (axum-side; pure functions over snapshots).
- **Create:** `src/app.rs` — `build_app(state: AppState) -> axum::Router` plus the `dashboard_handler` / `root_handler` axum handlers.
- **Modify:** `src/config.rs` — add `accept_handshake_timeout_secs: u64` field (default 15) so the concurrent-handshakes test can set a short value via TOML.
- **Modify:** `src/relay.rs` — replace listener+dispatch+old run flow with the axum + SpawningAcceptor setup. `Relay::new` now also seeds the bridge channels and the command pump; `RelayHandle::run` runs axum on the current runtime and the engine on a `LocalSet`. `run_for_test` mirrors that shape but doesn't await OS signals.
- **Modify:** `src/main.rs` — switch the runtime to `new_multi_thread().enable_all()` and `block_on(local.run_until(…))`.
- **Modify:** `src/lib.rs` — module re-exports (drop `router`, `status`; add `app`, `bridge`, `snapshot`, `render`).
- **Modify:** `tests/http_index.rs` — keep the same TCP-level assertions; they exercise axum's responses now.
- **Create:** `tests/relay_concurrent_handshakes.rs` — the new regression test that proves slow handshakes don't serialize.
- **Delete:** `src/router.rs` (after cut-over).
- **Delete:** `src/status.rs` (after cut-over; logic moves to `snapshot.rs` + `render.rs`).
- **Modify:** `Cargo.toml` — add `axum` dep + the `sunset-sync-ws-native` feature `axum` flag.

### Workspace

- **Modify:** `Cargo.toml` — add `axum = "0.7"` (or whatever current major) under `[workspace.dependencies]`. No `flake.nix` change needed; axum is a pure-Rust crate without C deps.

---

## Task 1: Add axum to workspace dependencies

**Files:**
- Modify: `Cargo.toml`

- [ ] **Step 1: Add the workspace dep**

Open `Cargo.toml`. In the `[workspace.dependencies]` block, add:

```toml
axum = { version = "0.7", default-features = false, features = ["http1", "tokio", "ws"] }
```

Place it alphabetically (between `async-trait` and `async-stream` is fine; ordering isn't strictly enforced).

The minimal feature set here is what axum needs to host an HTTP/1.1 server with WebSocket support on tokio. We don't need `http2`, `json` (we hand-format identity JSON today), `query`, etc.

- [ ] **Step 2: Verify the workspace still resolves**

Run: `nix develop --command cargo check --workspace`
Expected: compiles cleanly. axum isn't yet used by any crate — this step just verifies the dep entry is well-formed.

- [ ] **Step 3: Commit**

```bash
git add Cargo.toml Cargo.lock
git commit -m "$(cat <<'EOF'
deps: add axum 0.7 (http1+tokio+ws) to workspace

Used by sunset-sync-ws-native (behind feature) and sunset-relay.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 2: `sunset-sync-ws-native` — add `axum` feature gate

**Files:**
- Modify: `crates/sunset-sync-ws-native/Cargo.toml`
- Modify: `crates/sunset-sync-ws-native/src/lib.rs`

- [ ] **Step 1: Add the optional axum dep + feature**

Edit `crates/sunset-sync-ws-native/Cargo.toml`. Under `[dependencies]`:

```toml
axum = { workspace = true, optional = true }
```

Add a `[features]` section (the crate doesn't currently have one):

```toml
[features]
axum = ["dep:axum"]
```

- [ ] **Step 2: Add a feature-gated module stub**

In `crates/sunset-sync-ws-native/src/lib.rs`, add right after the existing `use` block:

```rust
#[cfg(feature = "axum")]
pub mod axum_integration;
```

Create the empty module file `crates/sunset-sync-ws-native/src/axum_integration.rs`:

```rust
//! axum 0.7 integration for `sunset-sync-ws-native`.
//!
//! Behind the optional `axum` feature. Provides a WebSocket upgrade
//! handler and the channel-fed `WebSocketRawTransport::serving()` mode
//! (the constructor itself stays in `lib.rs` — see below).
```

- [ ] **Step 3: Verify build (no feature)**

Run: `nix develop --command cargo check -p sunset-sync-ws-native`
Expected: compiles. The new module isn't included.

- [ ] **Step 4: Verify build (with feature)**

Run: `nix develop --command cargo check -p sunset-sync-ws-native --features axum`
Expected: compiles. The empty `axum_integration` module is included.

- [ ] **Step 5: Commit**

```bash
git add crates/sunset-sync-ws-native/Cargo.toml crates/sunset-sync-ws-native/src/lib.rs crates/sunset-sync-ws-native/src/axum_integration.rs
git commit -m "$(cat <<'EOF'
sunset-sync-ws-native: add optional axum feature gate

Empty module for now; populated in subsequent tasks. Keeps the crate
crypto-unaware and framework-optional — the feature only activates the
axum-flavored handler path.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 3: `sunset-sync-ws-native` — `WebSocketRawConnection::Axum` variant

**Files:**
- Modify: `crates/sunset-sync-ws-native/src/lib.rs`

- [ ] **Step 1: Extend the connection enum**

In `crates/sunset-sync-ws-native/src/lib.rs`, find the `WsSink` and `WsStream` enums (around lines 19 and 46). Add `Axum` variants gated by feature.

After `enum WsSink {` add a feature-gated variant:

```rust
enum WsSink {
    Client(SplitSink<WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>, Message>),
    Server(SplitSink<WebSocketStream<tokio::net::TcpStream>, Message>),
    #[cfg(feature = "axum")]
    Axum(SplitSink<axum::extract::ws::WebSocket, axum::extract::ws::Message>),
}
```

Update `WsSink::send` to handle the new variant:

```rust
impl WsSink {
    async fn send(&mut self, msg: Message) -> Result<(), tokio_tungstenite::tungstenite::Error> {
        match self {
            WsSink::Client(s) => s.send(msg).await,
            WsSink::Server(s) => s.send(msg).await,
            #[cfg(feature = "axum")]
            WsSink::Axum(s) => {
                // Translate tungstenite::Message → axum::extract::ws::Message.
                // We only ever send Binary in the data plane; close translates
                // into axum's Close. Anything else is a bug.
                let axum_msg = match msg {
                    Message::Binary(b) => axum::extract::ws::Message::Binary(b),
                    Message::Close(_) => axum::extract::ws::Message::Close(None),
                    _ => return Err(tokio_tungstenite::tungstenite::Error::Protocol(
                        tokio_tungstenite::tungstenite::error::ProtocolError::ResetWithoutClosingHandshake,
                    )),
                };
                s.send(axum_msg)
                    .await
                    .map_err(|e| tokio_tungstenite::tungstenite::Error::Io(
                        std::io::Error::other(format!("axum ws send: {e}")),
                    ))
            }
        }
    }

    async fn close(&mut self) {
        match self {
            WsSink::Client(s) => { s.close().await.ok(); }
            WsSink::Server(s) => { s.close().await.ok(); }
            #[cfg(feature = "axum")]
            WsSink::Axum(s) => { s.close().await.ok(); }
        }
    }
}
```

After `enum WsStream {` add a feature-gated variant similarly:

```rust
enum WsStream {
    Client(SplitStream<WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>>),
    Server(SplitStream<WebSocketStream<tokio::net::TcpStream>>),
    #[cfg(feature = "axum")]
    Axum(SplitStream<axum::extract::ws::WebSocket>),
}
```

For the `WsStream::next` impl, axum's `WebSocket::next()` yields `Option<Result<axum::extract::ws::Message, _>>`. We need to translate to `tokio_tungstenite::tungstenite::Message`. Replace the existing impl with:

```rust
impl WsStream {
    async fn next(&mut self) -> Option<Result<Message, tokio_tungstenite::tungstenite::Error>> {
        match self {
            WsStream::Client(s) => s.next().await,
            WsStream::Server(s) => s.next().await,
            #[cfg(feature = "axum")]
            WsStream::Axum(s) => {
                let item = s.next().await?;
                Some(item
                    .map(|m| match m {
                        axum::extract::ws::Message::Binary(b) => Message::Binary(b),
                        axum::extract::ws::Message::Text(t) => Message::Text(t),
                        axum::extract::ws::Message::Ping(b) => Message::Ping(b),
                        axum::extract::ws::Message::Pong(b) => Message::Pong(b),
                        axum::extract::ws::Message::Close(_) => Message::Close(None),
                    })
                    .map_err(|e| tokio_tungstenite::tungstenite::Error::Io(
                        std::io::Error::other(format!("axum ws recv: {e}")),
                    )))
            }
        }
    }
}
```

The translation is lossless for our needs: `recv_reliable` only acts on `Binary`/`Close`, and skips Ping/Pong with `continue`. Text would surface as an error (existing behavior under `Message::Text(_) | Message::Frame(_) => Err(...)`) — keep that.

- [ ] **Step 2: Verify with feature off**

Run: `nix develop --command cargo build -p sunset-sync-ws-native`
Expected: compiles. New variants don't exist when feature is off.

- [ ] **Step 3: Verify with feature on**

Run: `nix develop --command cargo build -p sunset-sync-ws-native --features axum`
Expected: compiles. New variants are present but not yet constructed anywhere.

- [ ] **Step 4: Verify existing tests still pass**

Run: `nix develop --command cargo test -p sunset-sync-ws-native --all-features`
Expected: existing tests (the listening_on roundtrip + two_peer_ws_noise) still pass — we haven't changed behavior, only added enum variants and match arms.

- [ ] **Step 5: Commit**

```bash
git add crates/sunset-sync-ws-native/src/lib.rs
git commit -m "$(cat <<'EOF'
sunset-sync-ws-native: add axum WebSocket variant to WsSink/WsStream

Gated behind the `axum` feature. Constructed in the next task by
WebSocketRawTransport::serving(); recv_reliable's existing Binary/Close
discipline carries over unchanged.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 4: `sunset-sync-ws-native` — `serving()` constructor + axum WS handler

**Files:**
- Modify: `crates/sunset-sync-ws-native/src/lib.rs`
- Modify: `crates/sunset-sync-ws-native/src/axum_integration.rs`

- [ ] **Step 1: Add the new TransportMode variant**

In `crates/sunset-sync-ws-native/src/lib.rs`, find `enum TransportMode { … }`. Add a new feature-gated variant for axum-fed serving:

```rust
enum TransportMode {
    DialOnly,
    Listening { listener: Mutex<TcpListener> },
    /// Accept pre-classified TcpStreams from an external dispatcher.
    ExternalStreams { rx: Mutex<tokio::sync::mpsc::Receiver<TcpStream>> },
    /// Drains a channel of *already-upgraded* axum WebSocket sockets.
    /// Populated by an upstream HTTP framework (axum) handler that did
    /// the WS upgrade. The transport is crypto-unaware; promotion to an
    /// authenticated connection happens above (e.g. sunset-noise).
    #[cfg(feature = "axum")]
    Serving { rx: Mutex<tokio::sync::mpsc::UnboundedReceiver<axum::extract::ws::WebSocket>> },
}
```

- [ ] **Step 2: Add the `serving()` constructor**

Below the existing constructors in `impl WebSocketRawTransport`, add:

```rust
/// Construct a server-side transport whose `accept()` drains a channel
/// of already-upgraded axum `WebSocket`s. Returns the transport plus a
/// `Send` sender that an HTTP framework handler uses to push upgrades.
///
/// Use the companion `axum_integration::ws_handler(tx)` to mount the
/// upgrade handler on an axum router.
#[cfg(feature = "axum")]
pub fn serving() -> (Self, tokio::sync::mpsc::UnboundedSender<axum::extract::ws::WebSocket>) {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<axum::extract::ws::WebSocket>();
    let transport = Self {
        mode: TransportMode::Serving { rx: Mutex::new(rx) },
    };
    (transport, tx)
}
```

- [ ] **Step 3: Update `local_addr` to know about the new mode**

The existing `local_addr` returns `None` for DialOnly and ExternalStreams. Treat Serving the same way — the relay (or axum) owns the actual `TcpListener`, not the transport.

```rust
pub fn local_addr(&self) -> Option<std::net::SocketAddr> {
    match &self.mode {
        TransportMode::Listening { listener } => {
            listener.try_lock().ok().and_then(|l| l.local_addr().ok())
        }
        TransportMode::DialOnly | TransportMode::ExternalStreams { .. } => None,
        #[cfg(feature = "axum")]
        TransportMode::Serving { .. } => None,
    }
}
```

- [ ] **Step 4: Update `RawTransport::accept` to drive the new mode**

In the `impl RawTransport for WebSocketRawTransport` block, the existing `accept()` matches on `self.mode`. Add the Serving arm:

```rust
async fn accept(&self) -> SyncResult<Self::Connection> {
    #[cfg(feature = "axum")]
    {
        if let TransportMode::Serving { rx } = &self.mode {
            let mut rx = rx.lock().await;
            let socket = rx
                .recv()
                .await
                .ok_or_else(|| SyncError::Transport("axum serving channel closed".into()))?;
            let (sink, stream) = futures_util::StreamExt::split(socket);
            return Ok(WebSocketRawConnection::new(
                WsSink::Axum(sink),
                WsStream::Axum(stream),
            ));
        }
    }
    let tcp = match &self.mode {
        TransportMode::Listening { listener } => {
            let listener = listener.lock().await;
            let (tcp, _peer) = listener
                .accept()
                .await
                .map_err(|e| SyncError::Transport(format!("accept: {e}")))?;
            tcp
        }
        TransportMode::ExternalStreams { rx } => {
            let mut rx = rx.lock().await;
            rx.recv()
                .await
                .ok_or_else(|| SyncError::Transport("external stream channel closed".into()))?
        }
        TransportMode::DialOnly => {
            std::future::pending::<()>().await;
            unreachable!();
        }
        #[cfg(feature = "axum")]
        TransportMode::Serving { .. } => unreachable!("handled above"),
    };
    let ws = tokio_tungstenite::accept_async(tcp)
        .await
        .map_err(|e| SyncError::Transport(format!("ws upgrade: {e}")))?;
    let (sink, stream) = ws.split();
    Ok(WebSocketRawConnection::new(
        WsSink::Server(sink),
        WsStream::Server(stream),
    ))
}
```

- [ ] **Step 5: Build the axum WS handler**

Replace the contents of `crates/sunset-sync-ws-native/src/axum_integration.rs` with:

```rust
//! axum 0.7 integration for `sunset-sync-ws-native`.
//!
//! Behind the optional `axum` feature. Provides a WebSocket upgrade
//! handler that pushes already-upgraded sockets onto the channel that
//! `WebSocketRawTransport::serving()` drains.

use axum::extract::WebSocketUpgrade;
use axum::response::Response;
use tokio::sync::mpsc::UnboundedSender;

/// Convert an inbound axum WebSocket upgrade request into an upgraded
/// socket pushed onto `tx`. Use as the body of an axum route handler:
///
/// ```ignore
/// let (raw, ws_tx) = WebSocketRawTransport::serving();
/// let app = axum::Router::new().route(
///     "/",
///     axum::routing::get(move |ws| ws_handler(ws, ws_tx.clone())),
/// );
/// ```
///
/// The returned `Response` is axum's standard 101 Switching Protocols
/// answer; the upgrade itself completes inside axum's per-request task,
/// so slow upgrades don't block other requests.
pub async fn ws_handler(
    ws: WebSocketUpgrade,
    tx: UnboundedSender<axum::extract::ws::WebSocket>,
) -> Response {
    ws.on_upgrade(move |socket| async move {
        // Best-effort send. If the receiver is gone, the relay is shutting
        // down; the upgraded socket will close on drop.
        let _ = tx.send(socket);
    })
}
```

- [ ] **Step 6: Verify compilation with feature on**

Run: `nix develop --command cargo build -p sunset-sync-ws-native --features axum`
Expected: compiles cleanly.

- [ ] **Step 7: Verify existing tests pass**

Run: `nix develop --command cargo test -p sunset-sync-ws-native --all-features`
Expected: existing tests (listening_on roundtrip + two_peer_ws_noise) pass. New code isn't yet exercised — that's task 5.

- [ ] **Step 8: Commit**

```bash
git add crates/sunset-sync-ws-native/src/lib.rs crates/sunset-sync-ws-native/src/axum_integration.rs
git commit -m "$(cat <<'EOF'
sunset-sync-ws-native: add WebSocketRawTransport::serving() + axum handler

Behind the `axum` feature: serving() returns a channel-fed transport plus
a Send sender that the axum upgrade handler pushes upgraded sockets onto.
listening_on / external_streams / TcpListener-based modes still exist
unchanged; deletion happens after the relay cuts over.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 5: Migrate `sunset-sync-ws-native` integration tests to axum

**Files:**
- Modify: `crates/sunset-sync-ws-native/src/lib.rs` (the `#[cfg(test)] mod tests` at the bottom)
- Modify: `crates/sunset-sync-ws-native/tests/two_peer_ws_noise.rs`
- Modify: `crates/sunset-sync-ws-native/Cargo.toml` (dev-dep: enable the `axum` feature for tests)

- [ ] **Step 1: Make the `axum` feature on for dev / tests**

In `crates/sunset-sync-ws-native/Cargo.toml`, under `[dev-dependencies]`, add a self-reference enabling the feature for tests:

```toml
sunset-sync-ws-native = { path = ".", features = ["axum"] }
```

This is a known idiom (a crate dev-depends on itself with extra features) so its own test files compile with the feature on without forcing all consumers to enable it.

Alternatively, if that pattern fights cargo, simply have the tests live behind `#[cfg(feature = "axum")]` and run them with `cargo test --features axum`. Pick whichever lands clean — try the dev-dep self-ref first.

- [ ] **Step 2: Rewrite the in-crate `raw_send_recv_roundtrip` test**

In `crates/sunset-sync-ws-native/src/lib.rs`, replace the existing `#[cfg(test)] mod tests { … raw_send_recv_roundtrip … }` block (it currently uses `listening_on`) with an axum-based version.

```rust
#[cfg(test)]
#[cfg(feature = "axum")]
mod tests {
    use super::*;

    #[tokio::test(flavor = "current_thread")]
    async fn raw_send_recv_roundtrip() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                // Build a server-side transport that drains a channel of
                // upgraded axum WebSockets, plus the Send sender used by
                // the axum handler.
                let (server_raw, ws_tx) = WebSocketRawTransport::serving();

                // Mount the WS handler on an axum app and bind a port.
                let app = axum::Router::new().route(
                    "/",
                    axum::routing::get({
                        let ws_tx = ws_tx.clone();
                        move |ws: axum::extract::WebSocketUpgrade| {
                            crate::axum_integration::ws_handler(ws, ws_tx.clone())
                        }
                    }),
                );
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                let bound = listener.local_addr().unwrap();
                let serve_handle = tokio::spawn(async move {
                    axum::serve(listener, app).await.unwrap();
                });

                // Server-side: accept one upgraded connection + echo one message.
                let server_handle = tokio::task::spawn_local(async move {
                    let conn = server_raw.accept().await.unwrap();
                    let msg = conn.recv_reliable().await.unwrap();
                    conn.send_reliable(msg).await.unwrap();
                });

                // Client-side: dial via dial_only + roundtrip.
                let client = WebSocketRawTransport::dial_only();
                let addr = PeerAddr::new(Bytes::from(format!("ws://{bound}")));
                let conn = client.connect(addr).await.unwrap();

                conn.send_reliable(Bytes::from_static(b"hello ws"))
                    .await
                    .unwrap();
                let echo = conn.recv_reliable().await.unwrap();
                assert_eq!(echo.as_ref(), b"hello ws");

                server_handle.await.unwrap();
                serve_handle.abort();
            })
            .await;
    }
}
```

- [ ] **Step 3: Rewrite `tests/two_peer_ws_noise.rs` listener side**

In `crates/sunset-sync-ws-native/tests/two_peer_ws_noise.rs`, replace lines 54–60 (the `listening_on` setup) with an axum-served acceptor. The change is local — find:

```rust
            // ---- bob listens on a random port ----
            let bob_raw = WebSocketRawTransport::listening_on("127.0.0.1:0".parse().unwrap())
                .await
                .unwrap();
            let bob_bound = bob_raw.local_addr().unwrap();
            let bob_noise =
                NoiseTransport::new(bob_raw, Arc::new(IdentityNoiseAdapter(bob.clone())));
```

Replace with:

```rust
            // ---- bob listens on a random port via in-process axum ----
            let (bob_raw, ws_tx) = WebSocketRawTransport::serving();
            let app = axum::Router::new().route(
                "/",
                axum::routing::get({
                    let ws_tx = ws_tx.clone();
                    move |ws: axum::extract::WebSocketUpgrade| {
                        sunset_sync_ws_native::axum_integration::ws_handler(ws, ws_tx.clone())
                    }
                }),
            );
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let bob_bound = listener.local_addr().unwrap();
            let _serve_handle = tokio::spawn(async move {
                axum::serve(listener, app).await.unwrap();
            });
            let bob_noise =
                NoiseTransport::new(bob_raw, Arc::new(IdentityNoiseAdapter(bob.clone())));
```

The rest of the test (alice dials, message exchange, decode) is unchanged — alice still uses `dial_only` and dials `bob_bound`, which is now bound by axum. The wire format is identical.

- [ ] **Step 4: Run the tests**

Run: `nix develop --command cargo test -p sunset-sync-ws-native --all-features`
Expected:
- `raw_send_recv_roundtrip` passes (now exercises axum + serving()).
- `alice_encrypts_bob_decrypts_over_ws_and_noise` passes (now uses axum on bob).

- [ ] **Step 5: Commit**

```bash
git add crates/sunset-sync-ws-native/Cargo.toml crates/sunset-sync-ws-native/src/lib.rs crates/sunset-sync-ws-native/tests/two_peer_ws_noise.rs
git commit -m "$(cat <<'EOF'
sunset-sync-ws-native: migrate listener tests to in-process axum

Both the in-crate raw_send_recv_roundtrip and the two_peer_ws_noise
integration test now stand up axum::serve in-process for the listener,
exercising the new serving() + ws_handler path. listening_on is no
longer used by anything in this crate; deletion happens later.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 6: `sunset-sync` — `SpawningAcceptor` core

**Files:**
- Create: `crates/sunset-sync/src/spawning_acceptor.rs`
- Modify: `crates/sunset-sync/src/lib.rs`

- [ ] **Step 1: Create the new module file**

Create `crates/sunset-sync/src/spawning_acceptor.rs` with:

```rust
//! `SpawningAcceptor` — a `Transport` decorator that runs each inbound
//! connection's promotion (the slow per-connection work between a raw
//! socket and a usable, authenticated connection) on its own task.
//!
//! This is the structural fix for the inbound-pipeline serialization
//! that affects engine accept loops. Without this wrapper, a single
//! slow client at any post-upgrade stage (Noise IK responder, future
//! TLS termination, anti-DoS challenge, etc.) holds the engine's accept
//! arm captive; with it, each promotion runs on its own `spawn_local`'d
//! task and successes land on a channel that `accept()` drains.
//!
//! The wrapper is generic over the promotion callback so it doesn't
//! depend on any specific cryptography. The caller wires up the
//! callback (e.g. the relay binary passes
//! `sunset_noise::do_handshake_responder`).

use std::future::Future;
use std::marker::PhantomData;
use std::rc::Rc;
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::{Mutex, mpsc};

use crate::error::{Error, Result};
use crate::spawn::{JoinHandle, spawn_local};
use crate::transport::{RawTransport, Transport, TransportConnection};
use crate::types::PeerAddr;

/// Wraps a server-side `RawTransport`, a connector `Transport` (used for
/// outbound `connect()` calls), and a "promote" callback that turns a
/// `RawConnection` into an authenticated `Connection`. On construction
/// it eagerly spawns a pump task: every successful `raw.accept()` is
/// handed to a fresh `spawn_local`'d task that runs `promote` under a
/// per-task timeout. Successes go to an internal mpsc; `Transport::accept()`
/// drains that mpsc.
///
/// The connector and the raw acceptor can — but need not — wrap the same
/// underlying machinery. In the relay's typical wiring the connector is
/// `NoiseTransport<WebSocketRawTransport::dial_only>` and the raw side is
/// `WebSocketRawTransport::serving()`.
pub struct SpawningAcceptor<R, T, F, Fut, C>
where
    R: RawTransport + 'static,
    R::Connection: 'static,
    T: Transport<Connection = C> + 'static,
    F: Fn(R::Connection) -> Fut + 'static,
    Fut: Future<Output = Result<C>> + 'static,
    C: TransportConnection + 'static,
{
    connector: Rc<T>,
    auth_rx: Mutex<mpsc::UnboundedReceiver<C>>,
    /// Held to keep the pump task alive. Aborted on drop, so SpawningAcceptor
    /// has the same lifecycle semantics as any owned task handle.
    _pump: JoinHandle<()>,
    _markers: PhantomData<(R, F, Fut)>,
}

impl<R, T, F, Fut, C> SpawningAcceptor<R, T, F, Fut, C>
where
    R: RawTransport + 'static,
    R::Connection: 'static,
    T: Transport<Connection = C> + 'static,
    F: Fn(R::Connection) -> Fut + 'static,
    Fut: Future<Output = Result<C>> + 'static,
    C: TransportConnection + 'static,
{
    /// Construct + start the pump. `handshake_timeout` bounds each
    /// individual `promote` future; on timeout the in-flight raw
    /// connection is dropped (closing its underlying socket).
    pub fn new(raw: R, connector: T, promote: F, handshake_timeout: Duration) -> Self {
        let raw = Rc::new(raw);
        let promote = Rc::new(promote);
        let (auth_tx, auth_rx) = mpsc::unbounded_channel::<C>();
        let pump = spawn_local(pump_loop(raw, promote, auth_tx, handshake_timeout));
        Self {
            connector: Rc::new(connector),
            auth_rx: Mutex::new(auth_rx),
            _pump: pump,
            _markers: PhantomData,
        }
    }
}

async fn pump_loop<R, F, Fut, C>(
    raw: Rc<R>,
    promote: Rc<F>,
    auth_tx: mpsc::UnboundedSender<C>,
    handshake_timeout: Duration,
) where
    R: RawTransport + 'static,
    R::Connection: 'static,
    F: Fn(R::Connection) -> Fut + 'static,
    Fut: Future<Output = Result<C>> + 'static,
    C: TransportConnection + 'static,
{
    loop {
        match raw.accept().await {
            Ok(rc) => {
                let auth_tx = auth_tx.clone();
                let promote = promote.clone();
                spawn_local(async move {
                    match with_timeout(handshake_timeout, promote(rc)).await {
                        Some(Ok(conn)) => {
                            // Receiver gone => acceptor dropped => discard.
                            let _ = auth_tx.send(conn);
                        }
                        Some(Err(e)) => {
                            eprintln!("sunset-sync: promote failed: {e}");
                        }
                        None => {
                            eprintln!(
                                "sunset-sync: promote timed out after {:?}; dropping",
                                handshake_timeout,
                            );
                        }
                    }
                });
            }
            Err(e) => {
                // Don't tear down the pump on a single accept error; transient
                // failures (probe with a malformed prologue, kernel ICMP, etc.)
                // are normal on the public internet.
                eprintln!("sunset-sync: raw accept failed: {e}; continuing");
            }
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
async fn with_timeout<F: Future>(d: Duration, f: F) -> Option<F::Output> {
    tokio::time::timeout(d, f).await.ok()
}

#[cfg(target_arch = "wasm32")]
async fn with_timeout<F: Future>(d: Duration, f: F) -> Option<F::Output> {
    wasmtimer::tokio::timeout(d, f).await.ok()
}

#[async_trait(?Send)]
impl<R, T, F, Fut, C> Transport for SpawningAcceptor<R, T, F, Fut, C>
where
    R: RawTransport + 'static,
    R::Connection: 'static,
    T: Transport<Connection = C> + 'static,
    F: Fn(R::Connection) -> Fut + 'static,
    Fut: Future<Output = Result<C>> + 'static,
    C: TransportConnection + 'static,
{
    type Connection = C;

    async fn connect(&self, addr: PeerAddr) -> Result<Self::Connection> {
        self.connector.connect(addr).await
    }

    async fn accept(&self) -> Result<Self::Connection> {
        let mut rx = self.auth_rx.lock().await;
        rx.recv()
            .await
            .ok_or_else(|| Error::Transport("acceptor channel closed".into()))
    }
}
```

- [ ] **Step 2: Re-export from `lib.rs`**

In `crates/sunset-sync/src/lib.rs`, add:

```rust
pub mod spawning_acceptor;
```

near the other `pub mod` declarations, and add to the re-export block:

```rust
pub use spawning_acceptor::SpawningAcceptor;
```

- [ ] **Step 3: Compile**

Run: `nix develop --command cargo build -p sunset-sync`
Expected: compiles. Warnings about unused `_pump` / `_markers` should be silenced by the leading underscore convention.

- [ ] **Step 4: Run existing tests**

Run: `nix develop --command cargo test -p sunset-sync --all-features`
Expected: passes. The new helper isn't exercised yet — that's the next task.

- [ ] **Step 5: Commit**

```bash
git add crates/sunset-sync/src/spawning_acceptor.rs crates/sunset-sync/src/lib.rs
git commit -m "$(cat <<'EOF'
sunset-sync: add SpawningAcceptor — generic per-conn task wrapper

Wraps a RawTransport + a connector Transport + a promote callback. Each
raw.accept() result is handed to its own spawn_local'd task that runs
the promote under a per-task timeout; successes land on a channel that
the engine drains via Transport::accept(). Decouples spawn-per-conn
concurrency from any specific cryptography — sunset-noise stays
unchanged; the relay binary will wire do_handshake_responder in as the
promote callback.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 7: `sunset-sync` — `SpawningAcceptor` unit tests

**Files:**
- Modify: `crates/sunset-sync/src/spawning_acceptor.rs`

The tests live inline in `#[cfg(test)] mod tests` so they don't pollute the public `test_helpers` surface. They use a tiny ad-hoc `RawTransport` stub that yields raw connections from a queue, plus a synthetic connector that no test needs to connect through.

- [ ] **Step 1: Add the test module**

Append to `crates/sunset-sync/src/spawning_acceptor.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;
    use tokio::sync::{Mutex as AsyncMutex, mpsc};

    use crate::transport::{RawConnection, TransportKind};
    use crate::types::PeerId;

    // ---- synthetic raw transport / connection ----

    /// A `RawConnection` we never read from — it just exists. Promote
    /// closures inspect a per-conn id to decide how to behave (fast,
    /// hang forever, fail).
    struct StubRawConn {
        id: usize,
    }

    #[async_trait(?Send)]
    impl RawConnection for StubRawConn {
        async fn send_reliable(&self, _: Bytes) -> Result<()> {
            Ok(())
        }
        async fn recv_reliable(&self) -> Result<Bytes> {
            std::future::pending().await
        }
        async fn send_unreliable(&self, _: Bytes) -> Result<()> {
            Ok(())
        }
        async fn recv_unreliable(&self) -> Result<Bytes> {
            std::future::pending().await
        }
        async fn close(&self) -> Result<()> {
            Ok(())
        }
    }

    /// A `RawTransport` whose `accept()` yields `StubRawConn`s from a
    /// pre-loaded queue. After the queue is drained, `accept()` blocks
    /// forever (matching real-world "no more connections coming right
    /// now" behavior).
    struct StubRawTransport {
        queue: AsyncMutex<mpsc::UnboundedReceiver<StubRawConn>>,
    }

    impl StubRawTransport {
        fn with_ids(ids: &[usize]) -> Self {
            let (tx, rx) = mpsc::unbounded_channel();
            for &id in ids {
                tx.send(StubRawConn { id }).unwrap();
            }
            Self { queue: AsyncMutex::new(rx) }
        }
    }

    #[async_trait(?Send)]
    impl RawTransport for StubRawTransport {
        type Connection = StubRawConn;
        async fn connect(&self, _: PeerAddr) -> Result<StubRawConn> {
            Err(Error::Transport("connect not used in these tests".into()))
        }
        async fn accept(&self) -> Result<StubRawConn> {
            let mut q = self.queue.lock().await;
            q.recv()
                .await
                .ok_or_else(|| Error::Transport("queue closed".into()))
        }
    }

    // ---- a `Transport` connection that the promote produces ----

    struct StubAuthConn {
        id: usize,
    }

    #[async_trait(?Send)]
    impl TransportConnection for StubAuthConn {
        async fn send_reliable(&self, _: Bytes) -> Result<()> {
            Ok(())
        }
        async fn recv_reliable(&self) -> Result<Bytes> {
            std::future::pending().await
        }
        async fn send_unreliable(&self, _: Bytes) -> Result<()> {
            Ok(())
        }
        async fn recv_unreliable(&self) -> Result<Bytes> {
            std::future::pending().await
        }
        fn peer_id(&self) -> PeerId {
            PeerId(sunset_store::VerifyingKey::new(Bytes::from(format!(
                "stub-{}",
                self.id
            ))))
        }
        fn kind(&self) -> TransportKind {
            TransportKind::Unknown
        }
        async fn close(&self) -> Result<()> {
            Ok(())
        }
    }

    /// A connector whose `connect()` is unused in these tests.
    struct UnusedConnector;

    #[async_trait(?Send)]
    impl Transport for UnusedConnector {
        type Connection = StubAuthConn;
        async fn connect(&self, _: PeerAddr) -> Result<StubAuthConn> {
            Err(Error::Transport("connector unused in these tests".into()))
        }
        async fn accept(&self) -> Result<StubAuthConn> {
            std::future::pending().await
        }
    }

    // ---- tests ----

    /// Two slow promotes never complete; the third's promote completes
    /// promptly. Acceptor.accept() must return the third without waiting
    /// on the slow ones.
    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn slow_promotes_do_not_block_a_fast_one() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let raw = StubRawTransport::with_ids(&[1, 2, 3]);
                let connector = UnusedConnector;
                let promote = move |rc: StubRawConn| async move {
                    if rc.id == 3 {
                        // Fast.
                        Ok(StubAuthConn { id: rc.id })
                    } else {
                        // Stall forever.
                        std::future::pending::<()>().await;
                        unreachable!()
                    }
                };
                let acceptor = SpawningAcceptor::new(
                    raw,
                    connector,
                    promote,
                    Duration::from_secs(60),
                );

                // The acceptor's pump fires the three accept()s as separate
                // promote tasks. Tasks 1 and 2 hang; task 3 completes.
                // accept() should return task 3's connection.
                let conn = tokio::time::timeout(
                    Duration::from_secs(5),
                    acceptor.accept(),
                )
                .await
                .expect("accept did not return within 5 s — slow promotes blocked the fast one")
                .expect("accept errored");
                assert_eq!(conn.id, 3);
            })
            .await;
    }

    /// Per-task timeout fires independently. With a 1 s timeout and three
    /// stalled promotes, all three tasks complete (drop+log) within ~1 s
    /// rather than serializing into 3 s.
    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn per_task_timeout_fires_independently() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let raw = StubRawTransport::with_ids(&[1, 2, 3]);
                let connector = UnusedConnector;
                let counter = Rc::new(RefCell::new(0usize));
                let counter_clone = counter.clone();
                let promote = move |_rc: StubRawConn| {
                    let counter_clone = counter_clone.clone();
                    async move {
                        // Stall, then the timeout will cancel us. The Drop
                        // of this future increments the counter.
                        struct Counter(Rc<RefCell<usize>>);
                        impl Drop for Counter {
                            fn drop(&mut self) {
                                *self.0.borrow_mut() += 1;
                            }
                        }
                        let _g = Counter(counter_clone);
                        std::future::pending::<()>().await;
                        Ok(StubAuthConn { id: 0 })
                    }
                };
                let _acceptor = SpawningAcceptor::new(
                    raw,
                    connector,
                    promote,
                    Duration::from_secs(1),
                );

                // Advance the paused clock past the timeout window. All three
                // tasks should hit timeout and drop their guard.
                tokio::time::advance(Duration::from_millis(1_500)).await;
                tokio::task::yield_now().await;

                // We expect 3 drops; allow up to 5 s of real-time padding for
                // the spawn-and-cancel ladder to settle (this is paused-clock
                // mode, so the real-time bound is just a safety net).
                let start = tokio::time::Instant::now();
                while *counter.borrow() < 3 && start.elapsed() < Duration::from_secs(5) {
                    tokio::task::yield_now().await;
                }
                assert_eq!(
                    *counter.borrow(),
                    3,
                    "expected 3 promote-task drops on timeout, saw {}",
                    counter.borrow(),
                );
            })
            .await;
    }
}
```

- [ ] **Step 2: Run the new tests**

Run: `nix develop --command cargo test -p sunset-sync spawning_acceptor`
Expected: both tests pass.

- [ ] **Step 3: Commit**

```bash
git add crates/sunset-sync/src/spawning_acceptor.rs
git commit -m "$(cat <<'EOF'
sunset-sync: SpawningAcceptor unit tests for the concurrency property

Two tests with synthetic RawTransport + promote callback (no Noise
dependency at the test layer):

- slow_promotes_do_not_block_a_fast_one: acceptor surfaces the third
  connection while two slow promotes are still hanging.

- per_task_timeout_fires_independently: with paused tokio time, three
  stalled promotes each hit their own timeout and drop, rather than
  serializing into one big timeout.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 8: `sunset-relay` — bridge module (commands + snapshots)

**Files:**
- Create: `crates/sunset-relay/src/bridge.rs`
- Modify: `crates/sunset-relay/src/lib.rs`

- [ ] **Step 1: Create the bridge module**

Create `crates/sunset-relay/src/bridge.rs`:

```rust
//! Send-friendly types that cross between the axum HTTP layer and the
//! engine-side LocalSet.
//!
//! axum handlers must be `Send` (axum spawns one task per request via
//! `tokio::spawn`, which has a `Send` bound). The engine, by contrast,
//! is `?Send` (it holds `Rc<…>` internally for WASM compatibility). The
//! bridge is just a small set of plain-old-data types and an mpsc-based
//! command protocol — handlers send commands, the engine-side command
//! pump answers via oneshot replies built from immediate-mode reads of
//! `Rc<Engine>` + `Arc<Store>`.

use std::net::SocketAddr;

use bytes::Bytes;
use tokio::sync::oneshot;

use sunset_store::{Filter, VerifyingKey};
use sunset_sync::PeerId;

/// One in-flight request from the axum side to the engine side.
pub enum RelayCommand {
    /// Build a fresh dashboard snapshot. Reply is the rendered POD.
    Snapshot {
        reply: oneshot::Sender<DashboardSnapshot>,
    },
    /// Build a fresh identity snapshot for the JSON `/` endpoint.
    Identity {
        reply: oneshot::Sender<IdentitySnapshot>,
    },
}

/// Send-only POD that captures everything the dashboard renderer needs.
/// Built on the engine-side; rendered (HTML) on the axum side.
#[derive(Clone, Debug)]
pub struct DashboardSnapshot {
    pub ed25519_public: [u8; 32],
    pub x25519_public: [u8; 32],
    pub listen_addr: SocketAddr,
    pub dial_url: String,

    pub configured_peers: Vec<String>,
    pub connected_peers: Vec<PeerId>,

    pub subscriptions: Vec<(PeerId, Filter)>,

    pub data_dir: std::path::PathBuf,
    pub on_disk_size: u64,
    pub store_stats: StoreStats,
}

/// Subset of `DashboardSnapshot` that's used for the JSON `/` route.
/// Kept separate so the JSON handler can answer with a smaller round-trip
/// to the engine.
#[derive(Clone, Debug)]
pub struct IdentitySnapshot {
    pub ed25519_public: [u8; 32],
    pub x25519_public: [u8; 32],
    pub dial_url: String,
}

#[derive(Clone, Debug, Default)]
pub struct StoreStats {
    pub entry_count: u64,
    pub entries_with_ttl: u64,
    pub entries_without_ttl: u64,
    pub subscription_entries: u64,
    pub cursor: Option<u64>,
    pub soonest_expiry: Option<EntryTtl>,
    pub latest_expiry: Option<EntryTtl>,
}

#[derive(Clone, Debug)]
pub struct EntryTtl {
    pub expires_at: u64,
    pub vk: VerifyingKey,
    pub name: Bytes,
}
```

- [ ] **Step 2: Wire the module into the crate**

In `crates/sunset-relay/src/lib.rs`, add (alphabetical with existing modules):

```rust
pub mod bridge;
```

- [ ] **Step 3: Build**

Run: `nix develop --command cargo build -p sunset-relay`
Expected: compiles. Nothing uses `bridge` yet.

- [ ] **Step 4: Commit**

```bash
git add crates/sunset-relay/src/bridge.rs crates/sunset-relay/src/lib.rs
git commit -m "$(cat <<'EOF'
sunset-relay: add bridge module — commands + Send snapshot types

RelayCommand enum + DashboardSnapshot / IdentitySnapshot / StoreStats
PODs that cross between axum (Send) and the engine LocalSet (?Send).
No use sites yet; subsequent tasks add the engine-side builder, the
axum-side renderer, and the handlers.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 9: `sunset-relay` — engine-side snapshot builder

**Files:**
- Create: `crates/sunset-relay/src/snapshot.rs`
- Modify: `crates/sunset-relay/src/lib.rs`

- [ ] **Step 1: Create the snapshot module**

Create `crates/sunset-relay/src/snapshot.rs`:

```rust
//! Engine-side snapshot construction for the dashboard / identity routes.
//!
//! Reads `Rc<SyncEngine>` + `Arc<FsStore>` and produces `Send` PODs
//! (`DashboardSnapshot` / `IdentitySnapshot`). Runs inside the LocalSet
//! command pump; never crosses runtimes itself.

use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;

use bytes::Bytes;
use futures::StreamExt;
use sunset_store::{Filter, Store};
use sunset_store_fs::FsStore;
use sunset_sync::SyncEngine;
use sunset_noise::NoiseTransport;
use sunset_sync_ws_native::WebSocketRawTransport;

use crate::bridge::{DashboardSnapshot, EntryTtl, IdentitySnapshot, StoreStats};

/// The concrete `SyncEngine` type the relay holds. The `SpawningAcceptor`
/// wrapping is a private detail of `relay.rs` — for snapshot purposes we
/// only need the engine APIs (`connected_peers`, `subscriptions_snapshot`)
/// which are independent of the wrapping transport. We type-erase via
/// generics in the function signature so the snapshot builder doesn't
/// need to know the wrapper's full type.
pub async fn build_dashboard_snapshot<T>(
    engine: &Rc<SyncEngine<FsStore, T>>,
    store: &Arc<FsStore>,
    data_dir: &Path,
    ed25519_public: [u8; 32],
    x25519_public: [u8; 32],
    listen_addr: std::net::SocketAddr,
    dial_url: &str,
    configured_peers: &[String],
) -> DashboardSnapshot
where
    T: sunset_sync::Transport + 'static,
    T::Connection: 'static,
{
    let connected_peers = engine.connected_peers().await;
    let subscriptions = engine.subscriptions_snapshot().await;
    let store_stats = collect_store_stats(&**store).await;
    let on_disk_size = dir_size(data_dir).unwrap_or(0);

    DashboardSnapshot {
        ed25519_public,
        x25519_public,
        listen_addr,
        dial_url: dial_url.to_owned(),
        configured_peers: configured_peers.to_vec(),
        connected_peers,
        subscriptions,
        data_dir: data_dir.to_path_buf(),
        on_disk_size,
        store_stats,
    }
}

pub fn build_identity_snapshot(
    ed25519_public: [u8; 32],
    x25519_public: [u8; 32],
    dial_url: &str,
) -> IdentitySnapshot {
    IdentitySnapshot {
        ed25519_public,
        x25519_public,
        dial_url: dial_url.to_owned(),
    }
}

async fn collect_store_stats<S: Store>(store: &S) -> StoreStats {
    let mut stats = StoreStats::default();
    if let Ok(c) = store.current_cursor().await {
        stats.cursor = Some(c.0);
    }
    let mut iter = match store.iter(Filter::NamePrefix(Bytes::new())).await {
        Ok(s) => s,
        Err(_) => return stats,
    };
    while let Some(item) = iter.next().await {
        let entry = match item {
            Ok(e) => e,
            Err(_) => continue,
        };
        stats.entry_count += 1;
        if entry.name.as_ref() == sunset_sync::reserved::SUBSCRIBE_NAME {
            stats.subscription_entries += 1;
        }
        match entry.expires_at {
            None => stats.entries_without_ttl += 1,
            Some(t) => {
                stats.entries_with_ttl += 1;
                let candidate = EntryTtl {
                    expires_at: t,
                    vk: entry.verifying_key.clone(),
                    name: entry.name.clone(),
                };
                if stats
                    .soonest_expiry
                    .as_ref()
                    .is_none_or(|s| t < s.expires_at)
                {
                    stats.soonest_expiry = Some(EntryTtl {
                        expires_at: candidate.expires_at,
                        vk: candidate.vk.clone(),
                        name: candidate.name.clone(),
                    });
                }
                if stats
                    .latest_expiry
                    .as_ref()
                    .is_none_or(|s| t > s.expires_at)
                {
                    stats.latest_expiry = Some(candidate);
                }
            }
        }
    }
    stats
}

fn dir_size(root: &Path) -> std::io::Result<u64> {
    let mut total = 0u64;
    let mut stack = vec![root.to_path_buf()];
    while let Some(p) = stack.pop() {
        let rd = match std::fs::read_dir(&p) {
            Ok(r) => r,
            Err(_) => continue,
        };
        for entry in rd.flatten() {
            let meta = match entry.metadata() {
                Ok(m) => m,
                Err(_) => continue,
            };
            if meta.is_dir() {
                stack.push(entry.path());
            } else if meta.is_file() {
                total = total.saturating_add(meta.len());
            }
        }
    }
    Ok(total)
}

/// Re-export of the `Engine` type alias the relay uses, parameterized
/// over the (post-cutover) wrapping transport. Used as the `T` in
/// `build_dashboard_snapshot`'s signature when invoked from `relay.rs`.
///
/// The relay's actual T is
///   SpawningAcceptor<WebSocketRawTransport, NoiseTransport<WebSocketRawTransport>, _, _, _>
/// — but this module doesn't need to spell it out because it works
/// generically over any T: Transport. Keeping a placeholder alias here
/// helps grepping.
#[allow(dead_code)]
pub type RelayEngine<T> = SyncEngine<FsStore, T>;

// Imports the relay's outbound side might pull in for `T`. Listed here
// only so unused imports don't drift when the builder type-erases.
#[allow(dead_code)]
type _DialOnlyConnector = NoiseTransport<WebSocketRawTransport>;
```

- [ ] **Step 2: Wire it into the crate**

In `crates/sunset-relay/src/lib.rs`:

```rust
pub mod snapshot;
```

- [ ] **Step 3: Build**

Run: `nix develop --command cargo build -p sunset-relay`
Expected: compiles. Used by the next task.

- [ ] **Step 4: Commit**

```bash
git add crates/sunset-relay/src/snapshot.rs crates/sunset-relay/src/lib.rs
git commit -m "$(cat <<'EOF'
sunset-relay: add engine-side snapshot builder

build_dashboard_snapshot / build_identity_snapshot consume
Rc<SyncEngine> + Arc<FsStore> and produce Send PODs that the axum
handlers consume. Generic over the wrapping Transport so the relay's
SpawningAcceptor type doesn't leak into the snapshot module.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 10: `sunset-relay` — axum-side renderer

**Files:**
- Create: `crates/sunset-relay/src/render.rs`
- Modify: `crates/sunset-relay/src/lib.rs`

This module is the move of the rendering logic out of `status.rs` into a Send-friendly pure function over snapshots. `status.rs` itself stays in tree until task 14 to keep this commit compilable.

- [ ] **Step 1: Create the render module**

Create `crates/sunset-relay/src/render.rs`:

```rust
//! Pure-function renderers used by the axum handlers. Inputs are the
//! `Send` POD snapshots from `bridge.rs`; outputs are HTML/JSON strings.
//! No engine handles or `Rc`s here.

use bytes::Bytes;

use sunset_store::{Filter, VerifyingKey};
use sunset_sync::PeerId;

use crate::bridge::{DashboardSnapshot, IdentitySnapshot, StoreStats};

/// Plaintext dashboard body. Same shape and content as the original
/// `status.rs::render`, just driven from a snapshot instead of an
/// `Rc<Engine>`.
pub fn render_dashboard(snap: &DashboardSnapshot) -> String {
    let mut out = String::new();
    out.push_str("sunset-relay\n");
    out.push_str("============\n\n");

    out.push_str("identity\n--------\n");
    out.push_str(&format!("ed25519:  {}\n", hex::encode(snap.ed25519_public)));
    out.push_str(&format!("x25519:   {}\n", hex::encode(snap.x25519_public)));
    out.push_str(&format!("listen:   ws://{}\n", snap.listen_addr));
    out.push_str(&format!("dial:     {}\n\n", snap.dial_url));

    out.push_str("peers\n-----\n");
    if snap.configured_peers.is_empty() {
        out.push_str("configured federated peers: (none)\n");
    } else {
        out.push_str(&format!(
            "configured federated peers ({}):\n",
            snap.configured_peers.len()
        ));
        for p in &snap.configured_peers {
            out.push_str(&format!("  - {}\n", p));
        }
    }
    if snap.connected_peers.is_empty() {
        out.push_str("connected peers:            (none)\n");
    } else {
        out.push_str(&format!("connected peers ({}):\n", snap.connected_peers.len()));
        for p in &snap.connected_peers {
            out.push_str(&format!("  - ed25519:{}\n", peer_short(p)));
        }
    }
    out.push('\n');

    out.push_str("subscriptions (advertised by connected peers)\n");
    out.push_str("---------------------------------------------\n");
    if snap.subscriptions.is_empty() {
        out.push_str("(none)\n\n");
    } else {
        for (peer, filter) in &snap.subscriptions {
            out.push_str(&format!(
                "  ed25519:{} -> {}\n",
                peer_short(peer),
                format_filter(filter)
            ));
        }
        out.push('\n');
    }

    out.push_str("store\n-----\n");
    let stats = &snap.store_stats;
    out.push_str(&format!("data dir:           {}\n", snap.data_dir.display()));
    out.push_str(&format!("on-disk size:       {}\n", human_bytes(snap.on_disk_size)));
    out.push_str(&format!("entries:            {}\n", stats.entry_count));
    out.push_str(&format!("  with ttl:         {}\n", stats.entries_with_ttl));
    out.push_str(&format!("  without ttl:      {}\n", stats.entries_without_ttl));
    out.push_str(&format!(
        "  subscriptions:    {} (under `_sunset-sync/subscribe`)\n",
        stats.subscription_entries
    ));
    out.push_str(&format!(
        "current cursor:     {}\n",
        stats
            .cursor
            .map(|c| c.to_string())
            .unwrap_or_else(|| "?".into())
    ));

    let now = web_time::SystemTime::now()
        .duration_since(web_time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    if let Some(s) = &stats.soonest_expiry {
        out.push_str(&format!(
            "soonest expiry:     in {} ({})\n",
            human_secs_until(s.expires_at, now),
            describe_entry(&s.vk, &s.name),
        ));
    } else {
        out.push_str("soonest expiry:     (no entries with ttl)\n");
    }
    if let Some(l) = &stats.latest_expiry {
        out.push_str(&format!(
            "latest expiry:      in {} ({})\n",
            human_secs_until(l.expires_at, now),
            describe_entry(&l.vk, &l.name),
        ));
    }

    out
}

/// JSON identity. Hex-only field values, no escaping needed.
pub fn render_identity(snap: &IdentitySnapshot) -> String {
    format!(
        "{{\"ed25519\":\"{}\",\"x25519\":\"{}\",\"address\":\"{}\"}}\n",
        hex::encode(snap.ed25519_public),
        hex::encode(snap.x25519_public),
        snap.dial_url,
    )
}

// --- helpers ---

fn peer_short(p: &PeerId) -> String {
    let h = hex::encode(p.0.as_bytes());
    if h.len() <= 16 {
        h
    } else {
        format!("{}…", &h[..16])
    }
}

fn vk_short(vk: &VerifyingKey) -> String {
    let h = hex::encode(vk.as_bytes());
    if h.len() <= 16 {
        h
    } else {
        format!("{}…", &h[..16])
    }
}

fn describe_entry(vk: &VerifyingKey, name: &Bytes) -> String {
    let name_str = match std::str::from_utf8(name) {
        Ok(s) => s.to_string(),
        Err(_) => format!("hex:{}", hex::encode(name)),
    };
    format!("vk={} name={}", vk_short(vk), name_str)
}

fn format_filter(f: &Filter) -> String {
    match f {
        Filter::Specific(vk, name) => {
            format!(
                "Specific(vk={}, name={})",
                vk_short(vk),
                String::from_utf8_lossy(name.as_ref())
            )
        }
        Filter::Keyspace(vk) => format!("Keyspace(vk={})", vk_short(vk)),
        Filter::Namespace(name) => {
            format!("Namespace({})", String::from_utf8_lossy(name.as_ref()))
        }
        Filter::NamePrefix(prefix) => {
            if prefix.is_empty() {
                "All (NamePrefix \"\")".to_string()
            } else {
                format!("NamePrefix({})", String::from_utf8_lossy(prefix.as_ref()))
            }
        }
        Filter::Union(filters) => {
            let parts: Vec<_> = filters.iter().map(format_filter).collect();
            format!("Union[{}]", parts.join(", "))
        }
    }
}

fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i + 1 < UNITS.len() {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{n} {}", UNITS[i])
    } else {
        format!("{:.1} {}", v, UNITS[i])
    }
}

fn human_secs_until(expires_at: u64, now: u64) -> String {
    if expires_at <= now {
        return "expired".to_string();
    }
    human_duration_secs(expires_at - now)
}

fn human_duration_secs(secs: u64) -> String {
    let d = secs / 86_400;
    let h = (secs % 86_400) / 3_600;
    let m = (secs % 3_600) / 60;
    let s = secs % 60;
    if d > 0 {
        format!("{d}d{h}h{m}m")
    } else if h > 0 {
        format!("{h}h{m}m")
    } else if m > 0 {
        format!("{m}m{s}s")
    } else {
        format!("{s}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_json_has_expected_shape() {
        let snap = IdentitySnapshot {
            ed25519_public: [0xab; 32],
            x25519_public: [0xcd; 32],
            dial_url: "ws://relay.example:8443".into(),
        };
        let json = render_identity(&snap);
        assert_eq!(
            json,
            "{\"ed25519\":\"abababababababababababababababababababababababababababababababab\",\
             \"x25519\":\"cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd\",\
             \"address\":\"ws://relay.example:8443\"}\n"
        );
    }

    #[test]
    fn dashboard_renders_minimal_snapshot() {
        let snap = DashboardSnapshot {
            ed25519_public: [0; 32],
            x25519_public: [0; 32],
            listen_addr: "127.0.0.1:8443".parse().unwrap(),
            dial_url: "ws://127.0.0.1:8443".into(),
            configured_peers: vec![],
            connected_peers: vec![],
            subscriptions: vec![],
            data_dir: std::path::PathBuf::from("/tmp/relay"),
            on_disk_size: 0,
            store_stats: StoreStats::default(),
        };
        let html = render_dashboard(&snap);
        assert!(html.starts_with("sunset-relay\n"));
        assert!(html.contains("connected peers:            (none)"));
        assert!(html.contains("subscriptions (advertised by connected peers)"));
    }
}
```

- [ ] **Step 2: Wire it into the crate**

In `crates/sunset-relay/src/lib.rs`:

```rust
pub mod render;
```

- [ ] **Step 3: Run the new tests**

Run: `nix develop --command cargo test -p sunset-relay render`
Expected: both renderer unit tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/sunset-relay/src/render.rs crates/sunset-relay/src/lib.rs
git commit -m "$(cat <<'EOF'
sunset-relay: add render module — Send-friendly snapshot renderers

render_dashboard / render_identity are pure functions over the bridge
PODs. They mirror the body that status.rs currently produces; status.rs
stays in tree until the cut-over so the workspace continues to compile.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 11: `sunset-relay` — axum app + handlers

**Files:**
- Create: `crates/sunset-relay/src/app.rs`
- Modify: `crates/sunset-relay/src/lib.rs`
- Modify: `crates/sunset-relay/Cargo.toml`

- [ ] **Step 1: Add axum dep to the relay**

In `crates/sunset-relay/Cargo.toml`, under `[dependencies]`:

```toml
axum = { workspace = true }
sunset-sync-ws-native = { workspace = true, features = ["axum"] }
```

(Replace the existing `sunset-sync-ws-native.workspace = true` line with the feature-flagged form.)

- [ ] **Step 2: Create the app module**

Create `crates/sunset-relay/src/app.rs`:

```rust
//! axum app + handlers for the relay's HTTP/WS endpoints.
//!
//! The app holds only `Send` state: the WS upgrade sender and the
//! engine-command sender. All engine reads go through `RelayCommand`.

use axum::extract::{State, WebSocketUpgrade};
use axum::http::{HeaderMap, header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use tokio::sync::{mpsc, oneshot};

use sunset_sync_ws_native::axum_integration::ws_handler;

use crate::bridge::RelayCommand;
use crate::render::{render_dashboard, render_identity};

#[derive(Clone)]
pub struct AppState {
    /// Sends already-upgraded axum WebSockets to the engine-side
    /// `WebSocketRawTransport::serving()` channel.
    pub ws_tx: mpsc::UnboundedSender<axum::extract::ws::WebSocket>,
    /// Sends commands (snapshot, identity) to the engine-side command pump.
    pub cmd_tx: mpsc::UnboundedSender<RelayCommand>,
}

pub fn build_app(state: AppState) -> Router {
    Router::new()
        .route("/dashboard", get(dashboard_handler))
        .route("/", get(root_handler))
        .with_state(state)
}

async fn dashboard_handler(State(state): State<AppState>) -> Response {
    let (reply, rx) = oneshot::channel();
    if state
        .cmd_tx
        .send(RelayCommand::Snapshot { reply })
        .is_err()
    {
        return (StatusCode::SERVICE_UNAVAILABLE, "engine unavailable\n").into_response();
    }
    let snap = match rx.await {
        Ok(s) => s,
        Err(_) => {
            return (StatusCode::SERVICE_UNAVAILABLE, "engine unavailable\n").into_response();
        }
    };
    let body = render_dashboard(&snap);
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        "text/plain; charset=utf-8".parse().unwrap(),
    );
    headers.insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    (StatusCode::OK, headers, body).into_response()
}

/// Either a WebSocket upgrade (engine path) or the JSON identity descriptor
/// for browsers/clients that GET / without an Upgrade header.
async fn root_handler(
    State(state): State<AppState>,
    upgrade: Option<WebSocketUpgrade>,
) -> Response {
    if let Some(ws) = upgrade {
        return ws_handler(ws, state.ws_tx).await;
    }
    let (reply, rx) = oneshot::channel();
    if state
        .cmd_tx
        .send(RelayCommand::Identity { reply })
        .is_err()
    {
        return (StatusCode::SERVICE_UNAVAILABLE, "engine unavailable\n").into_response();
    }
    let snap = match rx.await {
        Ok(s) => s,
        Err(_) => {
            return (StatusCode::SERVICE_UNAVAILABLE, "engine unavailable\n").into_response();
        }
    };
    let body = render_identity(&snap);
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        "application/json; charset=utf-8".parse().unwrap(),
    );
    headers.insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    headers.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*".parse().unwrap());
    (StatusCode::OK, headers, body).into_response()
}
```

- [ ] **Step 3: Wire it into the crate**

In `crates/sunset-relay/src/lib.rs`:

```rust
pub mod app;
```

- [ ] **Step 4: Build**

Run: `nix develop --command cargo build -p sunset-relay`
Expected: compiles.

- [ ] **Step 5: Commit**

```bash
git add crates/sunset-relay/Cargo.toml crates/sunset-relay/src/app.rs crates/sunset-relay/src/lib.rs
git commit -m "$(cat <<'EOF'
sunset-relay: add axum app + dashboard/identity/WS handlers

build_app(state) -> Router. Handlers hold only Send senders: ws_tx for
upgrades, cmd_tx for snapshot/identity reads. /dashboard always returns
plaintext; / either WS-upgrades (when Upgrade: websocket present) or
returns the JSON identity (CORS-open).

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 12: `sunset-relay` — wire `relay.rs` to the new path

This is the cut-over: `Relay::new` and `RelayHandle::run` switch from the byte-peek dispatcher + `external_streams` plumbing to the axum + `SpawningAcceptor` design. After this task, `router.rs` and the existing `status.rs` are no longer reached at runtime, but they remain in tree to keep the change reviewable; they're deleted in task 14.

**Files:**
- Modify: `crates/sunset-relay/src/config.rs`
- Modify: `crates/sunset-relay/src/relay.rs`
- Modify: `crates/sunset-relay/src/main.rs`

- [ ] **Step 1: Add `accept_handshake_timeout_secs` to `Config`**

In `crates/sunset-relay/src/config.rs`, change `Config` to add a field and `RawConfig` to parse it:

```rust
pub struct Config {
    pub listen_addr: SocketAddr,
    pub data_dir: PathBuf,
    pub interest_filter: InterestFilter,
    pub identity_secret_path: PathBuf,
    pub peers: Vec<String>,
    pub accept_handshake_timeout_secs: u64,
}
```

```rust
struct RawConfig {
    listen_addr: Option<String>,
    data_dir: Option<String>,
    interest_filter: Option<String>,
    identity_secret: Option<String>,
    #[serde(default)]
    peers: Vec<String>,
    accept_handshake_timeout_secs: Option<u64>,
}
```

In `from_raw`, append:

```rust
let accept_handshake_timeout_secs = raw.accept_handshake_timeout_secs.unwrap_or(15);
```

…and include the field in the `Config` constructor at the bottom of `from_raw`. Update existing tests that match on `Config` exhaustively if any do (most use direct field access).

Add a unit test:

```rust
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
```

Run: `nix develop --command cargo test -p sunset-relay config`
Expected: passes.

- [ ] **Step 2: Rewrite `relay.rs`**

Replace the contents of `crates/sunset-relay/src/relay.rs` with the new flow. The shape is:

- `Relay::new(config)` opens identity + store, builds the engine on the local set, sets up the `SpawningAcceptor` (which owns the inbound `WebSocketRawTransport::serving()` half), spawns the command pump, and returns a `RelayHandle` that owns: the `Rc<Engine>`, the bridge senders (`ws_tx`, `cmd_tx`), the bound `tokio::net::TcpListener`, and the configured peers list.
- `RelayHandle::run` builds the axum app from the senders, spawns axum::serve via `tokio::spawn`, dials federated peers via `engine.add_peer`, then waits for SIGINT/SIGTERM.
- `RelayHandle::run_for_test` mirrors `run` minus the OS-signal wait.

```rust
//! Relay: identity + store + engine + axum HTTP/WS host.
//!
//! `Relay::new(config)` does setup synchronously (in async fn form):
//! identity, store, engine, the SpawningAcceptor that wraps a
//! WebSocketRawTransport::serving(), the command pump, and a bound
//! TcpListener. The returned `RelayHandle` exposes the dial URL + a
//! `run`/`run_for_test` method that drives axum and the engine task
//! until shutdown.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use bytes::Bytes;
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use zeroize::Zeroizing;

use sunset_core::Identity;
use sunset_noise::{
    NoiseConnection, NoiseIdentity, NoiseTransport, do_handshake_responder,
    ed25519_seed_to_x25519_secret,
};
use sunset_store::{Filter, VerifyingKey};
use sunset_store_fs::FsStore;
use sunset_sync::{
    PeerAddr, PeerId, Signer, SpawningAcceptor, SyncConfig, SyncEngine,
};
use sunset_sync_ws_native::WebSocketRawTransport;

use crate::app::{AppState, build_app};
use crate::bridge::{DashboardSnapshot, IdentitySnapshot, RelayCommand};
use crate::config::{Config, InterestFilter};
use crate::error::Result;
use crate::identity;
use crate::snapshot::{build_dashboard_snapshot, build_identity_snapshot};

/// Concrete inbound-side `Transport` the engine sees. Kept private —
/// callers interact with `RelayHandle`, not this type.
type InboundTransport = SpawningAcceptor<
    WebSocketRawTransport,
    NoiseTransport<WebSocketRawTransport>,
    InboundPromote,
    NoiseHandshakeFuture,
    NoiseConnection<sunset_sync_ws_native::WebSocketRawConnection>,
>;

/// Type-erased pieces of the `SpawningAcceptor`'s generic parameters.
/// Defining the closure as a concrete `fn` is impossible (it captures
/// `Arc<dyn NoiseIdentity>`); we surface it via a boxed trait object
/// shape on the engine side.
type InboundPromote = Box<
    dyn Fn(sunset_sync_ws_native::WebSocketRawConnection) -> NoiseHandshakeFuture + 'static,
>;

type NoiseHandshakeFuture = std::pin::Pin<
    Box<
        dyn std::future::Future<
                Output = sunset_sync::Result<NoiseConnection<sunset_sync_ws_native::WebSocketRawConnection>>,
            > + 'static,
    >,
>;

type Engine = SyncEngine<FsStore, InboundTransport>;

pub struct Relay {/* sealed; see RelayHandle */}

pub struct RelayHandle {
    pub local_address: String,
    pub ed25519_public: [u8; 32],
    pub x25519_public: [u8; 32],

    engine: Rc<Engine>,
    peers: Vec<String>,
    subscription_filter: Filter,
    listener: Option<TcpListener>,
    /// Senders the axum app uses. Built once in `new`; cloned into
    /// `AppState` in `run` / `run_for_test`.
    ws_tx: mpsc::UnboundedSender<axum::extract::ws::WebSocket>,
    cmd_tx: mpsc::UnboundedSender<RelayCommand>,
    /// Engine-side context used by the command pump (one shared Rc).
    cmd_ctx: Rc<CommandContext>,
}

/// Held by the command pump task on the engine side. Captures the
/// references it needs to build snapshots without crossing runtimes.
struct CommandContext {
    engine: Rc<Engine>,
    store: Arc<FsStore>,
    data_dir: PathBuf,
    ed25519_public: [u8; 32],
    x25519_public: [u8; 32],
    listen_addr: SocketAddr,
    dial_url: String,
    configured_peers: Vec<String>,
}

/// Adapter so sunset-core's `Identity` can be used as a `NoiseIdentity`.
struct IdentityNoiseAdapter(Identity);

impl NoiseIdentity for IdentityNoiseAdapter {
    fn ed25519_public(&self) -> [u8; 32] {
        self.0.public().as_bytes()
    }
    fn ed25519_secret_seed(&self) -> Zeroizing<[u8; 32]> {
        Zeroizing::new(self.0.secret_bytes())
    }
}

impl Relay {
    /// Open store, load identity, bind listener, build engine. Returns a
    /// handle ready for `run()` / `run_for_test()`.
    #[allow(clippy::new_ret_no_self)]
    pub async fn new(config: Config) -> Result<RelayHandle> {
        // 1. Identity (load-or-generate; persists to disk on first start).
        tokio::fs::create_dir_all(&config.data_dir).await?;
        let identity = identity::load_or_generate(&config.identity_secret_path).await?;

        let ed25519_public = identity.public().as_bytes();
        let x25519_public = {
            let s = ed25519_seed_to_x25519_secret(&identity.secret_bytes());
            use curve25519_dalek::{MontgomeryPoint, scalar::Scalar};
            let scalar = Scalar::from_bytes_mod_order(*s);
            MontgomeryPoint::mul_base(&scalar).to_bytes()
        };

        // 2. Store.
        let store_root = config.data_dir.join("store");
        tokio::fs::create_dir_all(&store_root).await?;
        let store = Arc::new(
            FsStore::with_verifier(&store_root, Arc::new(sunset_core::Ed25519Verifier)).await?,
        );

        // 3. Bind the HTTP/WS listener up front so we know the bound port.
        let listener = TcpListener::bind(config.listen_addr).await?;
        let bound = listener.local_addr().unwrap_or(config.listen_addr);
        let local_address = format!("ws://{}#x25519={}", bound, hex::encode(x25519_public));

        // 4. Inbound side: serving() exposes a Send sender for axum and a
        //    drainable RawTransport. Outbound side: dial_only.
        let (raw_inbound, ws_tx) = WebSocketRawTransport::serving();
        let raw_outbound = WebSocketRawTransport::dial_only();
        let noise_id = Arc::new(IdentityNoiseAdapter(identity.clone()));
        let connector = NoiseTransport::new(raw_outbound, noise_id.clone());

        // 5. SpawningAcceptor — every inbound connection's Noise IK runs
        //    on its own task, bounded by the configured handshake timeout.
        let handshake_timeout = Duration::from_secs(config.accept_handshake_timeout_secs);
        let promote: InboundPromote = {
            let identity = noise_id.clone();
            Box::new(move |raw_conn| {
                let identity = identity.clone();
                Box::pin(async move {
                    do_handshake_responder(raw_conn, identity)
                        .await
                        .map_err(|e| {
                            sunset_sync::Error::Transport(format!("noise responder: {e}"))
                        })
                })
            })
        };
        let transport = SpawningAcceptor::new(raw_inbound, connector, promote, handshake_timeout);

        // 6. Engine.
        let local_peer = PeerId(VerifyingKey::new(Bytes::copy_from_slice(&ed25519_public)));
        let signer: Arc<dyn Signer> = Arc::new(identity.clone());
        let engine = Rc::new(SyncEngine::new(
            store.clone(),
            transport,
            SyncConfig::default(),
            local_peer,
            signer,
        ));

        // 7. Subscription filter for the relay's broad ingestion.
        let subscription_filter = match config.interest_filter {
            InterestFilter::All => Filter::NamePrefix(Bytes::new()),
        };

        // 8. Bridge channels.
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<RelayCommand>();

        // 9. Command pump context + task.
        let cmd_ctx = Rc::new(CommandContext {
            engine: engine.clone(),
            store: store.clone(),
            data_dir: config.data_dir.clone(),
            ed25519_public,
            x25519_public,
            listen_addr: bound,
            dial_url: local_address.clone(),
            configured_peers: config.peers.clone(),
        });
        spawn_command_pump(cmd_rx, cmd_ctx.clone());

        // 10. Banner.
        let mut banner = identity::format_address(&bound, &identity);
        banner.push_str(&format!("\n  dashboard: http://{}/dashboard", bound));
        banner.push_str(&format!("\n  identity:  http://{}/", bound));
        tracing::info!("\n{}", banner);
        println!("{}", banner);

        Ok(RelayHandle {
            local_address,
            ed25519_public,
            x25519_public,
            engine,
            peers: config.peers,
            subscription_filter,
            listener: Some(listener),
            ws_tx,
            cmd_tx,
            cmd_ctx,
        })
    }
}

fn spawn_command_pump(
    mut cmd_rx: mpsc::UnboundedReceiver<RelayCommand>,
    ctx: Rc<CommandContext>,
) {
    tokio::task::spawn_local(async move {
        while let Some(cmd) = cmd_rx.recv().await {
            match cmd {
                RelayCommand::Snapshot { reply } => {
                    let snap = build_dashboard_snapshot(
                        &ctx.engine,
                        &ctx.store,
                        &ctx.data_dir,
                        ctx.ed25519_public,
                        ctx.x25519_public,
                        ctx.listen_addr,
                        &ctx.dial_url,
                        &ctx.configured_peers,
                    )
                    .await;
                    let _ = reply.send(snap);
                }
                RelayCommand::Identity { reply } => {
                    let snap = build_identity_snapshot(
                        ctx.ed25519_public,
                        ctx.x25519_public,
                        &ctx.dial_url,
                    );
                    let _ = reply.send(snap);
                }
            }
        }
    });
}

impl RelayHandle {
    pub fn dial_address(&self) -> String {
        self.local_address.clone()
    }

    async fn dial_configured_peers(&self) {
        use sunset_relay_resolver::Resolver;
        let resolver = Resolver::new(crate::resolver_adapter::ReqwestFetch::default());
        for peer_url in &self.peers {
            let canonical = match resolver.resolve(peer_url).await {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!(peer = %peer_url, error = %e, "peer resolve failed, skipping");
                    continue;
                }
            };
            let addr = PeerAddr::new(Bytes::from(canonical));
            if let Err(e) = self.engine.add_peer(addr).await {
                tracing::warn!(peer = %peer_url, error = %e, "federated peer dial failed, continuing");
            } else {
                tracing::info!(peer = %peer_url, "federated peer dialed");
            }
        }
    }

    fn build_app_state(&self) -> AppState {
        AppState {
            ws_tx: self.ws_tx.clone(),
            cmd_tx: self.cmd_tx.clone(),
        }
    }

    /// Drive the engine + axum until shutdown.
    pub async fn run(mut self) -> Result<()> {
        let listener = self
            .listener
            .take()
            .expect("RelayHandle::run consumed twice");
        let app: Router = build_app(self.build_app_state());

        let engine_clone = self.engine.clone();
        let engine_task = tokio::task::spawn_local(async move { engine_clone.run().await });

        // axum runs as a Send task on the multi-thread runtime workers.
        let serve_task = tokio::spawn(async move {
            axum::serve(listener, app).await
        });

        // Subscription publish + federated dials happen on the engine side.
        self.engine
            .publish_subscription(self.subscription_filter.clone(), Duration::from_secs(3600))
            .await?;
        tracing::info!("published broad subscription");
        self.dial_configured_peers().await;

        #[cfg(unix)]
        {
            let mut sigterm =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {
                    tracing::info!("received SIGINT, shutting down");
                }
                _ = sigterm.recv() => {
                    tracing::info!("received SIGTERM, shutting down");
                }
            }
        }
        #[cfg(not(unix))]
        {
            tokio::signal::ctrl_c().await?;
            tracing::info!("received Ctrl+C, shutting down");
        }

        engine_task.abort();
        serve_task.abort();
        Ok(())
    }

    /// For tests: drive engine + axum without waiting for OS signals.
    /// Returns the engine task handle so the caller can abort it during teardown.
    /// The axum task is detached; the test runtime drop will cancel it.
    #[cfg(any(test, feature = "test-helpers"))]
    pub async fn run_for_test(
        &mut self,
    ) -> Result<tokio::task::JoinHandle<sunset_sync::Result<()>>> {
        let listener = self
            .listener
            .take()
            .expect("RelayHandle::run_for_test consumed twice");
        let app: Router = build_app(self.build_app_state());

        let engine_clone = self.engine.clone();
        let engine_task = tokio::task::spawn_local(async move { engine_clone.run().await });

        let _serve_task = tokio::spawn(async move {
            axum::serve(listener, app).await
        });

        self.engine
            .publish_subscription(self.subscription_filter.clone(), Duration::from_secs(3600))
            .await?;
        self.dial_configured_peers().await;

        Ok(engine_task)
    }

    /// For tests: access the underlying engine.
    #[cfg(any(test, feature = "test-helpers"))]
    pub fn engine(&self) -> &Rc<Engine> {
        &self.engine
    }
}

// `cmd_ctx` is held inside `RelayHandle` so the command pump's `Rc` graph
// stays alive for the relay's lifetime. The field is read-only (the pump
// task already holds its own clone), but storing it here documents the
// ownership: when RelayHandle drops, the pump task's Rc<CommandContext>
// becomes the only strong ref, the cmd_rx end of the channel closes when
// cmd_tx is dropped, the pump exits, and CommandContext drops.
impl Drop for RelayHandle {
    fn drop(&mut self) {
        // No-op; documented for clarity. tracing::trace! could go here.
    }
}
```

- [ ] **Step 3: Update `main.rs` to a multi-thread runtime + LocalSet**

Replace the body of `main` in `crates/sunset-relay/src/main.rs`:

```rust
fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("sunset_relay=info,sunset_sync=warn")),
        )
        .init();

    let cli = Cli::parse();

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    rt.block_on(async {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let config = match cli.config {
                    Some(path) => {
                        let text = std::fs::read_to_string(&path).map_err(|e| {
                            sunset_relay::Error::Config(format!("read {}: {e}", path.display(),))
                        })?;
                        Config::from_toml(&text)?
                    }
                    None => Config::defaults()?,
                };
                let handle = Relay::new(config).await?;
                handle.run().await
            })
            .await
    })
}
```

Add `tokio` features needed by the binary in `crates/sunset-relay/Cargo.toml`:

```toml
tokio = { workspace = true, features = ["io-util", "macros", "rt", "rt-multi-thread", "signal", "fs", "sync", "net", "time"] }
```

(Add `rt-multi-thread` to the existing list.)

- [ ] **Step 4: Build the workspace**

Run: `nix develop --command cargo build --workspace --all-features`
Expected: compiles. The old `router.rs` and `status.rs` are still in tree but unused.

- [ ] **Step 5: Run the relay's existing tests (without yet migrating http_index.rs)**

Run: `nix develop --command cargo test -p sunset-relay --all-features`
Expected:
- `accept_resilience.rs` tests pass — the SpawningAcceptor's per-task timeout (15 s default) replaces the engine-loop wrapper, and the healthy-client deadlines (5 s, 20 s) still hold because the rude probes occupy individual tasks.
- `multi_relay.rs` and `resolver_integration.rs` pass.
- `http_index.rs` may need migration (next task) — if its assertions are loose enough they may still pass against axum, but it's expected to need touch-up. If it fails, leave it failing for the next task; mark this task complete only if the *other* tests pass.

If `http_index.rs` is the only failure, proceed to task 13.

- [ ] **Step 6: Commit**

```bash
git add crates/sunset-relay/Cargo.toml crates/sunset-relay/src/relay.rs crates/sunset-relay/src/main.rs crates/sunset-relay/src/config.rs
git commit -m "$(cat <<'EOF'
sunset-relay: cut over to axum + SpawningAcceptor

Relay::new now sets up:
  • WebSocketRawTransport::serving() (axum-fed inbound) + dial_only (outbound)
  • NoiseTransport connector wrapping dial_only
  • SpawningAcceptor wrapping the inbound raw, connector, do_handshake_responder
  • a command pump on the LocalSet that answers Snapshot/Identity requests

RelayHandle::run / run_for_test build the axum app from the bridge
channels and spawn axum::serve via tokio::spawn (Send), engine via
spawn_local (?Send). Both run on a single multi_thread runtime + LocalSet
in the binary; current_thread + LocalSet in tests.

router.rs and status.rs are still in tree — unused at runtime — and are
deleted in the cleanup task once http_index.rs migrates.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 13: Migrate `tests/http_index.rs` to the axum routes

**Files:**
- Modify: `crates/sunset-relay/tests/http_index.rs`

The existing test sends raw HTTP/1.1 over a TcpStream and asserts on the response. axum's response shape is broadly compatible with the old hand-rolled responses, but a few details may differ (header casing, content-length presence, exact 404 body). Update assertions to be tolerant of axum's defaults.

- [ ] **Step 1: Update assertions to match axum's responses**

Edit `crates/sunset-relay/tests/http_index.rs`. Make the assertions tolerant to header casing (HTTP/1.1 headers are case-insensitive; axum may emit lowercase) and to extra headers axum adds. Replace the body of `get_root_returns_identity_json` and `get_unknown_path_is_404` with versions that:

- Match `HTTP/1.1 200 OK` / `HTTP/1.1 404 Not Found` (axum uses `Not Found` exactly; `404` prefix matches both old and new).
- Use `to_lowercase()` on the response header lines before searching for `content-type`, `access-control-allow-origin`.

Concretely, rewrite the two test bodies' assertions:

```rust
            let response_lower = response.to_ascii_lowercase();
            assert!(
                response.starts_with("HTTP/1.1 200"),
                "expected 200, got: {response}"
            );
            assert!(
                response_lower.contains("content-type: application/json"),
                "expected json content-type: {response}"
            );
            assert!(
                response_lower.contains("access-control-allow-origin: *"),
                "expected CORS header on identity response: {response}"
            );
            // Body is after the blank line.
            let body = response
                .split_once("\r\n\r\n")
                .map(|(_, b)| b)
                .unwrap_or(&response);
            assert!(
                body.contains(&format!("\"ed25519\":\"{ed_hex}\"")),
                "ed25519 field missing/wrong: {body}"
            );
            assert!(
                body.contains(&format!("\"x25519\":\"{x_hex}\"")),
                "x25519 field missing/wrong: {body}"
            );
            assert!(
                body.contains("\"address\":\"ws://"),
                "address field missing: {body}"
            );
```

For the 404 test, simply assert `response.starts_with("HTTP/1.1 404")` (axum emits `HTTP/1.1 404 Not Found`).

- [ ] **Step 2: Run the tests**

Run: `nix develop --command cargo test -p sunset-relay --test http_index --all-features`
Expected: both tests pass against axum.

- [ ] **Step 3: Commit**

```bash
git add crates/sunset-relay/tests/http_index.rs
git commit -m "$(cat <<'EOF'
sunset-relay: tests/http_index.rs — case-insensitive header matching

Same assertions, just tolerant to axum's lowercase header emission.
Confirms / and /dashboard routes still produce the expected
content-type, CORS, and body shapes.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 14: Delete the now-unused old plumbing

After cut-over, the old byte-peek dispatcher and the old WebSocketRawTransport modes are dead code. Remove them.

**Files:**
- Modify: `crates/sunset-sync-ws-native/src/lib.rs`
- Delete: `crates/sunset-relay/src/router.rs`
- Delete: `crates/sunset-relay/src/status.rs`
- Modify: `crates/sunset-relay/src/lib.rs`
- Modify: `crates/sunset-relay/Cargo.toml` (remove unused dev-deps)

- [ ] **Step 1: Delete `WebSocketRawTransport::listening_on` and `external_streams`**

In `crates/sunset-sync-ws-native/src/lib.rs`:

- Remove the `Listening { listener }` and `ExternalStreams { rx }` variants from `enum TransportMode`.
- Remove the `listening_on` and `external_streams` constructors from `impl WebSocketRawTransport`.
- Update `local_addr` to drop the deleted variants:

```rust
pub fn local_addr(&self) -> Option<std::net::SocketAddr> {
    match &self.mode {
        TransportMode::DialOnly => None,
        #[cfg(feature = "axum")]
        TransportMode::Serving { .. } => None,
    }
}
```

- Update `RawTransport::accept` to drop the deleted match arms — it becomes much simpler:

```rust
async fn accept(&self) -> SyncResult<Self::Connection> {
    #[cfg(feature = "axum")]
    if let TransportMode::Serving { rx } = &self.mode {
        let mut rx = rx.lock().await;
        let socket = rx
            .recv()
            .await
            .ok_or_else(|| SyncError::Transport("axum serving channel closed".into()))?;
        let (sink, stream) = futures_util::StreamExt::split(socket);
        return Ok(WebSocketRawConnection::new(
            WsSink::Axum(sink),
            WsStream::Axum(stream),
        ));
    }
    // DialOnly: accept never resolves.
    std::future::pending::<()>().await;
    unreachable!()
}
```

- Remove the `tokio::net::{TcpListener, TcpStream}` imports if they become unused (after the deletions, they should be — only `MaybeTlsStream<tokio::net::TcpStream>` remains, which is used in the client SplitSink/SplitStream type aliases via `tokio_tungstenite::MaybeTlsStream`).

- [ ] **Step 2: Delete `crates/sunset-relay/src/router.rs`**

```bash
rm crates/sunset-relay/src/router.rs
```

In `crates/sunset-relay/src/lib.rs`, remove the `pub(crate) mod router;` line.

- [ ] **Step 3: Delete `crates/sunset-relay/src/status.rs`**

```bash
rm crates/sunset-relay/src/status.rs
```

In `crates/sunset-relay/src/lib.rs`, remove the `pub(crate) mod status;` line.

- [ ] **Step 4: Remove now-unused dev-deps in `crates/sunset-relay/Cargo.toml`**

`tokio-tungstenite` was only used by the deleted byte-peek tests. Drop it from `[dev-dependencies]` if no remaining test references it. Verify with grep:

```bash
grep -r tokio_tungstenite crates/sunset-relay/
```

If nothing remains, remove the line. Otherwise leave it.

- [ ] **Step 5: Build the workspace**

Run: `nix develop --command cargo build --workspace --all-features`
Expected: compiles. Anything that referenced the deleted modes is the deleted code itself.

- [ ] **Step 6: Run all relay + ws-native tests**

Run: `nix develop --command cargo test -p sunset-relay -p sunset-sync-ws-native --all-features`
Expected: passes.

- [ ] **Step 7: Commit**

```bash
git add -A
git commit -m "$(cat <<'EOF'
remove the byte-peek dispatcher and old WS-native modes

After the axum cut-over:
  • crates/sunset-relay/src/router.rs (the byte-peek HTTP/WS classifier)
    is unreachable. Deleted.
  • crates/sunset-relay/src/status.rs is superseded by snapshot.rs +
    render.rs (and the cmd-pump in relay.rs). Deleted.
  • WebSocketRawTransport::{listening_on, external_streams} and the
    matching TransportMode variants are unused. Deleted, simplifying
    the transport to {dial_only, serving} only.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 15: Delete `accept_with_timeout` and `SyncConfig::accept_handshake_timeout`

The engine's accept loop no longer needs the timeout wrapper; the SpawningAcceptor's internal channel never blocks on a handshake. The config field has no consumers in `sunset-sync` after this change.

**Files:**
- Modify: `crates/sunset-sync/src/engine.rs`
- Modify: `crates/sunset-sync/src/types.rs`

- [ ] **Step 1: Replace the engine's accept arm**

In `crates/sunset-sync/src/engine.rs`:

- Delete the `accept_with_timeout` async helper function (lines around 47–66, including the `cfg`s).
- In `SyncEngine::run`'s `select!` block (around line 387), replace the `maybe_conn = accept_with_timeout(...) => { … }` arm with a plain accept arm:

```rust
loop {
    tokio::select! {
        accept_res = self.transport.accept() => {
            match accept_res {
                Ok(conn) => self.spawn_peer(conn, inbound_tx.clone()).await,
                Err(e) => {
                    // A single accept failure (e.g. an upstream pump that's
                    // shutting down) must not tear down the engine — log and
                    // keep accepting. If the channel underneath has truly
                    // closed, every subsequent accept will return an error
                    // too; that's fine — eventually the engine task is
                    // aborted by the host.
                    eprintln!("sunset-sync: transport accept failed; continuing: {e}");
                }
            }
        }
        Some(cmd) = cmd_rx.recv() => {
            self.handle_command(cmd, &inbound_tx).await;
        }
        Some(event) = inbound_rx.recv() => {
            self.handle_inbound_event(event).await;
        }
        Some(item) = local_sub.next() => {
            match item {
                Ok(ev) => self.handle_local_store_event(ev).await,
                Err(e) => return Err(Error::Store(e)),
            }
        }
        _ = anti_entropy.tick() => {
            self.tick_anti_entropy().await;
        }
    }
}
```

- [ ] **Step 2: Delete the `accept_handshake_timeout` field**

In `crates/sunset-sync/src/types.rs`, delete the field from the struct and from the `Default` impl (lines around 58–63 and 76).

```rust
pub struct SyncConfig {
    pub protocol_version: u32,
    pub anti_entropy_interval: Duration,
    pub bloom_size_bits: usize,
    pub bloom_hash_fns: u32,
    pub bootstrap_filter: Filter,
    pub heartbeat_interval: Duration,
    pub heartbeat_timeout: Duration,
}

impl Default for SyncConfig {
    fn default() -> Self {
        Self {
            protocol_version: 1,
            anti_entropy_interval: Duration::from_secs(30),
            bloom_size_bits: 4096,
            bloom_hash_fns: 4,
            bootstrap_filter: Filter::Namespace(reserved::SUBSCRIBE_NAME.into()),
            heartbeat_interval: Duration::from_secs(15),
            heartbeat_timeout: Duration::from_secs(45),
        }
    }
}
```

- [ ] **Step 3: Build + run tests**

Run: `nix develop --command cargo test --workspace --all-features`
Expected: full suite passes. Existing accept_resilience tests rely on the relay's per-acceptor handshake timeout (15 s default in Config), not the engine's deleted field, so they pass unchanged.

- [ ] **Step 4: Commit**

```bash
git add crates/sunset-sync/src/engine.rs crates/sunset-sync/src/types.rs
git commit -m "$(cat <<'EOF'
sunset-sync: delete accept_with_timeout + SyncConfig::accept_handshake_timeout

Both unused after the relay cut-over: SpawningAcceptor's per-task
timeout in the promote callback owns the bound now. The engine's
accept arm becomes a plain transport.accept() await — the connection
either arrives (already authenticated) or doesn't.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 16: Add `tests/relay_concurrent_handshakes.rs`

The regression test that proves slow handshakes don't serialize.

**Files:**
- Create: `crates/sunset-relay/tests/relay_concurrent_handshakes.rs`

- [ ] **Step 1: Create the test**

```rust
//! Regression test for the inbound-pipeline concurrency property.
//!
//! Spec: docs/superpowers/specs/2026-05-02-relay-axum-and-concurrent-handshakes-design.md
//!
//! With a small per-acceptor handshake timeout, launch N rude WS clients
//! that complete the upgrade and then stall (never send the Noise IK
//! initiator message). Then launch one healthy client. Assert the healthy
//! client completes its full Noise+Hello within ~3 s — i.e., not roughly
//! N × handshake_timeout.

use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use rand_core::OsRng;
use zeroize::Zeroizing;

use sunset_core::{Ed25519Verifier, Identity};
use sunset_noise::{NoiseIdentity, NoiseTransport};
use sunset_relay::{Config, Relay};
use sunset_store_memory::MemoryStore;
use sunset_sync::{PeerAddr, PeerId, Signer, SyncConfig, SyncEngine};
use sunset_sync_ws_native::WebSocketRawTransport;

struct IdentityAdapter(Identity);

impl NoiseIdentity for IdentityAdapter {
    fn ed25519_public(&self) -> [u8; 32] {
        self.0.public().as_bytes()
    }
    fn ed25519_secret_seed(&self) -> Zeroizing<[u8; 32]> {
        Zeroizing::new(self.0.secret_bytes())
    }
}

fn relay_config(data_dir: &std::path::Path, handshake_timeout_secs: u64) -> Config {
    Config::from_toml(&format!(
        r#"
        listen_addr = "127.0.0.1:0"
        data_dir = "{}"
        interest_filter = "all"
        identity_secret = "auto"
        peers = []
        accept_handshake_timeout_secs = {handshake_timeout_secs}
        "#,
        data_dir.display(),
    ))
    .unwrap()
}

fn extract_host_port(dial_addr: &str) -> String {
    dial_addr
        .strip_prefix("ws://")
        .unwrap()
        .split(['#', '/'])
        .next()
        .unwrap()
        .to_string()
}

#[tokio::test(flavor = "current_thread")]
async fn rude_clients_do_not_serialize_a_healthy_dial() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            // Short timeout so the test runs fast even when rude clients
            // eventually time out.
            let dir = tempfile::tempdir().unwrap();
            let mut relay = Relay::new(relay_config(dir.path(), 1))
                .await
                .expect("relay new");
            let dial_addr = relay.dial_address();
            let host_port = extract_host_port(&dial_addr);
            let _engine_task = relay.run_for_test().await.expect("relay run");

            // Launch N=8 rude WS clients in parallel — each completes the
            // WS upgrade then sits silent. The relay spawns one promote
            // task per upgrade; with concurrent_acceptor, none of them
            // block the others.
            let mut rude_handles = Vec::new();
            for _ in 0..8 {
                let url = format!("ws://{host_port}/");
                rude_handles.push(tokio::task::spawn_local(async move {
                    let (_ws, _resp) = tokio_tungstenite::connect_async(&url)
                        .await
                        .expect("rude WS upgrade");
                    // Hold the connection open without sending Noise.
                    tokio::time::sleep(Duration::from_secs(10)).await;
                }));
            }

            // Tiny settle so the relay has accepted the rude upgrades.
            tokio::time::sleep(Duration::from_millis(200)).await;

            // Healthy client: a normal Noise+Hello dial. Under the new
            // wiring, this completes within ~RTT regardless of how many
            // rude clients are stalled.
            let alice = Identity::generate(&mut OsRng);
            let store = Arc::new(MemoryStore::new(Arc::new(Ed25519Verifier)));
            let raw = WebSocketRawTransport::dial_only();
            let noise = NoiseTransport::new(raw, Arc::new(IdentityAdapter(alice.clone())));
            let signer: Arc<dyn Signer> = Arc::new(alice.clone());
            let engine = Rc::new(SyncEngine::new(
                store,
                noise,
                SyncConfig::default(),
                PeerId(alice.store_verifying_key()),
                signer,
            ));
            let engine_clone = engine.clone();
            tokio::task::spawn_local(async move {
                let _ = engine_clone.run().await;
            });

            let dial_result = tokio::time::timeout(
                Duration::from_secs(3),
                engine.add_peer(PeerAddr::new(Bytes::from(dial_addr))),
            )
            .await;

            for h in rude_handles {
                h.abort();
            }

            match dial_result {
                Err(_) => panic!(
                    "healthy dial did not complete within 3 s — \
                     8 rude clients are serializing the inbound pipeline. \
                     SpawningAcceptor's spawn-per-conn property has regressed."
                ),
                Ok(Err(e)) => panic!("healthy dial returned err: {e:?}"),
                Ok(Ok(_)) => {}
            }
        })
        .await;
}
```

- [ ] **Step 2: Run the test**

Run: `nix develop --command cargo test -p sunset-relay --test relay_concurrent_handshakes --all-features`
Expected: passes within ~3 s.

- [ ] **Step 3: Commit**

```bash
git add crates/sunset-relay/tests/relay_concurrent_handshakes.rs
git commit -m "$(cat <<'EOF'
sunset-relay: regression test for inbound-pipeline concurrency

Eight rude WS clients complete the upgrade and stall; a healthy client
must still complete Noise+Hello within 3 s. With a 1 s per-task
handshake timeout, the old serial accept loop would have made the
healthy dial wait ~8 s for the rude probes to time out one at a time.
SpawningAcceptor runs each promote on its own task, so the healthy
dial lands within RTT.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 17: Final verification

**Files:** none.

- [ ] **Step 1: Full test suite**

Run: `nix develop --command cargo test --workspace --all-features`
Expected: all tests pass.

- [ ] **Step 2: Clippy with -D warnings**

Run: `nix develop --command cargo clippy --workspace --all-features --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 3: Format check**

Run: `nix develop --command cargo fmt --all --check`
Expected: clean. If it complains, run `cargo fmt --all` and amend the most recent commit (or, if many commits are unformatted, run `cargo fmt --all` and create a single follow-up commit "fmt: cargo fmt --all").

- [ ] **Step 4: Manual smoke test**

Run the relay with default config:

```
nix develop --command cargo run -p sunset-relay
```

In another terminal:

```
curl http://127.0.0.1:8443/dashboard
curl http://127.0.0.1:8443/
```

Expected:
- `/dashboard` returns the plaintext status page (matches the previous output format).
- `/` returns `{"ed25519":"…","x25519":"…","address":"ws://127.0.0.1:8443#x25519=…"}`.

Stop with Ctrl-C. Expected: clean shutdown ("received SIGINT, shutting down").

- [ ] **Step 5: Commit any cleanup**

If `cargo fmt --all` produced changes, commit them; otherwise this step is a no-op.

```bash
git add -A
git diff --cached --stat
# If non-empty:
git commit -m "$(cat <<'EOF'
fmt: cargo fmt --all sweep after relay-axum cut-over

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Self-review checklist (run after the plan is written, before execution)

This is a checklist for the plan author (this document) — confirms each spec requirement maps to a task.

| Spec section / requirement | Task(s) |
|---|---|
| Replace byte-peek HTTP classifier with axum | 11, 12, 14 |
| Spawn-per-connection inbound concurrency | 6, 7, 12, 16 |
| `WebSocketRawTransport::serving()` + axum feature | 2, 3, 4 |
| Drop `listening_on` and `external_streams` | 14 |
| `SpawningAcceptor` in `sunset-sync` | 6, 7 |
| Delete `accept_with_timeout` and `accept_handshake_timeout` | 15 |
| `sunset-noise` unchanged | (verified by absence of any task touching it) |
| Two-runtime / runtime topology | 12 (single-runtime + LocalSet equivalent; documented at top) |
| `RelayCommand` + bridge | 8 |
| Snapshot builder (engine-side) | 9 |
| Renderer (axum-side) | 10 |
| axum router builder | 11 |
| Migration of `tests/two_peer_ws_noise.rs` | 5 |
| Migration of `tests/http_index.rs` | 13 |
| New `tests/relay_concurrent_handshakes.rs` | 16 |
| Verify lints + clippy + fmt | 17 |

No placeholders detected. Type names used in later tasks (e.g., `RelayCommand`, `DashboardSnapshot`, `AppState`, `InboundTransport`) are all defined in earlier tasks.
