# sunset-sync-webrtc-browser Unreliable Datachannel Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `WebRtcRawConnection::send_unreliable` / `recv_unreliable` actually move bytes between two browsers via a second SCTP datachannel, so `Bus::publish_ephemeral` works end-to-end on the web client.

**Architecture:** Each `RtcPeerConnection` carries two named `RtcDataChannel`s — `sunset-sync` (existing reliable, unchanged) and `sunset-sync-unrel` (new, `ordered: false` + `maxRetransmits: 0`). Both `connect()` and `accept()` wait until both channels report `open` before returning. `send_unreliable`/`recv_unreliable` mirror the reliable implementations but route through the new channel.

**Tech Stack:** Rust + `wasm32-unknown-unknown`, `web-sys` for `RtcDataChannel` / `RtcDataChannelInit`, `wasm-bindgen-futures` for `JsFuture`, `futures::channel::mpsc` + `oneshot` for in-tab plumbing.

**Spec:** `docs/superpowers/specs/2026-04-28-sunset-webrtc-unreliable-design.md`

---

## File Structure

Single source file modified:

- **`crates/sunset-sync-webrtc-browser/src/wasm.rs`** — adds 4 fields to `WebRtcRawConnection`, parallel datachannel creation in `connect`, label-dispatching `ondatachannel` in `run_accept_one`, real implementations of `send_unreliable` / `recv_unreliable`.

Unchanged:

- **`crates/sunset-sync-webrtc-browser/src/lib.rs`** — public re-exports unchanged.
- **`crates/sunset-sync-webrtc-browser/src/stub.rs`** — non-wasm stub; `RawConnection` trait surface is unchanged so this needs no edits.
- **`crates/sunset-sync-webrtc-browser/tests/construct.rs`** — only checks the trait surface compiles; trait surface unchanged.
- **`crates/sunset-sync-webrtc-browser/Cargo.toml`** — no new dependencies.
- **`web/e2e/kill_relay.spec.js`** — used as a regression check (not modified).

## Verification Strategy

Per the spec, **byte-flow validation is deferred to Plan C** (voice). This plan's verification is:

1. **Static**: `cargo build --target wasm32-unknown-unknown -p sunset-sync-webrtc-browser`, `cargo clippy --target wasm32-unknown-unknown -p sunset-sync-webrtc-browser --all-targets -- -D warnings`, `cargo fmt --check`.
2. **Existing trait-surface check**: the `webrtc_transport_constructs` test in `tests/construct.rs` continues to pass on wasm32 / node-experimental.
3. **Reliable-channel regression**: `web/e2e/kill_relay.spec.js` continues to pass — this is the only test that exercises real WebRTC I/O today, and it proves the reliable channel still negotiates and carries chat traffic end-to-end after the second datachannel is added.

The unreliable channel's structural correctness (correct `RtcDataChannelInit` flags, label dispatch, both-open synchronization) is verified by code review; the byte-flow path will be exercised by Plan C's voice tests once they exist.

---

## Task 1: Add unreliable channel on the connect side

**Files:**
- Modify: `crates/sunset-sync-webrtc-browser/src/wasm.rs:152-257` (the `RawTransport::connect` impl)
- Modify: `crates/sunset-sync-webrtc-browser/src/wasm.rs:438-450` (the `WebRtcRawConnection` struct definition)

**Why this task:** Introduces the new fields on `WebRtcRawConnection` and produces them from the connect side. Accept side is updated in Task 2; until then, `run_accept_one` still constructs the struct, so we need to give it placeholder initializers in this task that Task 2 then replaces with real wiring. The crate must continue to compile after every task.

- [ ] **Step 1: Add the new fields to `WebRtcRawConnection`**

In `crates/sunset-sync-webrtc-browser/src/wasm.rs`, replace the existing struct definition (around line 438-450):

