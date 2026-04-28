# sunset-sync browser WebSocket transport (Plan E.transport) — Implementation Plan

> **For agentic workers:** Use superpowers:executing-plans (or superpowers:subagent-driven-development) to execute this plan task-by-task.

**Goal:** Land Plan E.transport. Ship `crates/sunset-sync-ws-browser`: a browser-side `RawTransport` over `web-sys::WebSocket` that compiles cleanly to `wasm32-unknown-unknown` and slots into the same `sunset-noise::NoiseTransport<R>` decorator as the native crate from Plan C.

**Spec:** `docs/superpowers/specs/2026-04-27-sunset-sync-ws-browser-design.md`.

**Out of scope:** real end-to-end ws-browser ↔ relay test (Plan E's UI integration); reconnection / backoff logic; WebRTC / WebTransport.

---

## File structure

```
sunset/
├── Cargo.toml                                  # MODIFY: workspace add sunset-sync-ws-browser member + js-sys/web-sys/wasm-bindgen-futures deps
├── crates/
│   └── sunset-sync-ws-browser/                 # NEW
│       ├── Cargo.toml
│       └── src/
│           ├── lib.rs
│           ├── stub.rs                         # native fallback (compiled when target != wasm32)
│           └── wasm.rs                         # real impl (compiled on wasm32)
└── crates/sunset-sync-ws-browser/tests/
    └── construct.rs                            # wasm-bindgen-test: construct dial_only() + check types
```

---

## Tasks

### Task 1: Scaffold the `sunset-sync-ws-browser` crate

**Files:**
- Modify: `Cargo.toml` (workspace root)
- Create: `crates/sunset-sync-ws-browser/Cargo.toml`
- Create: `crates/sunset-sync-ws-browser/src/lib.rs`
- Create: `crates/sunset-sync-ws-browser/src/{stub,wasm}.rs` (placeholders)

- [ ] **Step 1:** Add to root `Cargo.toml`'s `[workspace.dependencies]` (alphabetical):

  ```toml
  js-sys = "0.3"
  wasm-bindgen-futures = "0.4"
  web-sys = { version = "0.3", features = [
    "WebSocket",
    "MessageEvent",
    "BinaryType",
    "CloseEvent",
    "Event",
  ] }
  ```

  (`futures` and `wasm-bindgen` workspace deps already exist from Plans A and C; reuse those pins.)

  Add `crates/sunset-sync-ws-browser` to `[workspace] members`. Don't add a path-dep entry (no other crate consumes it as a Rust dep yet — Plan E will add one).

- [ ] **Step 2:** Create `crates/sunset-sync-ws-browser/Cargo.toml`:

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

  [target.'cfg(target_arch = "wasm32")'.dependencies]
  js-sys.workspace = true
  wasm-bindgen.workspace = true
  wasm-bindgen-futures.workspace = true
  web-sys.workspace = true

  [target.'cfg(target_arch = "wasm32")'.dev-dependencies]
  wasm-bindgen-test.workspace = true
  ```

- [ ] **Step 3:** Create `crates/sunset-sync-ws-browser/src/lib.rs`:

  ```rust
  //! Browser-side `sunset_sync::RawTransport` over `web_sys::WebSocket`.
  //!
  //! Pair with `sunset_noise::NoiseTransport<R>` to get an authenticated
  //! encrypted `Transport` ready for `SyncEngine`.
  //!
  //! See `docs/superpowers/specs/2026-04-27-sunset-sync-ws-browser-design.md`.
  //!
  //! Native (non-wasm) compilation produces stub types that `cargo build` 
  //! happily, but actual calls to `connect` / `send_reliable` etc. return 
  //! `sunset_sync::Error::Transport`. This keeps the workspace buildable 
  //! without wasm tooling while still letting wasm consumers pull the crate 
  //! in directly.

  #[cfg(target_arch = "wasm32")]
  mod wasm;
  #[cfg(target_arch = "wasm32")]
  pub use wasm::{WebSocketRawConnection, WebSocketRawTransport};

  #[cfg(not(target_arch = "wasm32"))]
  mod stub;
  #[cfg(not(target_arch = "wasm32"))]
  pub use stub::{WebSocketRawConnection, WebSocketRawTransport};
  ```

- [ ] **Step 4:** Create the native stub at `crates/sunset-sync-ws-browser/src/stub.rs`:

  ```rust
  //! Native fallback. Compiled on non-wasm targets so the workspace builds
  //! without wasm tooling. Calls return `Error::Transport`.

  use async_trait::async_trait;
  use bytes::Bytes;

  use sunset_sync::{Error, PeerAddr, RawConnection, RawTransport, Result};

  pub struct WebSocketRawTransport;

  impl WebSocketRawTransport {
      pub fn dial_only() -> Self {
          Self
      }
  }

  #[async_trait(?Send)]
  impl RawTransport for WebSocketRawTransport {
      type Connection = WebSocketRawConnection;

      async fn connect(&self, _: PeerAddr) -> Result<Self::Connection> {
          Err(Error::Transport(
              "sunset-sync-ws-browser: native stub — must be built for wasm32".into(),
          ))
      }

      async fn accept(&self) -> Result<Self::Connection> {
          std::future::pending::<()>().await;
          unreachable!();
      }
  }

  pub struct WebSocketRawConnection;

  #[async_trait(?Send)]
  impl RawConnection for WebSocketRawConnection {
      async fn send_reliable(&self, _: Bytes) -> Result<()> {
          Err(Error::Transport("sunset-sync-ws-browser: native stub".into()))
      }
      async fn recv_reliable(&self) -> Result<Bytes> {
          Err(Error::Transport("sunset-sync-ws-browser: native stub".into()))
      }
      async fn send_unreliable(&self, _: Bytes) -> Result<()> {
          Err(Error::Transport("sunset-sync-ws-browser: native stub".into()))
      }
      async fn recv_unreliable(&self) -> Result<Bytes> {
          Err(Error::Transport("sunset-sync-ws-browser: native stub".into()))
      }
      async fn close(&self) -> Result<()> {
          Ok(())
      }
  }
  ```

- [ ] **Step 5:** Create `crates/sunset-sync-ws-browser/src/wasm.rs` (placeholder, populated in Tasks 2 + 3):

  ```rust
  //! Real wasm32 implementation. Populated in Tasks 2 + 3.
  //!
  //! Compiled only on `target_arch = "wasm32"`.

  // Populated below.
  ```

  Then add a minimal type-skeleton so the `pub use wasm::{...}` re-export from lib.rs resolves on wasm builds. Append:

  ```rust
  pub struct WebSocketRawTransport;
  pub struct WebSocketRawConnection;
  ```

  (These get the real implementations in Tasks 2 + 3.)

- [ ] **Step 6:** Verify both build paths:

  ```
  nix develop --command cargo fmt -p sunset-sync-ws-browser
  nix develop --command cargo build -p sunset-sync-ws-browser
  nix develop --command cargo build -p sunset-sync-ws-browser --target wasm32-unknown-unknown
  ```

  Both should succeed. The native build uses the stub; the wasm build uses the (currently empty) wasm.rs.

- [ ] **Step 7:** Commit:
  ```
  git add Cargo.toml Cargo.lock crates/sunset-sync-ws-browser/
  git commit -m "Scaffold sunset-sync-ws-browser crate (native stub + wasm placeholder)"
  ```

---

### Task 2: `WebSocketRawTransport` — connect + dial-only accept

**Files:**
- Modify: `crates/sunset-sync-ws-browser/src/wasm.rs`

This task adds the transport-level concern: parse the PeerAddr, open the WebSocket, await its `open` event, hand off to the (yet-to-be-implemented) `WebSocketRawConnection` constructor.

The `WebSocketRawConnection` definition lives in this file too — Task 3 fills in its `RawConnection` impl. For Task 2, define enough of the connection struct that `connect` can return one.

- [ ] **Step 1:** Replace `crates/sunset-sync-ws-browser/src/wasm.rs` with:

  ```rust
  //! Real wasm32 implementation of `RawTransport` over `web_sys::WebSocket`.

  use std::cell::RefCell;
  use std::rc::Rc;

  use async_trait::async_trait;
  use bytes::Bytes;
  use futures::channel::mpsc::{self, UnboundedReceiver, UnboundedSender};
  use futures::StreamExt;
  use js_sys::{ArrayBuffer, Uint8Array};
  use wasm_bindgen::prelude::*;
  use wasm_bindgen::JsCast;
  use web_sys::{BinaryType, CloseEvent, Event, MessageEvent, WebSocket};

  use sunset_sync::{Error, PeerAddr, RawConnection, RawTransport, Result};

  /// Browser WebSocket transport. Dial-only — browsers can't accept inbound.
  pub struct WebSocketRawTransport;

  impl WebSocketRawTransport {
      /// The only constructor.
      pub fn dial_only() -> Self {
          Self
      }
  }

  #[async_trait(?Send)]
  impl RawTransport for WebSocketRawTransport {
      type Connection = WebSocketRawConnection;

      async fn connect(&self, addr: PeerAddr) -> Result<Self::Connection> {
          let url = parse_addr_url(&addr)?;

          // Construct the WebSocket; throws on bad URL.
          let ws = WebSocket::new(&url)
              .map_err(|e| Error::Transport(format!("ws new: {:?}", e)))?;
          ws.set_binary_type(BinaryType::Arraybuffer);

          // Channels: open, error (one-shot), and message (continuous).
          let (open_tx, mut open_rx) = mpsc::unbounded::<()>();
          let (err_tx, mut err_rx) = mpsc::unbounded::<String>();
          let (msg_tx, msg_rx) = mpsc::unbounded::<Bytes>();
          let (close_tx, mut close_rx) = mpsc::unbounded::<()>();

          // on_open
          let on_open: Closure<dyn FnMut(Event)> = Closure::new({
              let open_tx = open_tx.clone();
              move |_: Event| {
                  let _ = open_tx.unbounded_send(());
              }
          });
          ws.set_onopen(Some(on_open.as_ref().unchecked_ref()));

          // on_message
          let on_message: Closure<dyn FnMut(MessageEvent)> = Closure::new({
              let msg_tx = msg_tx.clone();
              move |event: MessageEvent| {
                  let data = event.data();
                  if let Ok(buffer) = data.dyn_into::<ArrayBuffer>() {
                      let array = Uint8Array::new(&buffer);
                      let mut bytes = vec![0u8; array.length() as usize];
                      array.copy_to(&mut bytes);
                      let _ = msg_tx.unbounded_send(Bytes::from(bytes));
                  }
                  // Non-binary messages are silently dropped — sunset-sync
                  // only sends binary frames, so a text frame is a protocol
                  // error.
              }
          });
          ws.set_onmessage(Some(on_message.as_ref().unchecked_ref()));

          // on_error
          let on_error: Closure<dyn FnMut(Event)> = Closure::new({
              let err_tx = err_tx.clone();
              move |event: Event| {
                  let _ = err_tx.unbounded_send(format!("ws error: {:?}", event));
              }
          });
          ws.set_onerror(Some(on_error.as_ref().unchecked_ref()));

          // on_close
          let on_close: Closure<dyn FnMut(CloseEvent)> = Closure::new({
              let close_tx = close_tx.clone();
              move |_: CloseEvent| {
                  let _ = close_tx.unbounded_send(());
              }
          });
          ws.set_onclose(Some(on_close.as_ref().unchecked_ref()));

          // Wait for open OR error (whichever fires first).
          futures::select! {
              maybe_open = open_rx.next() => {
                  if maybe_open.is_none() {
                      return Err(Error::Transport("ws open channel closed before open".into()));
                  }
              }
              maybe_err = err_rx.next() => {
                  return Err(Error::Transport(
                      maybe_err.unwrap_or_else(|| "ws unknown error".into()),
                  ));
              }
              _ = close_rx.next() => {
                  return Err(Error::Transport("ws closed before open".into()));
              }
          }

          Ok(WebSocketRawConnection {
              ws,
              rx: RefCell::new(msg_rx),
              closed: Rc::new(RefCell::new(false)),
              _on_open: on_open,
              _on_message: on_message,
              _on_error: on_error,
              _on_close: on_close,
          })
      }

      async fn accept(&self) -> Result<Self::Connection> {
          // Browsers can't accept inbound. Return a never-completing future
          // per the trait's documented contract for dial-only transports.
          std::future::pending::<()>().await;
          unreachable!();
      }
  }

  /// Strip the `#x25519=...` fragment that the Noise wrapper above us
  /// consumes; pass the rest to `WebSocket::new()`.
  fn parse_addr_url(addr: &PeerAddr) -> Result<String> {
      let s = std::str::from_utf8(addr.as_bytes())
          .map_err(|e| Error::Transport(format!("addr not utf-8: {e}")))?;
      let no_frag = s.split('#').next().unwrap_or(s);
      Ok(no_frag.to_owned())
  }

  /// Authenticated, encrypted browser WebSocket connection — populated by
  /// Task 3 with the `RawConnection` impl.
  pub struct WebSocketRawConnection {
      pub(crate) ws: WebSocket,
      pub(crate) rx: RefCell<UnboundedReceiver<Bytes>>,
      pub(crate) closed: Rc<RefCell<bool>>,

      // Hold JS-side closures alive while the WebSocket exists. Dropping
      // these while `ws` is still receiving callbacks would cause UB.
      pub(crate) _on_open: Closure<dyn FnMut(Event)>,
      pub(crate) _on_message: Closure<dyn FnMut(MessageEvent)>,
      pub(crate) _on_error: Closure<dyn FnMut(Event)>,
      pub(crate) _on_close: Closure<dyn FnMut(CloseEvent)>,
  }
  ```

- [ ] **Step 2:** Verify the wasm build:
  ```
  nix develop --command cargo fmt -p sunset-sync-ws-browser
  nix develop --command cargo build -p sunset-sync-ws-browser --target wasm32-unknown-unknown
  ```

  Native build will likely warn about unused variants in the wasm-only re-export but should still succeed since the cfg-gated code isn't compiled there.

- [ ] **Step 3:** Note: clippy may flag missing `RawConnection` impl on `WebSocketRawConnection` (Task 3 supplies it). Skip clippy in this task; run it after Task 3.

- [ ] **Step 4:** Commit:
  ```
  git add crates/sunset-sync-ws-browser/src/wasm.rs
  git commit -m "Add WebSocketRawTransport: web-sys WebSocket connect + accept-pending"
  ```

---

### Task 3: `WebSocketRawConnection` — `RawConnection` impl

**Files:**
- Modify: `crates/sunset-sync-ws-browser/src/wasm.rs`

- [ ] **Step 1:** Append the `RawConnection` impl to `wasm.rs` (after the existing struct definition):

  ```rust
  #[async_trait(?Send)]
  impl RawConnection for WebSocketRawConnection {
      async fn send_reliable(&self, bytes: Bytes) -> Result<()> {
          if *self.closed.borrow() {
              return Err(Error::Transport("ws closed".into()));
          }
          self.ws
              .send_with_u8_array(&bytes)
              .map_err(|e| Error::Transport(format!("ws send: {:?}", e)))
      }

      async fn recv_reliable(&self) -> Result<Bytes> {
          let mut rx = self.rx.borrow_mut();
          rx.next()
              .await
              .ok_or_else(|| Error::Transport("ws closed".into()))
      }

      async fn send_unreliable(&self, _: Bytes) -> Result<()> {
          Err(Error::Transport(
              "websocket: unreliable channel unsupported".into(),
          ))
      }

      async fn recv_unreliable(&self) -> Result<Bytes> {
          Err(Error::Transport(
              "websocket: unreliable channel unsupported".into(),
          ))
      }

      async fn close(&self) -> Result<()> {
          if *self.closed.borrow() {
              return Ok(());
          }
          *self.closed.borrow_mut() = true;
          self.ws
              .close()
              .map_err(|e| Error::Transport(format!("ws close: {:?}", e)))
      }
  }

  // The on_close closure should also flip `closed` to true so a peer-initiated
  // close is observable to subsequent `send_reliable` calls. We reuse the
  // existing closure setup in `connect` — see the `closed` clone passed in
  // via Rc<RefCell<bool>>. (If you find that the connect() closures aren't
  // wired to update this flag, add a small helper that's called from the
  // on_close closure and references a clone of self.closed.)
  ```

  **Note on the close-flag wiring:** The `closed: Rc<RefCell<bool>>` field needs the `on_close` closure to flip it. Look at the `on_close` closure in `connect()` from Task 2 — extend it to take a clone of the `closed: Rc<RefCell<bool>>` and set it. Specifically, in the `on_close` closure body:

  ```rust
  let closed_for_on_close = closed.clone();   // Rc<RefCell<bool>> created above
  let on_close: Closure<dyn FnMut(CloseEvent)> = Closure::new({
      let close_tx = close_tx.clone();
      move |_: CloseEvent| {
          *closed_for_on_close.borrow_mut() = true;
          let _ = close_tx.unbounded_send(());
      }
  });
  ```

  Adjust Task 2's `connect()` body to construct `closed: Rc<RefCell<bool>> = Rc::new(RefCell::new(false))` BEFORE the closures, then `closed.clone()` into the on_close closure, and `closed` (the original) into the returned struct.

- [ ] **Step 2:** Verify:
  ```
  nix develop --command cargo fmt -p sunset-sync-ws-browser
  nix develop --command cargo build -p sunset-sync-ws-browser --target wasm32-unknown-unknown
  nix develop --command cargo build -p sunset-sync-ws-browser
  nix develop --command cargo clippy -p sunset-sync-ws-browser --all-targets --target wasm32-unknown-unknown -- -D warnings
  ```

  All clean. Note: native clippy targets the stub; wasm clippy targets the real impl.

- [ ] **Step 3:** Commit:
  ```
  git add crates/sunset-sync-ws-browser/src/wasm.rs
  git commit -m "Add WebSocketRawConnection: send/recv/close + closure lifecycle"
  ```

---

### Task 4: wasm-bindgen-test (compile + construct)

**Files:**
- Create: `crates/sunset-sync-ws-browser/tests/construct.rs`

- [ ] **Step 1:** Create the integration test:

  ```rust
  //! Compile + construct check. Real WebSocket I/O is exercised in Plan E's
  //! browser-side UI integration; this test only confirms the crate
  //! compiles for the wasm32 target and the constructor produces a value
  //! whose types fit the trait surface.

  #![cfg(target_arch = "wasm32")]

  use sunset_sync::RawTransport;
  use sunset_sync_ws_browser::WebSocketRawTransport;
  use wasm_bindgen_test::*;

  wasm_bindgen_test_configure!(run_in_node_experimental);

  #[wasm_bindgen_test]
  fn dial_only_constructs() {
      let t = WebSocketRawTransport::dial_only();
      // Confirm the trait is implemented (compile-time check; takes ownership).
      let _: &dyn TraitMarker = &t;
  }

  // Marker trait: `RawTransport` has an associated type, so `dyn RawTransport`
  // isn't directly usable as a trait object without specifying it. This
  // marker lets us check via dyn dispatch that the trait is implemented at
  // all.
  trait TraitMarker {}
  impl<T: RawTransport> TraitMarker for T {}
  ```

  (The marker-trait pattern dodges `RawTransport`'s associated `Connection` type, which would otherwise prevent `&dyn RawTransport`.)

- [ ] **Step 2:** Run:
  ```
  cd /home/nicolas/src/sunset/.worktrees/sunset-sync-ws-browser   # adjust to actual worktree
  nix develop --command bash -c 'cd crates/sunset-sync-ws-browser && wasm-pack test --node'
  ```

  Expect 1 passed.

- [ ] **Step 3:** Commit:
  ```
  git add crates/sunset-sync-ws-browser/tests/construct.rs
  git commit -m "Add wasm-bindgen-test: dial_only constructs and impls RawTransport"
  ```

---

### Task 5: Final pass

- [ ] **Step 1:** Workspace-wide checks:
  ```
  nix develop --command cargo fmt --all --check
  nix develop --command cargo clippy --workspace --all-features --all-targets -- -D warnings
  nix develop --command cargo test --workspace --all-features
  ```
  All clean / green.

- [ ] **Step 2:** Wasm builds for everything that should still build for wasm:
  ```
  nix develop --command cargo build -p sunset-noise --target wasm32-unknown-unknown --lib
  nix develop --command cargo build -p sunset-core --target wasm32-unknown-unknown --lib
  nix develop --command cargo build -p sunset-sync-ws-browser --target wasm32-unknown-unknown --lib
  ```

- [ ] **Step 3:** Plan A's flake artifact:
  ```
  nix build .#sunset-core-wasm --no-link
  ```

- [ ] **Step 4:** Plan D's flake artifacts:
  ```
  nix build .#sunset-relay --no-link
  nix build .#sunset-relay-docker --no-link
  ```

- [ ] **Step 5:** wasm-pack the new test:
  ```
  nix develop --command bash -c 'cd crates/sunset-sync-ws-browser && wasm-pack test --node'
  ```

- [ ] **Step 6:** If any cleanup commits were needed, commit:
  ```
  git add -u
  git commit -m "Final fmt + clippy pass"
  ```

---

## Verification (end-state acceptance)

After all 5 tasks land:

- `cargo fmt --all --check` clean.
- `cargo clippy --workspace --all-features --all-targets -- -D warnings` clean.
- `cargo test --workspace --all-features` green.
- `cargo build -p sunset-sync-ws-browser` (native) clean — uses the stub.
- `cargo build -p sunset-sync-ws-browser --target wasm32-unknown-unknown` clean — uses the real impl.
- `wasm-pack test --node crates/sunset-sync-ws-browser` — 1 passed.
- All prior nix builds (`sunset-core-wasm`, `sunset-relay`, `sunset-relay-docker`) still succeed.
- `git log --oneline master..HEAD` — roughly 5 task-by-task commits.

---

## What this unlocks

After Plan E.transport, **Plan E** can:

1. Build a `sunset-relay-client-wasm` (or similar) crate that bundles `sunset-store-memory` + `sunset-sync` + `sunset-sync-ws-browser` + `sunset-noise` + `sunset-core` into a single wasm artifact with a JS-callable surface (parallel to Plan A's `sunset-core-wasm` but adding the engine).
2. Wire that artifact into the Gleam UI, replacing fixture-data calls with real `compose_message` → engine.insert → relay roundtrip → engine receives → decode → render.
3. Two browsers connected to the same deployed relay can exchange messages.

That's the v0 web demo.
