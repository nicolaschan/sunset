# sunset-sync browser WebSocket transport (Plan E.transport) — Subsystem design

- **Date:** 2026-04-27
- **Status:** Draft (subsystem-level)
- **Scope:** Browser-side `RawTransport` implementation over `web-sys::WebSocket`. Mirror of `sunset-sync-ws-native` (Plan C) but for `wasm32-unknown-unknown` targets. Plan E.transport in the web roadmap (A → C → D → **E.transport** → E).

## Non-negotiable goals

1. **Implements `sunset_sync::RawTransport`** for the browser, identically shaped to `sunset-sync-ws-native::WebSocketRawTransport` so the same `sunset_noise::NoiseTransport<R>` decorator wraps it without modification.
2. **Compiles cleanly to `wasm32-unknown-unknown`.** This is the main acceptance criterion — the production end-to-end test is Plan E's UI integration.
3. **Zero crypto deps.** Same discipline as the native transport: bytes pipe, no Noise / Ed25519 / X25519. `sunset-noise` does the wrapping.
4. **No tokio.** Uses `futures::channel::mpsc` for the receive queue and `wasm-bindgen-futures` to await the WebSocket's `open` event. Tokio's net features are not wasm-compatible and dragging tokio in for sync primitives is overkill in a single-threaded runtime.

## Non-goals (deferred)

- **Inbound `accept`.** Browsers can't bind listeners. `accept()` returns `std::future::pending::<()>().await` per the trait contract for dial-only transports.
- **Reconnection / backoff** — Plan E (UI) handles user-facing retry semantics if needed.
- **`wss://` certificate pinning** — browsers' built-in TLS handles the underlying security; the Noise tunnel above us provides per-peer authentication regardless.
- **WebRTC, WebTransport** — separate plans (W, WT). They implement `RawTransport` the same way.
- **Live integration test that actually opens a WebSocket** — Node's WebSocket support is uneven and faking a server in the wasm test runtime is more code than the test is worth. Real end-to-end validation happens in Plan E (browser ↔ deployed relay over real network).

## Architecture

```
Browser app (Gleam, via Plan A's WASM bridge)
   ↑ uses
SyncEngine (sunset-sync, wasm)
   ↑ takes a Transport
NoiseTransport<R: RawTransport>     (sunset-noise — unchanged from Plan C)
   ↑ decorates
RawTransport
   ↑ implemented by
WebSocketRawTransport                 (NEW: this plan)
   ↑ wraps
web_sys::WebSocket                    (browser-native)
```

The crate has one purpose: shovel bytes between `web_sys::WebSocket` and the trait's async surface, holding the JS-side closures alive for the connection's lifetime.

### Connection lifecycle

```text
WebSocketRawTransport::connect(addr) ─┐
                                      │
        parse PeerAddr URL (drop fragment, that's for Noise above us)
                                      │
                                      ▼
        WebSocket::new(url)        ─┐
        binaryType = "arraybuffer" │
        attach 4 Closures:         │  open / message / error / close
          on_open  → mpsc(open_tx)─┘
          on_message → mpsc(rx_tx)
          on_close → mark closed
          on_error → record error
                                      │
                                      ▼
        await open_rx.recv() OR error_rx.recv()
                                      │
                                      ▼
        return WebSocketRawConnection {
          ws,                       // for send_reliable / close
          rx,                       // for recv_reliable
          _closures,                // hold JS closures alive
        }
```

Send is a synchronous JS call wrapped as an `async fn` that awaits nothing — `WebSocket.send_with_u8_array(&bytes)` returns immediately on success or throws on failure.

Recv awaits the next message from the `mpsc::UnboundedReceiver` populated by the `onmessage` closure. The closure converts the `MessageEvent.data` (an `ArrayBuffer` after we set `binaryType = "arraybuffer"`) to `Bytes`.

Close calls `WebSocket.close()`, which fires the `onclose` closure (which marks the connection closed); pending `recv_reliable` futures see the channel closed and return an error.

### Closure lifecycle (the wasm-bindgen footgun)