```rust
pub struct WebRtcRawConnection {
    _pc: RtcPeerConnection,
    dc: RtcDataChannel,
    rx: RefCell<mpsc::UnboundedReceiver<Bytes>>,
    /// Second datachannel: `ordered: false`, `maxRetransmits: 0`.
    /// Used by `send_unreliable` / `recv_unreliable` for ephemeral
    /// (e.g. voice) traffic.
    dc_unrel: RtcDataChannel,
    rx_unrel: RefCell<mpsc::UnboundedReceiver<Bytes>>,
    _on_ice: Closure<dyn FnMut(RtcPeerConnectionIceEvent)>,
    /// Connect side keeps these on the connection. Accept side leaks them
    /// inside the `ondatachannel` handler (page lifetime), so these are
    /// `None` on the accept side.
    _on_open: Option<Closure<dyn FnMut(JsValue)>>,
    _on_msg: Option<Closure<dyn FnMut(MessageEvent)>>,
    /// Connect side keeps these on the connection (mirrors `_on_open` /
    /// `_on_msg` for the unreliable channel). `None` on the accept side.
    _on_open_unrel: Option<Closure<dyn FnMut(JsValue)>>,
    _on_msg_unrel: Option<Closure<dyn FnMut(MessageEvent)>>,
    /// Only set on the accept side.
    _on_dc: Option<Closure<dyn FnMut(RtcDataChannelEvent)>>,
}
```

- [ ] **Step 2: Wire the unreliable channel in `connect`**

In the `connect` method, replace the block that creates the single datachannel + sets up its closures (currently lines 158-172) with a block that creates both channels in parallel:

```rust
        let pc = build_peer_connection(&self.ice_urls)?;

        // Reliable channel (existing behaviour, unchanged on the wire).
        let dc_init = RtcDataChannelInit::new();
        let dc = pc.create_data_channel_with_data_channel_dict("sunset-sync", &dc_init);
        dc.set_binary_type(RtcDataChannelType::Arraybuffer);

        // Unreliable channel: unordered + zero retransmits. SCTP will
        // chunk/reassemble each `send` but won't queue retransmissions
        // and won't enforce ordering across messages.
        let dc_unrel_init = RtcDataChannelInit::new();
        dc_unrel_init.set_ordered(false);
        dc_unrel_init.set_max_retransmits(0);
        let dc_unrel =
            pc.create_data_channel_with_data_channel_dict("sunset-sync-unrel", &dc_unrel_init);
        dc_unrel.set_binary_type(RtcDataChannelType::Arraybuffer);

        let (ice_tx, ice_rx) = mpsc::unbounded::<String>();
        let (open_tx, open_rx) = oneshot::channel::<()>();
        let (open_tx_unrel, open_rx_unrel) = oneshot::channel::<()>();
        let (msg_tx, msg_rx) = mpsc::unbounded::<Bytes>();
        let (msg_tx_unrel, msg_rx_unrel) = mpsc::unbounded::<Bytes>();

        let on_ice = make_ice_closure(ice_tx);
        pc.set_onicecandidate(Some(on_ice.as_ref().unchecked_ref()));

        let on_open = make_open_closure(open_tx);
        dc.set_onopen(Some(on_open.as_ref().unchecked_ref()));
        let on_msg = make_msg_closure(msg_tx);
        dc.set_onmessage(Some(on_msg.as_ref().unchecked_ref()));

        let on_open_unrel = make_open_closure(open_tx_unrel);
        dc_unrel.set_onopen(Some(on_open_unrel.as_ref().unchecked_ref()));
        let on_msg_unrel = make_msg_closure(msg_tx_unrel);
        dc_unrel.set_onmessage(Some(on_msg_unrel.as_ref().unchecked_ref()));
```

- [ ] **Step 3: Wait for both channels to open in the `select!` loop**

Still inside `connect`, the existing loop (currently around lines 208-244) waits for the single open future. Replace the loop body to track both opens and break only when both have fired. Replace the block from `let mut got_answer = false;` through the `}` that closes the `loop {}`:

```rust
        let mut got_answer = false;
        let mut pending_ice: Vec<String> = Vec::new();
        let mut opened_rel = false;
        let mut opened_unrel = false;
        let open_fut = open_rx.fuse();
        let open_fut_unrel = open_rx_unrel.fuse();
        futures::pin_mut!(open_fut, open_fut_unrel);
        loop {
            futures::select! {
                _ = open_fut.as_mut() => {
                    opened_rel = true;
                    if opened_rel && opened_unrel { break; }
                }
                _ = open_fut_unrel.as_mut() => {
                    opened_unrel = true;
                    if opened_rel && opened_unrel { break; }
                }
                opt = peer_in_rx.next().fuse() => {
                    let kind = opt.ok_or_else(|| {
                        Error::Transport("signaling closed before open".into())
                    })?;
                    match kind {
                        WebRtcSignalKind::Answer(sdp) if !got_answer => {
                            let sd = RtcSessionDescriptionInit::new(RtcSdpType::Answer);
                            sd.set_sdp(&sdp);
                            JsFuture::from(pc.set_remote_description(&sd)).await.map_err(|e| {
                                Error::Transport(format!("setRemoteDescription: {e:?}"))
                            })?;
                            got_answer = true;
                            for json in pending_ice.drain(..) {
                                add_remote_ice(&pc, &json).await?;
                            }
                        }
                        WebRtcSignalKind::IceCandidate(json) => {
                            if got_answer {
                                add_remote_ice(&pc, &json).await?;
                            } else {
                                pending_ice.push(json);
                            }
                        }
                        WebRtcSignalKind::Offer(_) | WebRtcSignalKind::Answer(_) => {
                            // Glare or duplicate — ignore.
                        }
                    }
                }
            }
        }
```

- [ ] **Step 4: Construct the new struct shape from `connect`**

Still inside `connect`, replace the existing `Ok(WebRtcRawConnection { … })` block (currently around lines 248-256) with one that fills in the new fields:

```rust
        Ok(WebRtcRawConnection {
            _pc: pc,
            dc,
            rx: RefCell::new(msg_rx),
            dc_unrel,
            rx_unrel: RefCell::new(msg_rx_unrel),
            _on_ice: on_ice,
            _on_open: Some(on_open),
            _on_msg: Some(on_msg),
            _on_open_unrel: Some(on_open_unrel),
            _on_msg_unrel: Some(on_msg_unrel),
            _on_dc: None,
        })
```

- [ ] **Step 5: Add placeholder initializers in `run_accept_one` so the crate still compiles**

The accept side will be wired in Task 2. For now, give `run_accept_one` placeholders for the new fields so the code compiles. Find the `Ok(WebRtcRawConnection { … })` block at the end of `run_accept_one` (currently around lines 427-435) and replace it with:

```rust
    // TASK-2 PLACEHOLDER: dc_unrel and rx_unrel are wired in Task 2 of
    // the unreliable-datachannel plan. For now we clone the reliable
    // channel handle into the unreliable slot and create an empty mpsc
    // so the crate compiles. send_unreliable / recv_unreliable still
    // return the v1 stub error during Task 1.
    let (_msg_tx_unrel_placeholder, msg_rx_unrel_placeholder) = mpsc::unbounded::<Bytes>();
    Ok(WebRtcRawConnection {
        _pc: pc,
        dc: dc.clone(),
        rx: RefCell::new(msg_rx),
        dc_unrel: dc,
        rx_unrel: RefCell::new(msg_rx_unrel_placeholder),
        _on_ice: on_ice,
        _on_open: None,
        _on_msg: None,
        _on_open_unrel: None,
        _on_msg_unrel: None,
        _on_dc: Some(on_dc),
    })
```

(Note: `RtcDataChannel` derives `Clone` via `wasm_bindgen` since it wraps a `JsValue`. The clone here is a placeholder that Task 2 deletes.)

- [ ] **Step 6: Build for wasm32 to verify compilation**

Run:

```bash
nix develop --command cargo build --target wasm32-unknown-unknown -p sunset-sync-webrtc-browser
```

Expected: `Finished` with no errors. Warnings about unused fields/closures during this transitional state are acceptable; they will be resolved by Tasks 2 and 3.

- [ ] **Step 7: Run the construct test to verify the trait surface still works**

Run:

```bash
nix develop --command cargo test --target wasm32-unknown-unknown -p sunset-sync-webrtc-browser --test construct
```

Expected: `webrtc_transport_constructs ... ok`. The test only checks the constructor + trait impl — it does not run a real handshake.

- [ ] **Step 8: Commit**

```bash
git add crates/sunset-sync-webrtc-browser/src/wasm.rs
git commit -m "Add unreliable datachannel on WebRTC connect side

Adds dc_unrel + rx_unrel + open/msg closures to WebRtcRawConnection,
and the parallel-create-and-wait-for-both-open behaviour on the connect
side. Accept side wiring follows in Task 2; for now run_accept_one
constructs the struct with placeholder values so the crate continues
to compile."
```

---

## Task 2: Dispatch by label on the accept side

**Files:**
- Modify: `crates/sunset-sync-webrtc-browser/src/wasm.rs:321-436` (the `run_accept_one` free function)