`web_sys::WebSocket` callbacks are JS-side functions held by the `WebSocket` itself. From Rust, we attach a `Closure<dyn FnMut(...)>` whose memory must be kept alive while the WebSocket is open — if it drops, the WebSocket fires the callback into freed memory.

The standard idiom: store the `Closure`s as fields on the connection struct so they're alive as long as the connection exists. On `close()` (or `Drop`), the `WebSocket` is closed first, then the closures can be safely dropped.

```rust
pub struct WebSocketRawConnection {
    ws: web_sys::WebSocket,
    rx: RefCell<UnboundedReceiver<Bytes>>,
    _on_message: Closure<dyn FnMut(MessageEvent)>,
    _on_close: Closure<dyn FnMut(CloseEvent)>,
    _on_error: Closure<dyn FnMut(Event)>,
    // _on_open is dropped after the connection is established (no longer needed),
    // unless we want to handle re-open events (we don't in v0).
}
```

`Closure::wrap` produces an owned closure; `closure.as_ref().unchecked_ref::<js_sys::Function>()` gets the JS function reference for `WebSocket::set_onmessage`. The closure's `Drop` removes the JS reference.

### `cfg`-gating

The whole crate is wasm-only. To keep the workspace's `cargo build` / `cargo test --workspace` happy on native development hosts, the lib has two cfg branches:

- `#[cfg(target_arch = "wasm32")]` — the real implementation.
- `#[cfg(not(target_arch = "wasm32"))]` — empty stub: just `pub use` re-exports of placeholder types so the workspace compiles.

The placeholder native types panic if anyone calls them; this is fine because nothing on native should be calling browser-only constructors. The intent is `cargo build --workspace` works on a Linux dev box.

### Send + Sync bounds

Wasm is single-threaded. `WebSocketRawConnection` is `!Send` and `!Sync` — `web_sys::WebSocket` is `!Send`. The `RawTransport` trait is already `#[async_trait(?Send)]` so this fits.

`RefCell<UnboundedReceiver<Bytes>>` (not `Mutex`) is the right primitive for the receive queue — single-threaded; locking is just borrow-check.

### `RawConnection`'s unreliable channel

`send_unreliable` / `recv_unreliable` return `Error::Transport("websocket: unreliable channel unsupported")` — same as the native crate. WebSocket has only reliable framing.

## Components

### `sunset-sync-ws-browser` crate

```toml
[package]
name = "sunset-sync-ws-browser"
version.workspace = true
edition.workspace = true
license.workspace = true
rust-version.workspace = true

[lib]
crate-type = ["cdylib", "rlib"]

[lints]
workspace = true

[dependencies]
async-trait.workspace = true
bytes.workspace = true
futures = { workspace = true, default-features = false, features = ["std", "alloc"] }
sunset-sync.workspace = true
thiserror.workspace = true

# wasm-only; gated by target.
[target.'cfg(target_arch = "wasm32")'.dependencies]
js-sys = "0.3"
wasm-bindgen = "=0.2.117"
wasm-bindgen-futures = "0.4"
web-sys = { version = "0.3", features = [
    "WebSocket",
    "MessageEvent",
    "BinaryType",
    "CloseEvent",
    "Event",
] }

[target.'cfg(target_arch = "wasm32")'.dev-dependencies]
wasm-bindgen-test.workspace = true
```

`futures` workspace dep already exists. `wasm-bindgen` pinned at 0.2.117 (matches Plan A's pin and nixpkgs's `wasm-bindgen-cli`).

### Public surface

```rust
//! Native fallback (placeholder; panics if used).
//! Wasm: real impl.

#[cfg(target_arch = "wasm32")]
mod wasm;
#[cfg(target_arch = "wasm32")]
pub use wasm::{WebSocketRawTransport, WebSocketRawConnection};

#[cfg(not(target_arch = "wasm32"))]
mod stub;
#[cfg(not(target_arch = "wasm32"))]
pub use stub::{WebSocketRawTransport, WebSocketRawConnection};
```

`WebSocketRawTransport`:

```rust
pub struct WebSocketRawTransport;

impl WebSocketRawTransport {
    /// The only constructor — browsers can't accept inbound, so dial-only is
    /// the only mode.
    pub fn dial_only() -> Self;
}

#[async_trait(?Send)]
impl RawTransport for WebSocketRawTransport { /* ... */ }
```

`WebSocketRawConnection` is the post-`connect` value; implements `RawConnection`.

### Native stub

```rust
// stub.rs (compiled when target != wasm32)

pub struct WebSocketRawTransport;
impl WebSocketRawTransport {
    pub fn dial_only() -> Self {
        Self
    }
}
#[async_trait::async_trait(?Send)]
impl sunset_sync::RawTransport for WebSocketRawTransport {
    type Connection = WebSocketRawConnection;
    async fn connect(&self, _: sunset_sync::PeerAddr) -> sunset_sync::Result<Self::Connection> {
        Err(sunset_sync::Error::Transport(
            "sunset-sync-ws-browser: native stub — must be built for wasm32".into(),
        ))
    }
    async fn accept(&self) -> sunset_sync::Result<Self::Connection> {
        std::future::pending::<()>().await;
        unreachable!()
    }
}

pub struct WebSocketRawConnection;
#[async_trait::async_trait(?Send)]
impl sunset_sync::RawConnection for WebSocketRawConnection {
    async fn send_reliable(&self, _: bytes::Bytes) -> sunset_sync::Result<()> {
        Err(sunset_sync::Error::Transport("native stub".into()))
    }
    async fn recv_reliable(&self) -> sunset_sync::Result<bytes::Bytes> {
        Err(sunset_sync::Error::Transport("native stub".into()))
    }
    async fn send_unreliable(&self, _: bytes::Bytes) -> sunset_sync::Result<()> {
        Err(sunset_sync::Error::Transport("native stub".into()))
    }
    async fn recv_unreliable(&self) -> sunset_sync::Result<bytes::Bytes> {
        Err(sunset_sync::Error::Transport("native stub".into()))
    }
    async fn close(&self) -> sunset_sync::Result<()> {
        Ok(())
    }
}
```

Native code that pulls in `sunset-sync-ws-browser` gets these stubs and any actual call returns `Error::Transport` — sufficient because the only consumer (Plan E's wasm bundle) only ever runs in the browser.

## Tests + verification

- `cargo build -p sunset-sync-ws-browser` (native) — succeeds (the stub compiles).
- `cargo build -p sunset-sync-ws-browser --target wasm32-unknown-unknown` — succeeds (the real impl compiles).
- `wasm-pack test --node crates/sunset-sync-ws-browser` — runs a single `wasm_bindgen_test` that calls `WebSocketRawTransport::dial_only()` and asserts the constructor compiles + works. **No actual WebSocket I/O** — Node's WebSocket polyfill story is uneven and the value isn't worth the wrestling. Real end-to-end ws-browser ↔ relay validation happens in Plan E.
- Workspace `cargo test --workspace --all-features` (native) — green; the stub doesn't break anything.
- Workspace `cargo clippy --workspace --all-features --all-targets -- -D warnings` — clean.

## Items deferred

- Browser-side reconnection / exponential backoff (Plan E concern if at all).
- WebSocket `Sec-WebSocket-Protocol` negotiation (out-of-scope; not used by sunset-sync).
- Live-roundtrip wasm-bindgen-test against a localhost WebSocket server (skip — Plan E's UI integration is the real test).
- Browser-side metrics / observability (Plan E concern).

## Self-review checklist

- [x] Four non-negotiables (RawTransport-shaped, wasm32 build, no crypto, no tokio) are met by named mechanisms.
- [x] Closure lifecycle (the wasm-bindgen footgun) is addressed explicitly.
- [x] Native fallback (stub) keeps the workspace buildable without wasm.
- [x] Crate-deps are minimal and use the correct workspace pins.
- [x] PeerAddr scheme matches the native crate (cross-transport reuse of Noise's fragment parsing).
- [x] Test plan honestly notes that real integration testing happens in Plan E.
- [x] Out-of-scope items prevent scope creep.