**Why this task:** Accept side currently has a single `ondatachannel` handler and a single open oneshot. We need to dispatch by `dc.label()` so each peer gets both channels, and we need to wait for both before returning.

- [ ] **Step 1: Add the unreliable plumbing variables before `on_dc`**

In `run_accept_one`, immediately after the existing line `let (dc_tx, dc_rx) = oneshot::channel::<RtcDataChannel>();` (currently around line 339), add the second set:

```rust
    let (dc_tx_unrel, dc_rx_unrel) = oneshot::channel::<RtcDataChannel>();
    let (open_tx_unrel, open_rx_unrel) = oneshot::channel::<()>();
    let (msg_tx_unrel, msg_rx_unrel) = mpsc::unbounded::<Bytes>();
```

- [ ] **Step 2: Replace `on_dc` with the label-dispatching version**

Replace the existing `let on_dc = Closure::<dyn FnMut(RtcDataChannelEvent)>::new(…)` block (currently around lines 347-362) with:

```rust
    let dc_tx_cell = Rc::new(RefCell::new(Some(dc_tx)));
    let open_tx_cell = Rc::new(RefCell::new(Some(open_tx)));
    let dc_tx_unrel_cell = Rc::new(RefCell::new(Some(dc_tx_unrel)));
    let open_tx_unrel_cell = Rc::new(RefCell::new(Some(open_tx_unrel)));
    let msg_tx_for_dc = msg_tx;
    let msg_tx_for_dc_unrel = msg_tx_unrel;
    let on_dc = Closure::<dyn FnMut(RtcDataChannelEvent)>::new(move |ev: RtcDataChannelEvent| {
        let dc = ev.channel();
        dc.set_binary_type(RtcDataChannelType::Arraybuffer);
        match dc.label().as_str() {
            "sunset-sync" => {
                let on_open = make_open_closure_from_cell(open_tx_cell.clone());
                dc.set_onopen(Some(on_open.as_ref().unchecked_ref()));
                on_open.forget();

                let on_msg = make_msg_closure(msg_tx_for_dc.clone());
                dc.set_onmessage(Some(on_msg.as_ref().unchecked_ref()));
                on_msg.forget();

                if let Some(tx) = dc_tx_cell.borrow_mut().take() {
                    let _ = tx.send(dc);
                }
            }
            "sunset-sync-unrel" => {
                let on_open = make_open_closure_from_cell(open_tx_unrel_cell.clone());
                dc.set_onopen(Some(on_open.as_ref().unchecked_ref()));
                on_open.forget();

                let on_msg = make_msg_closure(msg_tx_for_dc_unrel.clone());
                dc.set_onmessage(Some(on_msg.as_ref().unchecked_ref()));
                on_msg.forget();

                if let Some(tx) = dc_tx_unrel_cell.borrow_mut().take() {
                    let _ = tx.send(dc);
                }
            }
            other => {
                // Unknown label: ignore. Future protocol versions may
                // add channels; we don't want a typo in a peer's build
                // to break the handshake here.
                web_sys::console::warn_1(
                    &format!("sunset-sync: ignoring unknown datachannel label '{}'", other).into(),
                );
            }
        }
    });
    pc.set_ondatachannel(Some(on_dc.as_ref().unchecked_ref()));
```

- [ ] **Step 3: Replace the wait loop to track both channels**

Replace the existing wait-for-dc-and-open loop (currently around lines 397-422 — the block beginning `let dc_fut = dc_rx.fuse();` and ending at the matching `}` after the loop) with:

```rust
    let dc_fut = dc_rx.fuse();
    let dc_fut_unrel = dc_rx_unrel.fuse();
    let open_fut = open_rx.fuse();
    let open_fut_unrel = open_rx_unrel.fuse();
    futures::pin_mut!(dc_fut, dc_fut_unrel, open_fut, open_fut_unrel);
    let mut dc_opt: Option<RtcDataChannel> = None;
    let mut dc_opt_unrel: Option<RtcDataChannel> = None;
    let mut opened_rel = false;
    let mut opened_unrel = false;
    loop {
        futures::select! {
            got = dc_fut.as_mut() => {
                dc_opt = Some(got.map_err(|_| {
                    Error::Transport("peer connection dropped before reliable ondatachannel".into())
                })?);
            }
            got = dc_fut_unrel.as_mut() => {
                dc_opt_unrel = Some(got.map_err(|_| {
                    Error::Transport("peer connection dropped before unreliable ondatachannel".into())
                })?);
            }
            _ = open_fut.as_mut() => {
                opened_rel = true;
            }
            _ = open_fut_unrel.as_mut() => {
                opened_unrel = true;
            }
            opt = peer_in_rx.next().fuse() => {
                let kind = opt.ok_or_else(|| {
                    Error::Transport("signaling closed mid-handshake".into())
                })?;
                if let WebRtcSignalKind::IceCandidate(json) = kind {
                    add_remote_ice(&pc, &json).await?;
                }
            }
        }
        if dc_opt.is_some() && dc_opt_unrel.is_some() && opened_rel && opened_unrel {
            break;
        }
    }
```

- [ ] **Step 4: Replace the placeholder `Ok(WebRtcRawConnection { … })` with real wiring**

Replace the placeholder block added in Task 1 Step 5 (the block beginning with the `// TASK-2 PLACEHOLDER` comment and including the `Ok(WebRtcRawConnection { … })`) with:

```rust
    inner.borrow_mut().per_peer.remove(&from_peer);

    let dc = dc_opt.ok_or_else(|| Error::Transport("no inbound reliable datachannel".into()))?;
    let dc_unrel =
        dc_opt_unrel.ok_or_else(|| Error::Transport("no inbound unreliable datachannel".into()))?;
    Ok(WebRtcRawConnection {
        _pc: pc,
        dc,
        rx: RefCell::new(msg_rx),
        dc_unrel,
        rx_unrel: RefCell::new(msg_rx_unrel),
        _on_ice: on_ice,
        _on_open: None,
        _on_msg: None,
        _on_open_unrel: None,
        _on_msg_unrel: None,
        _on_dc: Some(on_dc),
    })
```

(Note: there is one existing line `inner.borrow_mut().per_peer.remove(&from_peer);` immediately before the original `Ok(...)` in the file. The replacement above includes that same line; if your editor leaves a duplicate, delete the duplicate.)

- [ ] **Step 5: Build for wasm32 to verify compilation**

```bash
nix develop --command cargo build --target wasm32-unknown-unknown -p sunset-sync-webrtc-browser
```

Expected: `Finished` with no errors and no warnings about unused channels/closures. The struct fields are now all consumed.

- [ ] **Step 6: Run the construct test**

```bash
nix develop --command cargo test --target wasm32-unknown-unknown -p sunset-sync-webrtc-browser --test construct
```

Expected: `webrtc_transport_constructs ... ok`.

- [ ] **Step 7: Commit**

```bash
git add crates/sunset-sync-webrtc-browser/src/wasm.rs
git commit -m "Wire unreliable datachannel on WebRTC accept side

ondatachannel now dispatches by RtcDataChannel.label() — sunset-sync
goes to the reliable slot, sunset-sync-unrel goes to the unreliable
slot, anything else logs and is dropped. The accept-side wait loop
now blocks until both channels have been delivered AND both have
fired open."
```

---

## Task 3: Implement `send_unreliable` / `recv_unreliable`

**Files:**
- Modify: `crates/sunset-sync-webrtc-browser/src/wasm.rs:472-482` (the two stub methods on `RawConnection`)

**Why this task:** With both channels now wired through the connection, replace the stub error returns with real implementations that mirror `send_reliable` / `recv_reliable` but route through `dc_unrel` / `rx_unrel`.

- [ ] **Step 1: Replace the two stub method bodies**

In `crates/sunset-sync-webrtc-browser/src/wasm.rs`, replace:

```rust
    async fn send_unreliable(&self, _: Bytes) -> Result<()> {
        Err(Error::Transport(
            "webrtc: unreliable channel not implemented in v1".into(),
        ))
    }

    async fn recv_unreliable(&self) -> Result<Bytes> {
        Err(Error::Transport(
            "webrtc: unreliable channel not implemented in v1".into(),
        ))
    }
```

with:

```rust
    async fn send_unreliable(&self, bytes: Bytes) -> Result<()> {
        self.dc_unrel
            .send_with_u8_array(&bytes)
            .map_err(|e| Error::Transport(format!("dc_unrel.send: {e:?}")))
    }

    async fn recv_unreliable(&self) -> Result<Bytes> {
        poll_fn(|cx| {
            let mut rx = self.rx_unrel.borrow_mut();
            match Stream::poll_next(Pin::new(&mut *rx), cx) {
                Poll::Ready(Some(b)) => Poll::Ready(Ok(b)),
                Poll::Ready(None) => Poll::Ready(Err(Error::Transport("dc_unrel closed".into()))),
                Poll::Pending => Poll::Pending,
            }
        })
        .await
    }
```

- [ ] **Step 2: Build for wasm32**

```bash
nix develop --command cargo build --target wasm32-unknown-unknown -p sunset-sync-webrtc-browser
```

Expected: `Finished` with no errors and no warnings.

- [ ] **Step 3: Run the construct test**

```bash
nix develop --command cargo test --target wasm32-unknown-unknown -p sunset-sync-webrtc-browser --test construct
```

Expected: `webrtc_transport_constructs ... ok`.

- [ ] **Step 4: Commit**

```bash
git add crates/sunset-sync-webrtc-browser/src/wasm.rs
git commit -m "Implement send_unreliable/recv_unreliable on browser WebRTC

Replace the v1 'not implemented' stubs with real implementations
mirroring send_reliable/recv_reliable but routed through dc_unrel
and rx_unrel. Bus::publish_ephemeral now flows end-to-end through
the browser transport."
```

---

## Task 4: Lint, format, and host-target build

**Files:** No code changes; this task verifies the workspace is clean.

**Why this task:** Catch clippy regressions, formatting drift, and any non-wasm32 build breakage (the crate has a non-wasm `stub.rs` that must continue compiling on the host target — used by the workspace's host-target tests).

- [ ] **Step 1: Run clippy on the wasm target**

```bash
nix develop --command cargo clippy --target wasm32-unknown-unknown -p sunset-sync-webrtc-browser --all-targets -- -D warnings
```

Expected: no warnings, exit 0. If clippy complains about an unused variable, an unused `_` binding pattern, or a redundant clone, fix it inline before continuing.

- [ ] **Step 2: Run clippy on the host target (covers `stub.rs`)**

```bash
nix develop --command cargo clippy -p sunset-sync-webrtc-browser --all-targets -- -D warnings
```

Expected: no warnings, exit 0. The `stub.rs` path is unchanged but the workspace lints (`unused_must_use = deny`) still apply.

- [ ] **Step 3: Run cargo fmt check**

```bash
nix develop --command cargo fmt --all --check
```

Expected: exit 0 (no formatting differences). If this fails, run `nix develop --command cargo fmt --all` and commit the formatting changes as a separate commit (per the project rule "create new commits, don't amend").

- [ ] **Step 4: Run the full workspace build for wasm32**

```bash
nix develop --command cargo build --target wasm32-unknown-unknown -p sunset-web-wasm
```

Expected: `Finished` with no errors. This proves the change is consumable by the actual web client.

- [ ] **Step 5: Commit if any fmt drift was applied**

If Step 3 produced any changes, commit them:

```bash
git add -u
git commit -m "fmt: apply rustfmt after unreliable-datachannel wiring"
```

If no changes, skip the commit.

---

## Task 5: Reliable-channel regression check via Playwright `kill_relay`

**Files:** No code changes. Runs the existing end-to-end test that exercises real WebRTC.

**Why this task:** This is the only test in the repo that drives a real `RtcPeerConnection`. If adding the second channel has broken the reliable handshake (e.g. because `select!` ordering got subtly wrong, or the both-open wait deadlocks, or the label dispatch dropped traffic), this test will catch it.

- [ ] **Step 1: Build the relay binary**

```bash
nix develop --command cargo build -p sunset-relay --release
```

Expected: `Finished` with no errors. The Playwright test spawns `sunset-relay` from PATH; the build ensures it's available in `target/release/`.

Then add it to PATH for the test session (or use `nix run`):

```bash
export PATH="$PWD/target/release:$PATH"
```

- [ ] **Step 2: Build the wasm bundle the web client loads**

```bash
nix develop --command cargo build --target wasm32-unknown-unknown -p sunset-web-wasm --release
```

Then run wasm-bindgen to produce the JS shim consumed by the Gleam app:

```bash
nix develop --command bash -c '
  mkdir -p web/build/dev/javascript &&
  wasm-bindgen \
    --target web \
    --no-typescript \
    --out-dir web/build/dev/javascript \
    --out-name sunset_web_wasm \
    target/wasm32-unknown-unknown/release/sunset_web_wasm.wasm
'
```

Expected: Two files produced — `web/build/dev/javascript/sunset_web_wasm.js` and `web/build/dev/javascript/sunset_web_wasm_bg.wasm`.

- [ ] **Step 3: Run the kill_relay Playwright test**

```bash
nix develop --command bash -c 'cd web && bun install --frozen-lockfile && npx playwright test e2e/kill_relay.spec.js'
```

Expected: 1 test passing. The test:

1. Starts a relay subprocess.
2. Opens two browser contexts.
3. Verifies relay-mediated chat works.
4. Calls `connect_direct` from A to B.
5. Waits for `peer_connection_mode` to report `"direct"` on A.
6. Kills the relay.
7. Sends chat in both directions and verifies arrival via the now-direct WebRTC datachannel.

If steps 4–7 pass, the reliable channel still negotiates, opens, and carries traffic correctly with the second channel present alongside it.

If the test fails:
- Failure during step 4–5 (`connect_direct` hangs, never reports `direct`): the both-open wait probably deadlocks because the unreliable channel never fires `open`. Re-check Task 1 Step 3 and Task 2 Step 3 — both `select!` loops must continue to drive the Answer/ICE traffic while waiting for opens.
- Failure during steps 6–7 (chat doesn't arrive after relay death): the reliable channel was opened but is now dropping or misrouting messages. Re-check the label dispatch in Task 2 Step 2 — the `"sunset-sync"` arm must wire the same closures to the same channels as before.

- [ ] **Step 4: Commit (no code changes; this task records that the regression check passed)**

This task does not modify any code, so there is nothing to commit. Move on to the spec-coverage check below.

---

## Spec coverage check (self-review)

Before considering the plan complete, walk the spec and confirm each section is implemented:

| Spec section / requirement                                          | Implemented in                                                                  |
|---------------------------------------------------------------------|----------------------------------------------------------------------------------|
| Two named datachannels (`sunset-sync` + `sunset-sync-unrel`)         | Task 1 Step 2 (connect side); Task 2 Step 2 (accept side label dispatch)         |
| `ordered: false`, `maxRetransmits: 0` on the unreliable channel      | Task 1 Step 2 (`set_ordered(false)` + `set_max_retransmits(0)`)                  |
| Both `connect()` and `accept()` wait for both opens before returning | Task 1 Step 3; Task 2 Step 3                                                     |
| `WebRtcRawConnection` carries both channels + their inbound queues   | Task 1 Step 1 (struct definition)                                                |
| `send_unreliable` / `recv_unreliable` route through `dc_unrel`       | Task 3 Step 1                                                                    |
| Unknown labels logged and dropped (graceful version skew)            | Task 2 Step 2 (`other => web_sys::console::warn_1(…)`)                            |
| Either-channel-fails-to-open ⇒ whole handshake errors                | Task 2 Step 4 (`ok_or_else` on both `dc_opt`s); Task 1 Step 3 + Task 2 Step 3 (loops never break early) |
| No new public API on `Client` / Bus                                  | Plan touches only `wasm.rs`; no `lib.rs` / `Client` / Bus edits                  |
| Reliable channel still works                                         | Task 5 Step 3 (`kill_relay` Playwright regression)                                |
| Byte-flow validation deferred to Plan C                              | Documented in spec + verification strategy above; no in-crate byte-flow test added |

Self-review pass: every spec requirement maps to a concrete task. No placeholders. Type names (`dc_unrel`, `rx_unrel`, `_on_open_unrel`, `_on_msg_unrel`, `WebRtcSignalKind`, `RtcDataChannelInit`, `set_ordered`, `set_max_retransmits`) are consistent across all tasks.

---

## Done criteria

- [ ] Task 1 commit landed: connect side creates both channels and waits for both opens.
- [ ] Task 2 commit landed: accept side dispatches by label and waits for both.
- [ ] Task 3 commit landed: `send_unreliable` / `recv_unreliable` route through `dc_unrel` / `rx_unrel`.
- [ ] Task 4: `cargo clippy` (both targets) + `cargo fmt --check` clean; `sunset-web-wasm` builds.
- [ ] Task 5: `kill_relay` Playwright test passes.
- [ ] Spec coverage table above is fully checked.
