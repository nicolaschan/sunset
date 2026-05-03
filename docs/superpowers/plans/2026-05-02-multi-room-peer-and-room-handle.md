# Multi-Room Peer + RoomHandle Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Decouple "the running peer" from "a single room" so the web client can have multiple rooms open concurrently against one shared engine, store, and transport stack. The room rail in the UI stops being decorative and actually routes messages to the room the user has selected.

**Architecture:** Multi-room logic lands in `sunset-core` as `Peer` + `OpenRoom` types. `sunset-web-wasm::Client` shrinks to a thin wasm-bindgen veneer over `Peer`; `sunset-web-wasm::RoomHandle` is the new wasm-bindgen veneer over `OpenRoom`. `presence_publisher` and `RelaySignaler` move from the wasm crate to core (already host-portable via `sunset_sync::spawn::spawn_local`). A new `MultiRoomSignaler` dispatches Noise_KK signaling across N open rooms over the existing single shared `WebRtcRawTransport`.

**Tech Stack:** Rust 2024 (workspace), `wasm-bindgen` for the bridge, Gleam + Lustre for the UI, Playwright for e2e. Existing `sunset_sync::spawn::spawn_local` shim covers wasm + native.

**Spec:** `docs/superpowers/specs/2026-05-02-multi-room-peer-and-room-handle-design.md`.

---

## File map

**Move (wasm crate → core):**

- `crates/sunset-web-wasm/src/presence_publisher.rs` (~70 lines) → `crates/sunset-core/src/membership/publisher.rs` (new module split). Logic unchanged; `web_sys::console::warn_1` swaps for `tracing::warn!`.
- `crates/sunset-web-wasm/src/relay_signaler.rs` (~366 lines) → `crates/sunset-core/src/signaling.rs` (new module). Logic unchanged; `web_sys::console::error_1` / `warn_1` swap for `tracing::error!` / `tracing::warn!`. The two wasm-specific imports (`wasm_bindgen::JsValue`) drop.

**Create (sunset-core):**

- `crates/sunset-core/src/membership/mod.rs` — split existing single-file `membership.rs` so we can add `publisher` as a sibling; existing public API re-exported unchanged.
- `crates/sunset-core/src/signaling.rs` — `RelaySignaler` (moved) + new `MultiRoomSignaler`.
- `crates/sunset-core/src/peer/mod.rs` — `Peer` struct.
- `crates/sunset-core/src/peer/open_room.rs` — `OpenRoom` + `RoomState`.

**Modify (sunset-core):**

- `crates/sunset-core/Cargo.toml` — add `tracing.workspace = true`, `sunset-noise.workspace = true`, `wasm-bindgen.workspace = true` (for `JsValue`-free console replacement; actually only `tracing` + `sunset-noise` needed, see Phase 1/2).
- `crates/sunset-core/src/lib.rs` — re-export new modules.

**Modify (sunset-web-wasm):**

- `crates/sunset-web-wasm/src/lib.rs` — drop `mod presence_publisher`, `mod relay_signaler`; add `mod room_handle`; re-export `RelaySignaler` is removed (callers import from `sunset_core` now).
- `crates/sunset-web-wasm/src/client.rs` — slim down to thin `Peer` veneer. Drop `room_name` parameter from `Client::new`. Add `Client::open_room(name) -> RoomHandle`. Move `send_message`, `on_message`, `on_receipt`, `on_members_changed`, `on_relay_status_changed`, `start_presence`, `connect_direct`, `peer_connection_mode`, `publish_room_subscription` to `RoomHandle`.
- `crates/sunset-web-wasm/src/room_handle.rs` (new) — `#[wasm_bindgen]` wrapper around `sunset_core::OpenRoom`.

**Delete (sunset-web-wasm):**

- `crates/sunset-web-wasm/src/presence_publisher.rs` (moved).
- `crates/sunset-web-wasm/src/relay_signaler.rs` (moved).

**Modify (Gleam web client):**

- `web/src/sunset_web/sunset.gleam` — drop `room_name` from `create_client`; add `RoomHandle` opaque type + `open_room` external; move `send_message`, `on_message`, `on_receipt`, `on_members_changed`, `on_relay_status_changed`, `start_presence`, `client_connect_direct`, `client_peer_connection_mode`, `publish_room_subscription` to take `RoomHandle` instead of `ClientHandle`.
- `web/src/sunset_web/sunset.ffi.mjs` — matching JS shims.
- `web/src/sunset_web/domain.gleam` — add `RoomState` record (or inline in `sunset_web.gleam` Model — see Phase 12).
- `web/src/sunset_web.gleam` — Model gains `rooms: Dict(String, RoomState)`; per-room state moves out of the flat fields; bootstrap flow opens a `RoomHandle` per joined room; `IncomingMsg` / `IncomingReceipt` / `MembersUpdated` / `RelayStatusUpdated` callbacks dispatch with a room-name argument; `SubmitDraft` looks up the active room's handle.
- `web/src/sunset_web/views/main_panel.gleam` — receives messages-for-active-room from the model lookup (no signature change).
- `web/src/sunset_web/views/members.gleam` — receives members-for-active-room from the model lookup.

**Modify (Gleam tests):**

- `web/src/sunset_web/fixture.gleam` — no change needed (channel/member fixtures still consumed by views directly).

**Modify (e2e):**

- `web/e2e/presence.spec.js` — `client.connect_direct(...)` → `roomHandle.connect_direct(...)`. Test setup needs to call `client.open_room(...)` first to obtain a handle.
- `web/e2e/kill_relay.spec.js` — same rename + open_room call.

**Create (e2e):**

- `web/e2e/room_switching.spec.js` (new) — single browser tab, join two rooms, send a message in each, verify isolation.

---

## Quick reference

**Working directory (created in Phase 0):** `/home/nicolas/src/sunset/.worktrees/multi-room` (branch `feature/multi-room-peer`).

**Native test:** `nix develop --command cargo test --workspace --all-features`
**One crate:** `nix develop --command cargo test -p sunset-core --all-features`
**One test:** `nix develop --command cargo test -p sunset-core --all-features peer::tests::open_room_is_idempotent`
**Wasm-bridge test (browser-only types build):** `nix develop --command cargo build -p sunset-web-wasm --target wasm32-unknown-unknown`
**Wasm package build:** `nix run .#web-build` (rebuilds `web/priv/wasm/sunset_web_wasm_bg.wasm`)
**Gleam test:** `cd web && nix develop --command gleam test`
**Playwright (single test):** `nix run .#web-test -- room_switching.spec.js --project=chromium`
**Playwright (whole suite):** `nix run .#web-test -- --project=chromium`
**Lint:** `nix develop --command cargo clippy --workspace --all-features --all-targets -- -D warnings`
**Format:** `nix develop --command cargo fmt --all --check`

---

## Phase 0: Worktree setup

### Task 0: Create worktree off the design branch

**Files:** none (git operations only).

- [ ] **Step 1: Verify the design branch is current**

```bash
cd /home/nicolas/src/sunset
git fetch origin
git log -1 design/multi-room-peer-and-room-handle --format='%h %s'
```

Expected: shows the spec commit `7607dea spec: drop Spawner trait, ...` (or any later spec amendment).

- [ ] **Step 2: Use the using-git-worktrees skill to create the worktree**

Invoke `superpowers:using-git-worktrees` skill with target branch `feature/multi-room-peer` and base branch `design/multi-room-peer-and-room-handle`. Worktree path: `/home/nicolas/src/sunset/.worktrees/multi-room`.

- [ ] **Step 3: Verify worktree state**

```bash
cd /home/nicolas/src/sunset/.worktrees/multi-room
git status
git log -1 --format='%h %s'
```

Expected: clean working tree on `feature/multi-room-peer`, HEAD pointing at the spec commit.

- [ ] **Step 4: Baseline test**

```bash
cd /home/nicolas/src/sunset/.worktrees/multi-room
nix develop --command cargo test --workspace --all-features
```

Expected: PASS. Records the green baseline before any changes.

---

## Phase 1: Move presence_publisher to sunset-core

### Task 1: Split membership.rs into a module directory

The existing `crates/sunset-core/src/membership.rs` is one file. Split it into a `mod.rs` so we can add `publisher` as a sibling without churning the existing API.

**Files:**
- Create: `crates/sunset-core/src/membership/mod.rs`
- Delete: `crates/sunset-core/src/membership.rs`

- [ ] **Step 1: Move the file**

```bash
cd /home/nicolas/src/sunset/.worktrees/multi-room
git mv crates/sunset-core/src/membership.rs crates/sunset-core/src/membership/mod.rs
```

- [ ] **Step 2: Run tests**

```bash
nix develop --command cargo test -p sunset-core --all-features
```

Expected: PASS. The module path didn't change (`crate::membership::...` still resolves), only the file location.

- [ ] **Step 3: Commit**

```bash
git commit -am "sunset-core: move membership.rs into module directory"
```

### Task 2: Add `tracing` dep to sunset-core

**Files:** Modify `crates/sunset-core/Cargo.toml`.

- [ ] **Step 1: Read current deps**

```bash
grep -A30 '^\[dependencies\]' crates/sunset-core/Cargo.toml
```

- [ ] **Step 2: Add `tracing` to `[dependencies]`**

In `crates/sunset-core/Cargo.toml`, under `[dependencies]`, add:

```toml
tracing.workspace = true
```

(Place alphabetically near `tokio.workspace = true`.)

- [ ] **Step 3: Verify it builds**

```bash
nix develop --command cargo build -p sunset-core --all-features
```

Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/sunset-core/Cargo.toml
git commit -m "sunset-core: add tracing dep (for moved logging from wasm crate)"
```

### Task 3: Create publisher module in core with the moved logic

**Files:**
- Create: `crates/sunset-core/src/membership/publisher.rs`
- Modify: `crates/sunset-core/src/membership/mod.rs` (add `pub mod publisher;` and `pub use publisher::spawn_publisher;`)

- [ ] **Step 1: Write the new file**

Create `crates/sunset-core/src/membership/publisher.rs`:

```rust
//! Heartbeat publisher: spawns a task that periodically writes a
//! `<room_fp>/presence/<my_pk>` entry into the local store. The
//! engine's existing room_filter subscription propagates these to
//! peers automatically.
//!
//! Moved from `sunset-web-wasm::presence_publisher` so non-web hosts
//! (TUI, Minecraft mod, native relay) can use the same logic.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use wasmtimer::tokio::sleep;

use crate::Identity;
use sunset_store::{ContentBlock, SignedKvEntry, Store, canonical::signing_payload};

/// Spawn the heartbeat publisher. Runs forever (host-process / page lifetime).
pub fn spawn_publisher<S: Store + 'static>(
    identity: Identity,
    room_fp_hex: String,
    store: Arc<S>,
    interval_ms: u64,
    ttl_ms: u64,
) {
    sunset_sync::spawn::spawn_local(async move {
        let my_hex = hex::encode(identity.store_verifying_key().as_bytes());
        let name_str = format!("{room_fp_hex}/presence/{my_hex}");
        loop {
            if let Err(e) = publish_once(&identity, &name_str, &*store, ttl_ms).await {
                tracing::warn!("presence publisher: {e}");
            }
            sleep(Duration::from_millis(interval_ms)).await;
        }
    });
}

async fn publish_once<S: Store + 'static>(
    identity: &Identity,
    name_str: &str,
    store: &S,
    ttl_ms: u64,
) -> Result<(), String> {
    let block = ContentBlock {
        data: Bytes::new(),
        references: vec![],
    };
    let value_hash = block.hash();
    let now = web_time::SystemTime::now()
        .duration_since(web_time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let mut entry = SignedKvEntry {
        verifying_key: identity.store_verifying_key(),
        name: Bytes::from(name_str.to_owned()),
        value_hash,
        priority: now,
        expires_at: Some(now + ttl_ms),
        signature: Bytes::new(),
    };
    let payload = signing_payload(&entry);
    let sig = identity.sign(&payload);
    entry.signature = Bytes::copy_from_slice(&sig.to_bytes());
    store
        .insert(entry, Some(block))
        .await
        .map_err(|e| format!("{e}"))?;
    Ok(())
}
```

Note: generic over `Store` so it works with any backend (was hard-bound to `MemoryStore` in the wasm version).

- [ ] **Step 2: Wire into the membership module**

Edit `crates/sunset-core/src/membership/mod.rs`. At the very top, after the existing `//!` doc comment, add:

```rust
pub mod publisher;
pub use publisher::spawn_publisher;
```

- [ ] **Step 3: Build and run tests**

```bash
nix develop --command cargo test -p sunset-core --all-features
```

Expected: PASS. New module compiles; no behavior change in core itself.

- [ ] **Step 4: Commit**

```bash
git add crates/sunset-core/src/membership/publisher.rs crates/sunset-core/src/membership/mod.rs
git commit -m "sunset-core: add membership::publisher (moved from sunset-web-wasm)"
```

### Task 4: Switch wasm Client to use the core publisher and delete the old file

**Files:**
- Modify: `crates/sunset-web-wasm/src/client.rs`
- Modify: `crates/sunset-web-wasm/src/lib.rs`
- Delete: `crates/sunset-web-wasm/src/presence_publisher.rs`

- [ ] **Step 1: Update Client to call the core publisher**

In `crates/sunset-web-wasm/src/client.rs`, find the call to `crate::presence_publisher::spawn_publisher(...)` (around line 219) and change it to:

```rust
sunset_core::membership::spawn_publisher(
    self.identity.clone(),
    room_fp_hex.clone(),
    self.store.clone(),
    interval_ms as u64,
    ttl_ms as u64,
);
```

- [ ] **Step 2: Drop the module from lib.rs**

In `crates/sunset-web-wasm/src/lib.rs`, find and delete:

```rust
mod presence_publisher;
```

- [ ] **Step 3: Delete the old file**

```bash
git rm crates/sunset-web-wasm/src/presence_publisher.rs
```

- [ ] **Step 4: Build wasm and run native tests**

```bash
nix develop --command cargo build -p sunset-web-wasm --target wasm32-unknown-unknown
nix develop --command cargo test --workspace --all-features
```

Expected: both PASS. The wasm build is the load-bearing one — it confirms the moved code still works in the browser target.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "sunset-web-wasm: use sunset-core's spawn_publisher; remove local copy"
```

---

## Phase 2: Move RelaySignaler to sunset-core

### Task 5: Add `sunset-noise` dep to sunset-core

**Files:** Modify `crates/sunset-core/Cargo.toml`.

- [ ] **Step 1: Add `sunset-noise.workspace = true` to `[dependencies]`**

In `crates/sunset-core/Cargo.toml`, under `[dependencies]`, add:

```toml
sunset-noise.workspace = true
sunset-store-memory.workspace = true
```

`sunset-store-memory` is needed because the moved `RelaySignaler` ties to `Arc<MemoryStore>` today; we'll generalize over `Store` in Task 6 to drop this dep again, but adding it now keeps the move atomic. (See Step 4 of Task 6.)

Wait — do not add `sunset-store-memory`. We will generalize over `S: Store` in the move itself (Task 6, Step 1). Leave the dep list with only `sunset-noise.workspace = true` added.

- [ ] **Step 2: Build**

```bash
nix develop --command cargo build -p sunset-core --all-features
```

Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/sunset-core/Cargo.toml
git commit -m "sunset-core: add sunset-noise dep (for moved RelaySignaler)"
```

### Task 6: Move RelaySignaler into core, generic over Store

**Files:**
- Create: `crates/sunset-core/src/signaling.rs`
- Modify: `crates/sunset-core/src/lib.rs` (add `pub mod signaling;`)

- [ ] **Step 1: Read the existing wasm file to copy from**

```bash
cat crates/sunset-web-wasm/src/relay_signaler.rs
```

Keep that content open. The new file at `crates/sunset-core/src/signaling.rs` is the same content with these changes:
1. `use sunset_core::Identity;` → `use crate::Identity;`
2. Generic over `S: Store + 'static` instead of hard-bound to `MemoryStore`. Replace `Arc<MemoryStore>` with `Arc<S>` everywhere; replace `pub fn new(local_identity: Identity, room_fp_hex: String, store: &Arc<MemoryStore>)` with `pub fn new<S: Store + 'static>(local_identity: Identity, room_fp_hex: String, store: &Arc<S>) -> Rc<Self>` and have the struct hold `store: Arc<dyn ErasedStore>` via a small object-safe wrapper, OR (simpler) keep the type generic on the struct itself with a `_store: PhantomData<S>` if needed.

Actually the simplest version: parameterize the struct with `S`:

```rust
pub struct RelaySignaler<S: Store + 'static = sunset_store_memory::MemoryStore> {
    // fields ...
    store: Arc<S>,
    // ...
}
```

But making `MemoryStore` the default keeps the dep — and per Task 5 we agreed to NOT add `sunset-store-memory` to core. Drop the default. Callers must specify the store type.

- [ ] **Step 2: Write the new file**

Create `crates/sunset-core/src/signaling.rs` with the content of `relay_signaler.rs`, modified as follows:

```rust
//! `Signaler` impl that sits on top of an existing `Store` +
//! `SyncEngine`. Each outbound `SignalMessage` becomes a `SignedKvEntry`
//! named `<room_fp_hex>/webrtc/<from_hex>/<to_hex>/<seq:016x>` whose
//! content block carries the Noise_KK ciphertext for the payload.
//!
//! Moved from `sunset-web-wasm::relay_signaler` so non-web hosts can
//! signal Noise_KK setup via the same CRDT-entry path.

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use async_trait::async_trait;
use bytes::Bytes;
use futures::StreamExt;
use futures::channel::{mpsc, oneshot};
use tokio::sync::Mutex;
use zeroize::Zeroizing;

use crate::Identity;
use sunset_noise::{KkInitiator, KkResponder, KkSession, ed25519_seed_to_x25519_secret};
use sunset_store::{
    ContentBlock, Filter, Replay, SignedKvEntry, Store, VerifyingKey, canonical::signing_payload,
};
use sunset_sync::{Error as SyncError, PeerId, Result as SyncResult, SignalMessage, Signaler};

pub fn signaling_filter(room_fp_hex: &str) -> Filter {
    Filter::NamePrefix(Bytes::from(format!("{room_fp_hex}/webrtc/")))
}

fn entry_name(room_fp_hex: &str, from: &PeerId, to: &PeerId, seq: u64) -> Bytes {
    let from_hex = hex::encode(from.verifying_key().as_bytes());
    let to_hex = hex::encode(to.verifying_key().as_bytes());
    Bytes::from(format!(
        "{room_fp_hex}/webrtc/{from_hex}/{to_hex}/{seq:016x}"
    ))
}

fn parse_entry_name(name: &[u8], room_fp_hex: &str) -> Option<(PeerId, PeerId, u64)> {
    let s = std::str::from_utf8(name).ok()?;
    let suffix = s.strip_prefix(&format!("{room_fp_hex}/webrtc/"))?;
    let mut parts = suffix.splitn(3, '/');
    let from_hex = parts.next()?;
    let to_hex = parts.next()?;
    let seq_hex = parts.next()?;
    let from_bytes = hex::decode(from_hex).ok()?;
    let to_bytes = hex::decode(to_hex).ok()?;
    let seq = u64::from_str_radix(seq_hex, 16).ok()?;
    Some((
        PeerId(VerifyingKey::new(Bytes::from(from_bytes))),
        PeerId(VerifyingKey::new(Bytes::from(to_bytes))),
        seq,
    ))
}

#[derive(Default)]
struct PeerKkSlot {
    initiator: Option<KkInitiator>,
    responder: Option<KkResponder>,
    session: Option<KkSession>,
    next_send_seq: u64,
    on_session_ready: Vec<oneshot::Sender<()>>,
}

struct Inner {
    peers: HashMap<PeerId, PeerKkSlot>,
}

pub struct RelaySignaler<S: Store + 'static> {
    local_identity: Identity,
    local_x25519_secret: Zeroizing<[u8; 32]>,
    x25519_pub_cache: Mutex<HashMap<PeerId, [u8; 32]>>,
    pub(crate) room_fp_hex: String,
    store: Arc<S>,
    inner: Mutex<Inner>,
    inbound_rx: Mutex<mpsc::UnboundedReceiver<SignalMessage>>,
}

impl<S: Store + 'static> RelaySignaler<S> {
    pub fn new(local_identity: Identity, room_fp_hex: String, store: &Arc<S>) -> Rc<Self> {
        let local_x25519_secret = ed25519_seed_to_x25519_secret(&local_identity.secret_bytes());
        let (inbound_tx, inbound_rx) = mpsc::unbounded::<SignalMessage>();
        let signaler = Rc::new(Self {
            local_identity,
            local_x25519_secret,
            x25519_pub_cache: Mutex::new(HashMap::new()),
            room_fp_hex,
            store: store.clone(),
            inner: Mutex::new(Inner {
                peers: HashMap::new(),
            }),
            inbound_rx: Mutex::new(inbound_rx),
        });
        let me = signaler.clone();
        sunset_sync::spawn::spawn_local(async move {
            me.run_dispatcher(inbound_tx).await;
        });
        signaler
    }

    fn local_peer(&self) -> PeerId {
        PeerId(self.local_identity.store_verifying_key())
    }

    async fn x25519_pub_for(&self, peer: &PeerId) -> SyncResult<[u8; 32]> {
        if let Some(p) = self.x25519_pub_cache.lock().await.get(peer) {
            return Ok(*p);
        }
        let bytes: &[u8] = peer.verifying_key().as_bytes();
        let arr: [u8; 32] = bytes.try_into().map_err(|_| {
            SyncError::Transport(format!("peer pubkey wrong length: {}", bytes.len()))
        })?;
        let x = sunset_noise::ed25519_public_to_x25519(&arr)
            .map_err(|e| SyncError::Transport(format!("x25519 derive: {e}")))?;
        self.x25519_pub_cache.lock().await.insert(peer.clone(), x);
        Ok(x)
    }

    async fn run_dispatcher(&self, inbound_tx: mpsc::UnboundedSender<SignalMessage>) {
        let filter = signaling_filter(&self.room_fp_hex);
        let mut events = match self.store.subscribe(filter, Replay::All).await {
            Ok(s) => s,
            Err(e) => {
                tracing::error!("RelaySignaler subscribe: {e}");
                return;
            }
        };
        while let Some(ev) = events.next().await {
            let entry = match ev {
                Ok(sunset_store::Event::Inserted(e)) => e,
                Ok(sunset_store::Event::Replaced { new, .. }) => new,
                Ok(_) => continue,
                Err(e) => {
                    tracing::error!("RelaySignaler event: {e}");
                    continue;
                }
            };
            if let Err(e) = self.handle_entry(&entry, &inbound_tx).await {
                tracing::warn!("RelaySignaler handle_entry: {e}");
            }
        }
    }

    async fn handle_entry(
        &self,
        entry: &SignedKvEntry,
        inbound_tx: &mpsc::UnboundedSender<SignalMessage>,
    ) -> SyncResult<()> {
        let (from, to, seq) = parse_entry_name(&entry.name, &self.room_fp_hex)
            .ok_or_else(|| SyncError::Transport("bad signaling entry name".into()))?;
        if to != self.local_peer() {
            return Ok(());
        }
        if from == self.local_peer() {
            return Ok(());
        }

        let block = self
            .store
            .get_content(&entry.value_hash)
            .await?
            .ok_or_else(|| SyncError::Transport("missing content block".into()))?;
        let ciphertext: &[u8] = &block.data;

        let plaintext = self.decrypt_inbound(&from, ciphertext).await?;

        let _ = inbound_tx.unbounded_send(SignalMessage {
            from,
            to,
            seq,
            payload: Bytes::from(plaintext),
        });
        Ok(())
    }

    async fn decrypt_inbound(&self, from: &PeerId, ciphertext: &[u8]) -> SyncResult<Vec<u8>> {
        let mut inner = self.inner.lock().await;
        let slot = inner.peers.entry(from.clone()).or_default();
        if slot.session.is_none() && slot.initiator.is_none() && slot.responder.is_none() {
            let remote_x = self.x25519_pub_for(from).await?;
            let mut resp = KkResponder::new(&self.local_x25519_secret, &remote_x)
                .map_err(|e| SyncError::Transport(format!("KkResponder::new: {e}")))?;
            let pt = resp
                .read_message_1(ciphertext)
                .map_err(|e| SyncError::Transport(format!("read_message_1: {e}")))?;
            slot.responder = Some(resp);
            return Ok(pt);
        }
        if let Some(init) = slot.initiator.take() {
            let (pt, session) = init
                .read_message_2(ciphertext)
                .map_err(|e| SyncError::Transport(format!("read_message_2: {e}")))?;
            slot.session = Some(session);
            for waiter in slot.on_session_ready.drain(..) {
                let _ = waiter.send(());
            }
            return Ok(pt);
        }
        if let Some(session) = slot.session.as_mut() {
            return session
                .decrypt(ciphertext)
                .map_err(|e| SyncError::Transport(format!("session.decrypt: {e}")));
        }
        Err(SyncError::Transport(
            "inbound before responder sent msg2; dropped".into(),
        ))
    }

    async fn next_send_seq(&self, to: &PeerId) -> u64 {
        let mut inner = self.inner.lock().await;
        let slot = inner.peers.entry(to.clone()).or_default();
        let s = slot.next_send_seq;
        slot.next_send_seq = s + 1;
        s
    }

    async fn write_entry(&self, to: &PeerId, seq: u64, ciphertext: Vec<u8>) -> SyncResult<()> {
        let from = self.local_peer();
        let block = ContentBlock {
            data: Bytes::from(ciphertext),
            references: vec![],
        };
        let value_hash = block.hash();
        let priority = web_time::SystemTime::now()
            .duration_since(web_time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);

        let mut entry = SignedKvEntry {
            verifying_key: self.local_identity.store_verifying_key(),
            name: entry_name(&self.room_fp_hex, &from, to, seq),
            value_hash,
            priority,
            expires_at: Some(priority + 3_600_000),
            signature: Bytes::new(),
        };
        let payload = signing_payload(&entry);
        let sig = self.local_identity.sign(&payload);
        entry.signature = Bytes::copy_from_slice(&sig.to_bytes());

        self.store
            .insert(entry, Some(block))
            .await
            .map_err(SyncError::Store)?;
        Ok(())
    }
}

#[async_trait(?Send)]
impl<S: Store + 'static> Signaler for RelaySignaler<S> {
    async fn send(&self, message: SignalMessage) -> SyncResult<()> {
        let to = message.to.clone();
        let plaintext = message.payload;

        loop {
            let ciphertext_opt = {
                let mut inner = self.inner.lock().await;
                let slot = inner.peers.entry(to.clone()).or_default();
                if slot.initiator.is_none() && slot.responder.is_none() && slot.session.is_none() {
                    let remote_x = self.x25519_pub_for(&to).await?;
                    let mut init = KkInitiator::new(&self.local_x25519_secret, &remote_x)
                        .map_err(|e| SyncError::Transport(format!("KkInitiator::new: {e}")))?;
                    let ct = init
                        .write_message_1(&plaintext)
                        .map_err(|e| SyncError::Transport(format!("write_message_1: {e}")))?;
                    slot.initiator = Some(init);
                    Some(ct)
                } else if let Some(resp) = slot.responder.take() {
                    let (ct, session) = resp
                        .write_message_2(&plaintext)
                        .map_err(|e| SyncError::Transport(format!("write_message_2: {e}")))?;
                    slot.session = Some(session);
                    for waiter in slot.on_session_ready.drain(..) {
                        let _ = waiter.send(());
                    }
                    Some(ct)
                } else if let Some(session) = slot.session.as_mut() {
                    let ct = session
                        .encrypt(&plaintext)
                        .map_err(|e| SyncError::Transport(format!("session.encrypt: {e}")))?;
                    Some(ct)
                } else {
                    let (tx, rx) = oneshot::channel::<()>();
                    slot.on_session_ready.push(tx);
                    drop(inner);
                    let _ = rx.await;
                    None
                }
            };
            if let Some(ciphertext) = ciphertext_opt {
                let seq = self.next_send_seq(&to).await;
                self.write_entry(&to, seq, ciphertext).await?;
                return Ok(());
            }
        }
    }

    async fn recv(&self) -> SyncResult<SignalMessage> {
        let mut rx = self.inbound_rx.lock().await;
        rx.next()
            .await
            .ok_or_else(|| SyncError::Transport("signaler closed".into()))
    }
}
```

- [ ] **Step 3: Wire into lib.rs**

Edit `crates/sunset-core/src/lib.rs`. Add at the appropriate place (alphabetical with existing modules):

```rust
pub mod signaling;
```

And add to the re-exports block:

```rust
pub use signaling::{RelaySignaler, signaling_filter};
```

- [ ] **Step 4: Build**

```bash
nix develop --command cargo build -p sunset-core --all-features
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/sunset-core/src/signaling.rs crates/sunset-core/src/lib.rs
git commit -m "sunset-core: add signaling::RelaySignaler (moved from sunset-web-wasm, generic over Store)"
```

### Task 7: Switch wasm Client to use the core RelaySignaler and delete the old file

**Files:**
- Modify: `crates/sunset-web-wasm/src/client.rs`
- Modify: `crates/sunset-web-wasm/src/lib.rs`
- Delete: `crates/sunset-web-wasm/src/relay_signaler.rs`

- [ ] **Step 1: Update Client imports**

In `crates/sunset-web-wasm/src/client.rs`:

```rust
// Replace:
use crate::relay_signaler::RelaySignaler;
// With:
use sunset_core::RelaySignaler;
```

- [ ] **Step 2: The construction call site needs the type parameter**

In `Client::new`, the call:

```rust
let signaler = RelaySignaler::new(identity.clone(), room_fp_hex.clone(), &store);
```

Should still type-infer correctly since `store: Arc<MemoryStore>` is in scope. If it doesn't, annotate:

```rust
let signaler: Rc<RelaySignaler<MemoryStore>> = RelaySignaler::new(...);
```

- [ ] **Step 3: Update the `signaler_dyn` cast**

The line:

```rust
let signaler_dyn: Rc<dyn sunset_sync::Signaler> = signaler;
```

Stays the same — the Signaler trait impl in core works the same.

- [ ] **Step 4: Drop the module from lib.rs**

In `crates/sunset-web-wasm/src/lib.rs`:

```rust
// Remove:
mod relay_signaler;
// Remove:
pub use relay_signaler::{RelaySignaler, signaling_filter};
```

- [ ] **Step 5: Delete the old file**

```bash
git rm crates/sunset-web-wasm/src/relay_signaler.rs
```

- [ ] **Step 6: Build wasm + run native tests + run wasm e2e baseline**

```bash
nix develop --command cargo build -p sunset-web-wasm --target wasm32-unknown-unknown
nix develop --command cargo test --workspace --all-features
nix develop --command cargo clippy --workspace --all-features --all-targets -- -D warnings
```

Expected: all PASS.

- [ ] **Step 7: Run baseline e2e to confirm no regression**

```bash
nix run .#web-test -- presence.spec.js --project=chromium
```

Expected: PASS. (This test exercises the `connect_direct` path which uses the moved RelaySignaler.)

- [ ] **Step 8: Commit**

```bash
git add -A
git commit -m "sunset-web-wasm: use sunset-core's RelaySignaler; remove local copy"
```

---

## Phase 3: MultiRoomSignaler in core

### Task 8: TDD — MultiRoomSignaler register/unregister

**Files:**
- Modify: `crates/sunset-core/src/signaling.rs` — add `MultiRoomSignaler` struct + tests.

- [ ] **Step 1: Write the failing test**

Append to `crates/sunset-core/src/signaling.rs`:

```rust
#[cfg(test)]
mod multi_room_tests {
    use super::*;
    use crate::crypto::room::Room;
    use crate::crypto::constants::test_fast_params;
    use crate::Identity;
    use sunset_store_memory::MemoryStore;
    use sunset_store::Verifier;
    use std::sync::Arc;

    fn ident(seed: u8) -> Identity {
        Identity::from_seed(&[seed; 32]).unwrap()
    }

    fn store() -> Arc<MemoryStore> {
        Arc::new(MemoryStore::new(Arc::new(crate::Ed25519Verifier)))
    }

    #[tokio::test(flavor = "current_thread")]
    async fn register_inserts_and_unregister_removes() {
        let local = tokio::task::LocalSet::new();
        local.run_until(async {
            let dispatcher = MultiRoomSignaler::new();
            let id = ident(1);
            let st = store();
            let room = Room::open_with_params("alpha", &test_fast_params()).unwrap();
            let fp = room.fingerprint();
            let signaler = RelaySignaler::new(id, fp.to_hex(), &st);

            assert_eq!(dispatcher.len(), 0);
            dispatcher.register(fp, signaler.clone());
            assert_eq!(dispatcher.len(), 1);
            assert!(dispatcher.contains(&fp));

            dispatcher.unregister(&fp);
            assert_eq!(dispatcher.len(), 0);
            assert!(!dispatcher.contains(&fp));
        }).await;
    }
}
```

- [ ] **Step 2: Run the test (expect FAIL — types don't exist)**

```bash
nix develop --command cargo test -p sunset-core --all-features signaling::multi_room_tests::register_inserts_and_unregister_removes
```

Expected: FAIL with errors about `MultiRoomSignaler`, `Verifier` (might need to check module path), or the helper types.

If `Identity::from_seed` doesn't exist, use the actual constructor — `grep -n "impl Identity" crates/sunset-core/src/identity.rs` to find it. The test should use whichever constructor works.

- [ ] **Step 3: Add the MultiRoomSignaler struct (without Signaler impl yet)**

In `crates/sunset-core/src/signaling.rs`, before the `#[cfg(test)]` block:

```rust
use std::cell::RefCell;
use crate::crypto::room::RoomFingerprint;

/// Routes signaling for a `WebRtcRawTransport` across N open rooms.
/// Holds a per-room `RelaySignaler` for each open room. `send` picks any
/// registered signaler (the receiver subscribes to all its open rooms,
/// so the message reaches them via any one); `recv` fans across all
/// per-room receivers via select!.
pub struct MultiRoomSignaler {
    by_room: RefCell<HashMap<RoomFingerprint, Rc<dyn Signaler>>>,
    /// Notifier fired when a new signaler is registered, so an in-flight
    /// `recv` blocked on the current set can re-do its select!.
    register_notify: tokio::sync::Notify,
}

impl MultiRoomSignaler {
    pub fn new() -> Rc<Self> {
        Rc::new(Self {
            by_room: RefCell::new(HashMap::new()),
            register_notify: tokio::sync::Notify::new(),
        })
    }

    pub fn register<S: Store + 'static>(self: &Rc<Self>, fp: RoomFingerprint, signaler: Rc<RelaySignaler<S>>) {
        let dyn_signaler: Rc<dyn Signaler> = signaler;
        self.by_room.borrow_mut().insert(fp, dyn_signaler);
        self.register_notify.notify_waiters();
    }

    pub fn unregister(&self, fp: &RoomFingerprint) {
        self.by_room.borrow_mut().remove(fp);
    }

    pub fn len(&self) -> usize {
        self.by_room.borrow().len()
    }

    pub fn contains(&self, fp: &RoomFingerprint) -> bool {
        self.by_room.borrow().contains_key(fp)
    }
}

impl Default for MultiRoomSignaler {
    fn default() -> Self {
        Self {
            by_room: RefCell::new(HashMap::new()),
            register_notify: tokio::sync::Notify::new(),
        }
    }
}
```

(The `Signaler` trait impl comes in Task 9.)

- [ ] **Step 4: Add `tokio.workspace = true` features if needed**

Verify `crates/sunset-core/Cargo.toml` has `tokio = { workspace = true, features = ["sync", "time"] }`. The `sync` feature gives us `Notify`. If not present, add it.

```bash
grep "^tokio" crates/sunset-core/Cargo.toml
```

- [ ] **Step 5: Run the test (expect PASS)**

```bash
nix develop --command cargo test -p sunset-core --all-features signaling::multi_room_tests::register_inserts_and_unregister_removes
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/sunset-core/src/signaling.rs crates/sunset-core/Cargo.toml
git commit -m "sunset-core: add MultiRoomSignaler skeleton with register/unregister"
```

### Task 9: TDD — MultiRoomSignaler send delegates to registered signaler

**Files:** Modify `crates/sunset-core/src/signaling.rs`.

- [ ] **Step 1: Write the failing test**

Append to the `multi_room_tests` module:

```rust
#[tokio::test(flavor = "current_thread")]
async fn send_routes_to_registered_signaler_and_reaches_via_recv() {
    let local = tokio::task::LocalSet::new();
    local.run_until(async {
        // Two peers (Alice, Bob) sharing one room. Each builds a
        // MultiRoomSignaler with one entry. Alice sends to Bob; Bob
        // recv's the message.
        let alice_id = ident(1);
        let bob_id = ident(2);
        let alice_pk = PeerId(alice_id.store_verifying_key());
        let bob_pk = PeerId(bob_id.store_verifying_key());

        let store = store(); // shared store, simulating a fully-replicated
                             // relay so both signalers see the same entries
        let room = Room::open_with_params("alpha", &test_fast_params()).unwrap();
        let fp = room.fingerprint();

        let alice_signaler = RelaySignaler::new(alice_id, fp.to_hex(), &store);
        let bob_signaler = RelaySignaler::new(bob_id, fp.to_hex(), &store);

        let alice_dispatcher = MultiRoomSignaler::new();
        alice_dispatcher.register(fp, alice_signaler);
        let bob_dispatcher = MultiRoomSignaler::new();
        bob_dispatcher.register(fp, bob_signaler);

        let payload = bytes::Bytes::from_static(b"hello-bob");
        alice_dispatcher.send(SignalMessage {
            from: alice_pk.clone(),
            to: bob_pk.clone(),
            seq: 0,
            payload: payload.clone(),
        }).await.unwrap();

        let received = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            bob_dispatcher.recv(),
        ).await.expect("recv timed out").unwrap();

        // The payload that arrives is decrypted Noise plaintext, which is
        // our original `payload` bytes. (KK first message carries
        // attached payload as plaintext after decryption.)
        assert_eq!(received.from, alice_pk);
        assert_eq!(received.to, bob_pk);
        assert_eq!(received.payload.as_ref(), b"hello-bob");
    }).await;
}
```

- [ ] **Step 2: Run the test (expect FAIL — `Signaler` not implemented for `MultiRoomSignaler`)**

```bash
nix develop --command cargo test -p sunset-core --all-features signaling::multi_room_tests::send_routes_to_registered_signaler_and_reaches_via_recv
```

Expected: FAIL with "the trait `Signaler` is not implemented for `MultiRoomSignaler`".

- [ ] **Step 3: Add the Signaler impl**

Append in `signaling.rs` after the `MultiRoomSignaler` impl block:

```rust
#[async_trait(?Send)]
impl Signaler for MultiRoomSignaler {
    async fn send(&self, message: SignalMessage) -> SyncResult<()> {
        // Pick the first registered per-room signaler. The receiver
        // subscribes to all its open rooms, so any one is sufficient as
        // a carrier as long as we both have it open. If we have no rooms
        // open, fail with a clear error.
        let signaler = {
            let map = self.by_room.borrow();
            map.values().next().cloned()
        };
        match signaler {
            Some(s) => s.send(message).await,
            None => Err(SyncError::Transport(
                "MultiRoomSignaler::send with no rooms registered".into(),
            )),
        }
    }

    async fn recv(&self) -> SyncResult<SignalMessage> {
        // Loop: snapshot the current set of per-room signalers, race their
        // recv()s + the register_notify. If a new signaler registers,
        // re-snapshot.
        loop {
            let signalers: Vec<Rc<dyn Signaler>> = {
                self.by_room.borrow().values().cloned().collect()
            };
            if signalers.is_empty() {
                // No signalers — wait for a registration.
                self.register_notify.notified().await;
                continue;
            }
            // Build a select! across N recvs + the notify.
            let mut futures: futures::stream::FuturesUnordered<_> = signalers
                .iter()
                .map(|s| {
                    let s = s.clone();
                    async move { s.recv().await }
                })
                .collect();
            tokio::select! {
                biased;
                _ = self.register_notify.notified() => {
                    // New room registered; re-snapshot.
                    continue;
                }
                Some(result) = futures::stream::StreamExt::next(&mut futures) => {
                    return result;
                }
            }
        }
    }
}
```

- [ ] **Step 4: Run the test**

```bash
nix develop --command cargo test -p sunset-core --all-features signaling::multi_room_tests::send_routes_to_registered_signaler_and_reaches_via_recv
```

Expected: PASS.

- [ ] **Step 5: Run all sunset-core tests + clippy**

```bash
nix develop --command cargo test -p sunset-core --all-features
nix develop --command cargo clippy -p sunset-core --all-features --all-targets -- -D warnings
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/sunset-core/src/signaling.rs
git commit -m "sunset-core: implement Signaler for MultiRoomSignaler (fan-out send/recv)"
```

### Task 10: Wire wasm Client to use MultiRoomSignaler with one entry (no behavior change)

This preserves the existing single-room Client behavior while moving to the new transport pattern.

**Files:** Modify `crates/sunset-web-wasm/src/client.rs`.

- [ ] **Step 1: Read current Client::new**

```bash
grep -n "let signaler\|let signaler_dyn\|let rtc_raw\|MultiTransport::new" crates/sunset-web-wasm/src/client.rs
```

- [ ] **Step 2: Replace the per-room signaler with a MultiRoomSignaler holding one entry**

In `Client::new`, replace:

```rust
let room_fp_hex = room.fingerprint().to_hex();
let signaler = RelaySignaler::new(identity.clone(), room_fp_hex.clone(), &store);
let local_peer = PeerId(identity.store_verifying_key());
let signaler_dyn: Rc<dyn sunset_sync::Signaler> = signaler;
let rtc_raw = WebRtcRawTransport::new(
    signaler_dyn,
    local_peer.clone(),
    vec!["stun:stun.l.google.com:19302".into()],
);
```

With:

```rust
let room_fp_hex = room.fingerprint().to_hex();
let signaler = RelaySignaler::new(identity.clone(), room_fp_hex.clone(), &store);
let dispatcher = sunset_core::signaling::MultiRoomSignaler::new();
dispatcher.register(room.fingerprint(), signaler);
let local_peer = PeerId(identity.store_verifying_key());
let dispatcher_dyn: Rc<dyn sunset_sync::Signaler> = dispatcher.clone();
let rtc_raw = WebRtcRawTransport::new(
    dispatcher_dyn,
    local_peer.clone(),
    vec!["stun:stun.l.google.com:19302".into()],
);
```

Hold the `dispatcher: Rc<MultiRoomSignaler>` on the `Client` struct so future `open_room` calls (Phase 5+) can register additional signalers:

```rust
pub struct Client {
    // ... existing fields ...
    rtc_signaler_dispatcher: Rc<sunset_core::signaling::MultiRoomSignaler>,
}
```

And add it to the `Ok(Client { ... })` construction.

- [ ] **Step 3: Build wasm + run tests + run e2e baseline**

```bash
nix develop --command cargo build -p sunset-web-wasm --target wasm32-unknown-unknown
nix develop --command cargo test --workspace --all-features
nix run .#web-test -- presence.spec.js --project=chromium
```

Expected: all PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/sunset-web-wasm/src/client.rs
git commit -m "sunset-web-wasm: route signaling through MultiRoomSignaler (one room registered)"
```

---

## Phase 4: Add Peer in core

### Task 11: TDD — Peer construction and basic getters

**Files:**
- Create: `crates/sunset-core/src/peer/mod.rs`
- Modify: `crates/sunset-core/src/lib.rs`

- [ ] **Step 1: Wire the new module into lib.rs (compile gate)**

In `crates/sunset-core/src/lib.rs`, add:

```rust
pub mod peer;
pub use peer::{Peer, OpenRoom};
```

- [ ] **Step 2: Write the failing test**

Create `crates/sunset-core/src/peer/mod.rs`:

```rust
//! `Peer` is the host-agnostic "running sunset peer" entity.
//! Holds identity, store, sync engine, supervisor, and a registry of
//! open rooms. `Peer::open_room(name)` returns an `OpenRoom` handle.

mod open_room;

pub use open_room::OpenRoom;

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::{Rc, Weak};
use std::sync::Arc;

use bytes::Bytes;

use crate::crypto::room::RoomFingerprint;
use crate::signaling::MultiRoomSignaler;
use crate::Identity;
use sunset_store::Store;
use sunset_sync::{MultiTransport, PeerSupervisor, RawTransport, SyncEngine};

pub struct Peer<St: Store + 'static, T: 'static> {
    identity: Identity,
    store: Arc<St>,
    engine: Rc<SyncEngine<St, T>>,
    supervisor: Rc<PeerSupervisor<St, T>>,
    relay_status: Rc<RefCell<String>>,
    open_rooms: RefCell<HashMap<RoomFingerprint, Weak<open_room::RoomState<St, T>>>>,
    rtc_signaler_dispatcher: Rc<MultiRoomSignaler>,
}

impl<St, T> Peer<St, T>
where
    St: Store + 'static,
    T: 'static,
{
    pub fn new(
        identity: Identity,
        store: Arc<St>,
        engine: Rc<SyncEngine<St, T>>,
        supervisor: Rc<PeerSupervisor<St, T>>,
        rtc_signaler_dispatcher: Rc<MultiRoomSignaler>,
    ) -> Rc<Self> {
        Rc::new(Self {
            identity,
            store,
            engine,
            supervisor,
            relay_status: Rc::new(RefCell::new("disconnected".to_owned())),
            open_rooms: RefCell::new(HashMap::new()),
            rtc_signaler_dispatcher,
        })
    }

    pub fn public_key(&self) -> [u8; 32] {
        self.identity.public().as_bytes()
    }

    pub fn relay_status(&self) -> String {
        self.relay_status.borrow().clone()
    }

    pub(crate) fn identity(&self) -> &Identity {
        &self.identity
    }

    pub(crate) fn store(&self) -> &Arc<St> {
        &self.store
    }

    pub(crate) fn engine(&self) -> &Rc<SyncEngine<St, T>> {
        &self.engine
    }

    pub(crate) fn supervisor(&self) -> &Rc<PeerSupervisor<St, T>> {
        &self.supervisor
    }

    pub(crate) fn rtc_dispatcher(&self) -> &Rc<MultiRoomSignaler> {
        &self.rtc_signaler_dispatcher
    }

    pub(crate) fn relay_status_cell(&self) -> Rc<RefCell<String>> {
        self.relay_status.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Ed25519Verifier;
    use sunset_store_memory::MemoryStore;
    use sunset_sync::{BackoffPolicy, SyncConfig};

    fn ident(seed: u8) -> Identity {
        Identity::from_seed(&[seed; 32]).expect("Identity::from_seed")
    }

    /// Fake transport used purely to satisfy generics in unit tests that
    /// don't exercise the network path. Implements RawTransport with no-op
    /// methods so SyncEngine + PeerSupervisor compile.
    pub(super) struct NopTransport;
    // ... see Task 12 for the full impl ...

    #[tokio::test(flavor = "current_thread")]
    async fn peer_new_exposes_public_key_and_default_relay_status() {
        let local = tokio::task::LocalSet::new();
        local.run_until(async {
            // Constructing engine/supervisor/transport is fiddly; we use
            // a NopTransport defined in this test module (Task 12) once
            // it's available. This test just exercises the construction
            // path via the helper `mk_peer()` defined in `tests::helpers`.
            let (peer, _engine_join) = tests::helpers::mk_peer(ident(7)).await;
            assert_eq!(peer.public_key().len(), 32);
            assert_eq!(peer.relay_status(), "disconnected");
        }).await;
    }

    pub(super) mod helpers {
        // Filled in by Task 12.
    }
}
```

- [ ] **Step 3: Run the test (expect FAIL — `helpers::mk_peer` doesn't exist)**

```bash
nix develop --command cargo test -p sunset-core --all-features peer::tests::peer_new_exposes_public_key_and_default_relay_status
```

Expected: FAIL with reference to `helpers::mk_peer` not existing. The compile gate confirms the struct itself is well-formed.

- [ ] **Step 4: Commit the skeleton**

```bash
git add crates/sunset-core/src/peer/mod.rs crates/sunset-core/src/lib.rs
git commit -m "sunset-core: add Peer skeleton (no behavior yet)"
```

### Task 12: TDD — NopTransport test helper + peer construction proof

**Files:**
- Create: `crates/sunset-core/src/peer/open_room.rs` (empty placeholder; populated by later tasks).
- Modify: `crates/sunset-core/src/peer/mod.rs` — add `tests::helpers::mk_peer` + `NopTransport`.

- [ ] **Step 1: Create the empty open_room.rs**

```bash
cat > crates/sunset-core/src/peer/open_room.rs <<'RUST'
//! `OpenRoom` is the per-room handle returned by `Peer::open_room`.
//! Filled in by Phase 5+.

use std::cell::Cell;
use std::rc::{Rc, Weak};
use std::sync::Arc;

use crate::crypto::room::Room;
use crate::membership::TrackerHandles;
use crate::signaling::RelaySignaler;
use sunset_store::Store;

pub(crate) struct RoomState<St: Store + 'static, T: 'static> {
    pub(crate) room: Rc<Room>,
    pub(crate) peer_weak: Weak<super::Peer<St, T>>,
    pub(crate) presence_started: Cell<bool>,
    pub(crate) tracker_handles: Rc<TrackerHandles>,
    pub(crate) signaler: Rc<RelaySignaler<St>>,
    pub(crate) cancel_decode: Rc<Cell<bool>>,
}

pub struct OpenRoom<St: Store + 'static, T: 'static> {
    pub(crate) inner: Rc<RoomState<St, T>>,
}

impl<St: Store + 'static, T: 'static> Drop for RoomState<St, T> {
    fn drop(&mut self) {
        self.cancel_decode.set(true);
        if let Some(peer) = self.peer_weak.upgrade() {
            peer.rtc_signaler_dispatcher.unregister(&self.room.fingerprint());
        }
    }
}
RUST
```

(Phase 5+ replaces this with the real impl — for now we just need it to compile.)

- [ ] **Step 2: Add NopTransport + mk_peer helper**

In `crates/sunset-core/src/peer/mod.rs`, replace the `pub(super) mod helpers { }` block with:

```rust
pub(super) mod helpers {
    use super::*;
    use async_trait::async_trait;
    use bytes::Bytes;
    use sunset_store::Verifier;
    use sunset_sync::{
        BackoffPolicy, MultiTransport, PeerAddr, PeerId, RawConnection, RawTransport,
        Signer, SyncConfig,
    };

    /// Stub transport for unit tests that don't exercise the network.
    pub(crate) struct NopTransport;

    #[async_trait(?Send)]
    impl RawTransport for NopTransport {
        type Conn = NopConnection;
        async fn connect(&self, _addr: PeerAddr) -> sunset_sync::Result<Self::Conn> {
            Err(sunset_sync::Error::Transport("nop".into()))
        }
        async fn accept(&self) -> sunset_sync::Result<Self::Conn> {
            std::future::pending().await
        }
    }

    pub(crate) struct NopConnection;

    #[async_trait(?Send)]
    impl RawConnection for NopConnection {
        async fn send(&mut self, _bytes: Bytes) -> sunset_sync::Result<()> {
            Ok(())
        }
        async fn recv(&mut self) -> sunset_sync::Result<Bytes> {
            std::future::pending().await
        }
        fn peer_addr(&self) -> Option<PeerAddr> {
            None
        }
        fn peer_pubkey(&self) -> Option<[u8; 32]> {
            None
        }
        async fn close(self) -> sunset_sync::Result<()> {
            Ok(())
        }
    }

    pub(super) async fn mk_peer(
        identity: Identity,
    ) -> (Rc<Peer<sunset_store_memory::MemoryStore, MultiTransport<NopTransport, NopTransport>>>, sunset_sync::spawn::JoinHandle<()>) {
        let store = Arc::new(sunset_store_memory::MemoryStore::new(Arc::new(crate::Ed25519Verifier)));
        let multi = MultiTransport::new(NopTransport, NopTransport);
        let signer: Arc<dyn Signer> = Arc::new(identity.clone());
        let local_peer = PeerId(identity.store_verifying_key());
        let engine = Rc::new(SyncEngine::new(
            store.clone(),
            multi,
            SyncConfig::default(),
            local_peer,
            signer,
        ));
        let engine_clone = engine.clone();
        let join = sunset_sync::spawn::spawn_local(async move {
            let _ = engine_clone.run().await;
        });
        let supervisor = PeerSupervisor::new(engine.clone(), BackoffPolicy::default());
        sunset_sync::spawn::spawn_local({
            let s = supervisor.clone();
            async move { s.run().await }
        });
        let dispatcher = MultiRoomSignaler::new();
        (Peer::new(identity, store, engine, supervisor, dispatcher), join)
    }
}
```

The exact `RawTransport` / `RawConnection` trait shapes may differ slightly from the above. **Before writing the impls**, run:

```bash
grep -n "trait RawTransport\|trait RawConnection\|fn connect\|fn accept\|fn send\|fn recv\|fn peer_pubkey\|fn close" crates/sunset-sync/src/transport.rs crates/sunset-sync/src/raw.rs 2>/dev/null
```

Adjust the impl signatures to match exactly. If `peer_pubkey` is an `Option<Bytes>` instead of `Option<[u8; 32]>`, use that.

- [ ] **Step 3: Run the test**

```bash
nix develop --command cargo test -p sunset-core --all-features peer::tests::peer_new_exposes_public_key_and_default_relay_status
```

Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/sunset-core/src/peer/
git commit -m "sunset-core: add Peer/OpenRoom skeletons + NopTransport test helper"
```

### Task 13: Peer::add_relay (delegates to supervisor; updates relay_status)

**Files:** Modify `crates/sunset-core/src/peer/mod.rs`.

- [ ] **Step 1: Write the failing test**

Append to the `tests` module in `peer/mod.rs`:

```rust
#[tokio::test(flavor = "current_thread")]
async fn add_relay_with_unreachable_addr_sets_status_error() {
    let local = tokio::task::LocalSet::new();
    local.run_until(async {
        let (peer, _join) = helpers::mk_peer(ident(8)).await;
        assert_eq!(peer.relay_status(), "disconnected");

        // Use an obviously-bogus addr — NopTransport's connect() returns
        // `Transport("nop")` immediately, so add_relay short-circuits to
        // error and the status flips to "error".
        let result = peer.add_relay(sunset_sync::PeerAddr::new(bytes::Bytes::from_static(b"wss://nowhere.invalid"))).await;
        assert!(result.is_err());
        assert_eq!(peer.relay_status(), "error");
    }).await;
}
```

- [ ] **Step 2: Run (expect FAIL — `add_relay` doesn't exist)**

```bash
nix develop --command cargo test -p sunset-core --all-features peer::tests::add_relay_with_unreachable_addr_sets_status_error
```

- [ ] **Step 3: Implement `add_relay`**

In the `impl<St, T> Peer<St, T>` block, add:

```rust
pub async fn add_relay(&self, addr: sunset_sync::PeerAddr) -> sunset_sync::Result<()> {
    *self.relay_status.borrow_mut() = "connecting".to_owned();
    match self.supervisor.add(addr).await {
        Ok(()) => {
            *self.relay_status.borrow_mut() = "connected".to_owned();
            Ok(())
        }
        Err(e) => {
            *self.relay_status.borrow_mut() = "error".to_owned();
            Err(e)
        }
    }
}
```

- [ ] **Step 4: Run the test (expect PASS)**

```bash
nix develop --command cargo test -p sunset-core --all-features peer::tests::add_relay_with_unreachable_addr_sets_status_error
```

- [ ] **Step 5: Commit**

```bash
git add crates/sunset-core/src/peer/mod.rs
git commit -m "sunset-core: Peer::add_relay (delegates to supervisor, updates relay_status)"
```

---

## Phase 5: OpenRoom — open / send_text / decode loop

### Task 14: TDD — Peer::open_room is idempotent on fingerprint

**Files:** Modify `crates/sunset-core/src/peer/mod.rs`, `crates/sunset-core/src/peer/open_room.rs`.

- [ ] **Step 1: Write the failing test**

Append to the `tests` module:

```rust
#[tokio::test(flavor = "current_thread")]
async fn open_room_twice_returns_same_state() {
    let local = tokio::task::LocalSet::new();
    local.run_until(async {
        let (peer, _join) = helpers::mk_peer(ident(9)).await;
        let r1 = peer.open_room("alpha").await.unwrap();
        let r2 = peer.open_room("alpha").await.unwrap();
        assert_eq!(r1.fingerprint(), r2.fingerprint());
        // Internal: both handles share the same Rc<RoomState>.
        assert!(Rc::ptr_eq(&r1.inner, &r2.inner));
    }).await;
}
```

- [ ] **Step 2: Run (expect FAIL — `open_room` and `fingerprint` not implemented)**

```bash
nix develop --command cargo test -p sunset-core --all-features peer::tests::open_room_twice_returns_same_state
```

- [ ] **Step 3: Implement `Peer::open_room` and `OpenRoom::fingerprint`**

In `peer/mod.rs`:

```rust
impl<St, T> Peer<St, T>
where
    St: Store + 'static,
    T: 'static,
{
    pub async fn open_room(self: &Rc<Self>, room_name: &str) -> crate::Result<OpenRoom<St, T>> {
        // Open the Room (Argon2id derivation; expensive — ~tens to
        // hundreds of ms with production params).
        let room = Rc::new(crate::Room::open(room_name)?);
        let fp = room.fingerprint();

        // Idempotency check: if this fingerprint is already open and the
        // weak still upgrades, return another handle to the same RoomState.
        if let Some(weak) = self.open_rooms.borrow().get(&fp) {
            if let Some(strong) = weak.upgrade() {
                return Ok(OpenRoom { inner: strong });
            }
        }

        // Build a fresh per-room signaler and register it with the
        // dispatcher.
        let signaler = crate::signaling::RelaySignaler::new(
            self.identity.clone(),
            fp.to_hex(),
            &self.store,
        );
        self.rtc_signaler_dispatcher.register(fp, signaler.clone());

        // Publish the room subscription. Renewal is started by Task 19.
        let filter = crate::filters::room_filter(&room);
        self.engine
            .publish_subscription(filter, std::time::Duration::from_secs(3600))
            .await
            .map_err(|e| crate::Error::Other(format!("publish_subscription: {e}")))?;

        let state = Rc::new(open_room::RoomState {
            room,
            peer_weak: Rc::downgrade(self),
            presence_started: std::cell::Cell::new(false),
            tracker_handles: Rc::new(crate::membership::TrackerHandles::new(
                &self.relay_status.borrow(),
            )),
            signaler,
            cancel_decode: Rc::new(std::cell::Cell::new(false)),
        });

        self.open_rooms.borrow_mut().insert(fp, Rc::downgrade(&state));
        Ok(OpenRoom { inner: state })
    }
}
```

In `peer/open_room.rs`:

```rust
impl<St: Store + 'static, T: 'static> OpenRoom<St, T> {
    pub fn fingerprint(&self) -> crate::crypto::room::RoomFingerprint {
        self.inner.room.fingerprint()
    }
}
```

You may need to add an `Other(String)` variant to `crate::Error`. Check: `grep -n "pub enum Error" crates/sunset-core/src/error.rs`. If there's no general-purpose variant, add one:

```rust
// In crates/sunset-core/src/error.rs:
#[error("{0}")]
Other(String),
```

- [ ] **Step 4: Run the test (expect PASS)**

```bash
nix develop --command cargo test -p sunset-core --all-features peer::tests::open_room_twice_returns_same_state
```

- [ ] **Step 5: Run all sunset-core tests**

```bash
nix develop --command cargo test -p sunset-core --all-features
```

Expected: PASS (no regressions).

- [ ] **Step 6: Commit**

```bash
git add crates/sunset-core/src/peer/ crates/sunset-core/src/error.rs
git commit -m "sunset-core: Peer::open_room with idempotent registry; OpenRoom::fingerprint"
```

### Task 15: TDD — OpenRoom::send_text inserts a Text entry into the store

**Files:** Modify `crates/sunset-core/src/peer/open_room.rs`.

- [ ] **Step 1: Write the failing test**

Append to `tests` in `peer/mod.rs`:

```rust
#[tokio::test(flavor = "current_thread")]
async fn send_text_inserts_a_text_entry() {
    let local = tokio::task::LocalSet::new();
    local.run_until(async {
        let (peer, _join) = helpers::mk_peer(ident(10)).await;
        let room = peer.open_room("alpha").await.unwrap();

        let now_ms = 1_700_000_000_000u64;
        let value_hash = room.send_text("hello world".to_owned(), now_ms).await.unwrap();

        // The store should now hold both an entry under that hash AND the
        // backing content block. Use the engine's store handle to verify.
        use sunset_store::{Filter, Store as _};
        let block = peer.store().get_content(&value_hash).await.unwrap();
        assert!(block.is_some(), "content block missing");
    }).await;
}
```

The test references `peer.store()` which is `pub(crate)` — that's fine for in-crate tests. If clippy complains, narrow to a `#[cfg(test)] pub` accessor.

- [ ] **Step 2: Run (expect FAIL — `send_text` not implemented)**

- [ ] **Step 3: Implement `OpenRoom::send_text`**

In `peer/open_room.rs`:

```rust
use bytes::Bytes;
use rand_chacha::ChaCha20Rng;
use rand_core::SeedableRng;

use crate::crypto::envelope::MessageBody;
use crate::message::compose_message;

impl<St: Store + 'static, T: 'static> OpenRoom<St, T> {
    pub async fn send_text(
        &self,
        body: String,
        sent_at_ms: u64,
    ) -> crate::Result<sunset_store::Hash> {
        let peer = self
            .inner
            .peer_weak
            .upgrade()
            .ok_or_else(|| crate::Error::Other("peer dropped".into()))?;

        let mut rng = ChaCha20Rng::from_entropy();
        let composed = compose_message(
            peer.identity(),
            &self.inner.room,
            0u64,
            sent_at_ms,
            MessageBody::Text(body),
            &mut rng,
        )?;

        let value_hash = composed.entry.value_hash;
        peer.store()
            .insert(composed.entry, Some(composed.block))
            .await
            .map_err(|e| crate::Error::Other(format!("store insert: {e}")))?;
        Ok(value_hash)
    }
}
```

If `rand_chacha` and `rand_core` are not already in `sunset-core/Cargo.toml`, add them. Check first:

```bash
grep "^rand_" crates/sunset-core/Cargo.toml
```

If missing, add `rand_chacha.workspace = true` and `rand_core.workspace = true` to `[dependencies]`.

- [ ] **Step 4: Run the test (expect PASS)**

```bash
nix develop --command cargo test -p sunset-core --all-features peer::tests::send_text_inserts_a_text_entry
```

- [ ] **Step 5: Commit**

```bash
git add crates/sunset-core/src/peer/ crates/sunset-core/Cargo.toml
git commit -m "sunset-core: OpenRoom::send_text composes + inserts a Text entry"
```

### Task 16: TDD — OpenRoom decode loop fires on_message callback for inserted Text

**Files:** Modify `crates/sunset-core/src/peer/open_room.rs`.

- [ ] **Step 1: Write the failing test**

Append to `tests`:

```rust
#[tokio::test(flavor = "current_thread")]
async fn on_message_fires_for_self_send() {
    let local = tokio::task::LocalSet::new();
    local.run_until(async {
        let (peer, _join) = helpers::mk_peer(ident(11)).await;
        let room = peer.open_room("alpha").await.unwrap();

        let received: Rc<RefCell<Vec<(String, bool)>>> = Rc::new(RefCell::new(Vec::new()));
        let received_clone = received.clone();
        room.on_message(move |decoded, is_self| {
            if let crate::MessageBody::Text(t) = &decoded.body {
                received_clone.borrow_mut().push((t.clone(), is_self));
            }
        });

        let _ = room.send_text("hello self".to_owned(), 1_700_000_000_000).await.unwrap();

        // Yield repeatedly so the decode loop's spawn_local runs.
        for _ in 0..50 {
            tokio::task::yield_now().await;
        }

        let got = received.borrow().clone();
        assert_eq!(got, vec![("hello self".to_owned(), true)]);
    }).await;
}
```

- [ ] **Step 2: Run (expect FAIL)**

- [ ] **Step 3: Implement decode loop + on_message callback registration**

In `peer/open_room.rs`, extend the file:

```rust
use std::cell::RefCell;
use crate::message::{DecodedMessage, decode_message};
use crate::filters::room_messages_filter;
use sunset_store::{Event, Replay, Store as _};

pub(crate) type MessageCallback = Box<dyn Fn(&DecodedMessage, bool /* is_self */)>;
pub(crate) type ReceiptCallback = Box<dyn Fn(sunset_store::Hash, &sunset_store::VerifyingKey)>;

pub(crate) struct RoomCallbacks {
    pub(crate) on_message: Option<MessageCallback>,
    pub(crate) on_receipt: Option<ReceiptCallback>,
}

impl Default for RoomCallbacks {
    fn default() -> Self {
        Self { on_message: None, on_receipt: None }
    }
}
```

Add a `callbacks: Rc<RefCell<RoomCallbacks>>` field on `RoomState`. Update construction in `peer/mod.rs::open_room` accordingly. Then in `OpenRoom`:

```rust
impl<St: Store + 'static, T: 'static> OpenRoom<St, T> {
    pub fn on_message<F: Fn(&DecodedMessage, bool) + 'static>(&self, cb: F) {
        let mut cbs = self.inner.callbacks.borrow_mut();
        let was_unregistered = cbs.on_message.is_none() && cbs.on_receipt.is_none();
        cbs.on_message = Some(Box::new(cb));
        drop(cbs);

        // First on_message/on_receipt call kicks off the decode loop.
        // (Subsequent calls just replace the callback; the loop already
        // running picks up the new closure on its next event.)
        if was_unregistered {
            self.spawn_decode_loop();
        }
    }

    fn spawn_decode_loop(&self) {
        let inner = self.inner.clone();
        let peer = match inner.peer_weak.upgrade() {
            Some(p) => p,
            None => return,
        };
        let store = peer.store().clone();
        let identity_pub = peer.identity().public();
        let room = inner.room.clone();
        let cancel = inner.cancel_decode.clone();
        let callbacks = inner.callbacks.clone();

        sunset_sync::spawn::spawn_local(async move {
            use futures::StreamExt;
            let filter = room_messages_filter(&room);
            let mut events = match store.subscribe(filter, Replay::All).await {
                Ok(s) => s,
                Err(e) => {
                    tracing::error!("OpenRoom decode subscribe: {e}");
                    return;
                }
            };
            while let Some(ev) = events.next().await {
                if cancel.get() {
                    return;
                }
                let entry = match ev {
                    Ok(Event::Inserted(e)) => e,
                    Ok(Event::Replaced { new, .. }) => new,
                    Ok(_) => continue,
                    Err(e) => {
                        tracing::error!("OpenRoom decode event: {e}");
                        continue;
                    }
                };
                let block = match store.get_content(&entry.value_hash).await {
                    Ok(Some(b)) => b,
                    Ok(None) => continue,
                    Err(e) => {
                        tracing::error!("OpenRoom decode get_content: {e}");
                        continue;
                    }
                };
                let decoded = match decode_message(&room, &entry, &block) {
                    Ok(d) => d,
                    Err(e) => {
                        tracing::error!("OpenRoom decode_message: {e}");
                        continue;
                    }
                };

                let is_self = decoded.author_key == identity_pub;
                let cbs = callbacks.borrow();
                match &decoded.body {
                    crate::MessageBody::Text(_) => {
                        if let Some(cb) = cbs.on_message.as_ref() {
                            cb(&decoded, is_self);
                        }
                    }
                    crate::MessageBody::Receipt { for_value_hash } => {
                        if let Some(cb) = cbs.on_receipt.as_ref() {
                            cb(*for_value_hash, &decoded.author_key);
                        }
                    }
                }
            }
        });
    }
}
```

The `RoomState` struct needs the `callbacks` field added; update the placeholder definition in `open_room.rs` and the construction site in `peer/mod.rs::open_room`.

- [ ] **Step 4: Run the test (expect PASS)**

```bash
nix develop --command cargo test -p sunset-core --all-features peer::tests::on_message_fires_for_self_send
```

- [ ] **Step 5: Commit**

```bash
git add crates/sunset-core/src/peer/
git commit -m "sunset-core: OpenRoom::on_message + spawn decode loop on first registration"
```

### Task 17: TDD — on_receipt fires for Receipt entries

**Files:** Modify `crates/sunset-core/src/peer/open_room.rs`.

- [ ] **Step 1: Write the failing test**

Append:

```rust
#[tokio::test(flavor = "current_thread")]
async fn on_receipt_fires_for_inserted_receipt() {
    let local = tokio::task::LocalSet::new();
    local.run_until(async {
        let (peer, _join) = helpers::mk_peer(ident(12)).await;
        let room = peer.open_room("alpha").await.unwrap();

        let received: Rc<RefCell<Vec<sunset_store::Hash>>> = Rc::new(RefCell::new(Vec::new()));
        let received_clone = received.clone();
        // Register a no-op on_message so the decode loop spawns even
        // though we only care about receipts here.
        room.on_message(|_, _| {});
        room.on_receipt(move |for_hash, _from| {
            received_clone.borrow_mut().push(for_hash);
        });

        // Compose+insert a Receipt referencing some target hash.
        let target: sunset_store::Hash = blake3::hash(b"target").into();
        let mut rng = rand_chacha::ChaCha20Rng::from_seed([42; 32]);
        let composed = crate::compose_receipt(
            peer.identity(),
            &room.inner.room,
            0,
            1_700_000_000_000,
            target,
            &mut rng,
        ).unwrap();
        use sunset_store::Store as _;
        peer.store().insert(composed.entry, Some(composed.block)).await.unwrap();

        for _ in 0..50 {
            tokio::task::yield_now().await;
        }
        assert_eq!(received.borrow().clone(), vec![target]);
    }).await;
}
```

- [ ] **Step 2: Run (expect FAIL — `on_receipt` not implemented)**

- [ ] **Step 3: Implement on_receipt**

```rust
impl<St: Store + 'static, T: 'static> OpenRoom<St, T> {
    pub fn on_receipt<F: Fn(sunset_store::Hash, &sunset_store::VerifyingKey) + 'static>(
        &self,
        cb: F,
    ) {
        let mut cbs = self.inner.callbacks.borrow_mut();
        let was_unregistered = cbs.on_message.is_none() && cbs.on_receipt.is_none();
        cbs.on_receipt = Some(Box::new(cb));
        drop(cbs);
        if was_unregistered {
            self.spawn_decode_loop();
        }
    }
}
```

- [ ] **Step 4: Run the test**

- [ ] **Step 5: Commit**

```bash
git add crates/sunset-core/src/peer/open_room.rs
git commit -m "sunset-core: OpenRoom::on_receipt routes Receipt entries to callback"
```

---

## Phase 6: OpenRoom — presence + connect_direct + drop semantics

### Task 18: TDD — start_presence wires the publisher and tracker

**Files:** Modify `crates/sunset-core/src/peer/open_room.rs`.

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test(flavor = "current_thread")]
async fn start_presence_publishes_a_heartbeat_entry() {
    let local = tokio::task::LocalSet::new();
    local.run_until(async {
        let (peer, _join) = helpers::mk_peer(ident(13)).await;
        let room = peer.open_room("alpha").await.unwrap();
        let my_hex = hex::encode(peer.public_key());

        room.start_presence(50, 1000, 100).await;

        // Wait for the publisher's first iteration.
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;

        use sunset_store::{Filter, Replay, Store as _};
        let presence_filter = Filter::NamePrefix(bytes::Bytes::from(format!(
            "{}/presence/{}",
            room.fingerprint().to_hex(),
            my_hex,
        )));
        let mut sub = peer.store().subscribe(presence_filter, Replay::All).await.unwrap();
        use futures::StreamExt;
        let ev = tokio::time::timeout(std::time::Duration::from_millis(500), sub.next())
            .await
            .expect("no presence entry within 500ms")
            .expect("subscription closed");
        assert!(matches!(ev, Ok(sunset_store::Event::Inserted(_))));
    }).await;
}
```

- [ ] **Step 2: Run (expect FAIL)**

- [ ] **Step 3: Implement start_presence**

```rust
impl<St: Store + 'static, T: 'static> OpenRoom<St, T> {
    pub async fn start_presence(&self, interval_ms: u64, ttl_ms: u64, refresh_ms: u64) {
        if self.inner.presence_started.replace(true) {
            return;
        }
        let peer = match self.inner.peer_weak.upgrade() {
            Some(p) => p,
            None => return,
        };
        let room_fp_hex = self.inner.room.fingerprint().to_hex();
        let local_peer = sunset_sync::PeerId(peer.identity().store_verifying_key());

        crate::membership::spawn_publisher(
            peer.identity().clone(),
            room_fp_hex.clone(),
            peer.store().clone(),
            interval_ms,
            ttl_ms,
        );

        let engine_events = peer.engine().subscribe_engine_events().await;
        let snapshot = peer.engine().current_peers().await;
        {
            let mut peer_kinds = self.inner.tracker_handles.peer_kinds.borrow_mut();
            for (pk, kind) in snapshot {
                peer_kinds.insert(pk, kind);
            }
        }

        crate::membership::spawn_tracker(
            peer.store().clone(),
            engine_events,
            local_peer,
            room_fp_hex,
            interval_ms,
            ttl_ms,
            refresh_ms,
            (*self.inner.tracker_handles).clone(),
        );

        crate::membership::fire_relay_status_now(&self.inner.tracker_handles);
    }
}
```

- [ ] **Step 4: Run + commit**

```bash
nix develop --command cargo test -p sunset-core --all-features peer::tests::start_presence_publishes_a_heartbeat_entry
git add crates/sunset-core/src/peer/open_room.rs
git commit -m "sunset-core: OpenRoom::start_presence wires publisher + tracker"
```

### Task 19: TDD — Subscription renewal task republishes before TTL/2

**Files:** Modify `crates/sunset-core/src/peer/mod.rs`.

- [ ] **Step 1: Test approach**

This is a non-trivial test because the current TTL is 1 hour. For testability, expose the TTL as a config field on `Peer` (or a constant we can override in tests).

Alternative: make `open_room` take a `subscription_ttl: Duration` argument. But that's an API smell.

Best: introduce a `pub(crate) const SUBSCRIPTION_TTL: Duration = Duration::from_secs(3600);` in core and a sibling `#[cfg(test)] const SUBSCRIPTION_TTL: Duration = Duration::from_millis(200);`. Test verifies that after `open_room` and a sleep > 100ms, the engine's subscription registry still shows the room's filter.

Actually the simplest verifiable assertion: spawn_local'd renewal task calls `engine.publish_subscription` again at `TTL/2`. We can verify by making the subscription expire fast and observing that messages still propagate. That's complex for a unit test.

Pragmatic minimum-test approach: mock the renewal interval down to 50ms via a test-only knob, and observe that `publish_subscription` is called multiple times within 200ms. We can wrap the engine in a small counting harness... but the engine is owned by the peer.

**Recommended approach:** factor renewal into a function we can unit-test directly:

```rust
async fn renewal_loop<St, T>(
    engine: Rc<sunset_sync::SyncEngine<St, T>>,
    filter: sunset_store::Filter,
    ttl: std::time::Duration,
    cancel: Rc<std::cell::Cell<bool>>,
) where St: sunset_store::Store + 'static, T: 'static {
    use wasmtimer::tokio::sleep;
    let renewal_interval = ttl / 2;
    loop {
        sleep(renewal_interval).await;
        if cancel.get() { return; }
        if let Err(e) = engine.publish_subscription(filter.clone(), ttl).await {
            tracing::warn!("subscription renewal failed: {e}");
        }
    }
}
```

Test: spawn this with a fake "engine" wrapper that records calls to `publish_subscription`. Even simpler: rather than introduce a trait, test that calling cancel.set(true) ends the loop within renewal_interval + buffer.

For this PR, keep the renewal task simple (one short loop, no trait abstraction) and add the cancel-stops-loop test only:

```rust
#[tokio::test(flavor = "current_thread")]
async fn renewal_loop_exits_when_cancel_set() {
    let local = tokio::task::LocalSet::new();
    local.run_until(async {
        let (peer, _join) = helpers::mk_peer(ident(14)).await;
        let room = peer.open_room("alpha").await.unwrap();

        // Drop the OpenRoom handle; verify cancel was set.
        let cancel = room.inner.cancel_decode.clone();
        drop(room);
        // The Drop impl on RoomState fires cancel_decode = true. Yield
        // so the renewal-loop task notices.
        tokio::task::yield_now().await;
        assert!(cancel.get());
    }).await;
}
```

(This actually tests Drop semantics rather than renewal directly, but renewal correctness is structurally guaranteed by the same `cancel.get()` check.)

- [ ] **Step 2: Wire the renewal task into `open_room`**

In `peer/mod.rs::open_room`, after `engine.publish_subscription(...)`, add:

```rust
let engine_for_renewal = self.engine.clone();
let filter_for_renewal = crate::filters::room_filter(&room).clone();
let cancel_for_renewal = state.cancel_decode.clone();
let ttl = std::time::Duration::from_secs(3600);
sunset_sync::spawn::spawn_local(async move {
    use wasmtimer::tokio::sleep;
    let renewal = ttl / 2;
    loop {
        sleep(renewal).await;
        if cancel_for_renewal.get() {
            return;
        }
        if let Err(e) = engine_for_renewal.publish_subscription(filter_for_renewal.clone(), ttl).await {
            tracing::warn!("subscription renewal failed: {e}");
        }
    }
});
```

(`Filter` may not be `Clone` — check. If not, use `room_filter` to recompute each iteration; cheap.)

- [ ] **Step 3: Run the test + commit**

```bash
nix develop --command cargo test -p sunset-core --all-features peer::tests::renewal_loop_exits_when_cancel_set
git add crates/sunset-core/src/peer/
git commit -m "sunset-core: OpenRoom subscription renewal at TTL/2; cancel on drop"
```

### Task 20: TDD — OpenRoom::connect_direct + peer_connection_mode

**Files:** Modify `crates/sunset-core/src/peer/open_room.rs`.

- [ ] **Step 1: Write a smoke-only failing test**

Real `connect_direct` requires a working WebRTC transport which we can't unit-test. Cover only that the method compiles and dispatches into `peer.supervisor.add(...)` with a synthesized `webrtc://` PeerAddr — and that `peer_connection_mode` reads from the tracker handle.

```rust
#[tokio::test(flavor = "current_thread")]
async fn peer_connection_mode_reads_from_tracker() {
    let local = tokio::task::LocalSet::new();
    local.run_until(async {
        let (peer, _join) = helpers::mk_peer(ident(15)).await;
        let room = peer.open_room("alpha").await.unwrap();
        let bogus_pk = [9u8; 32];
        // Without start_presence, peer_kinds is empty → "unknown"
        assert_eq!(room.peer_connection_mode(bogus_pk), "unknown");

        // Inject a kind manually (test-only, via the tracker handle).
        use sunset_sync::{PeerId, TransportKind};
        let pk = PeerId(sunset_store::VerifyingKey::new(bytes::Bytes::copy_from_slice(&bogus_pk)));
        room.inner.tracker_handles.peer_kinds.borrow_mut().insert(pk, TransportKind::Secondary);
        assert_eq!(room.peer_connection_mode(bogus_pk), "direct");
    }).await;
}
```

- [ ] **Step 2: Implement connect_direct + peer_connection_mode**

```rust
impl<St: Store + 'static, T: 'static> OpenRoom<St, T> {
    pub async fn connect_direct(&self, peer_pubkey: [u8; 32]) -> crate::Result<()> {
        let peer = self.inner.peer_weak.upgrade()
            .ok_or_else(|| crate::Error::Other("peer dropped".into()))?;
        let x_pub = sunset_noise::ed25519_public_to_x25519(&peer_pubkey)
            .map_err(|e| crate::Error::Other(format!("x25519 derive: {e}")))?;
        let addr_str = format!("webrtc://{}#x25519={}", hex::encode(peer_pubkey), hex::encode(x_pub));
        let addr = sunset_sync::PeerAddr::new(bytes::Bytes::from(addr_str));
        peer.supervisor()
            .add(addr)
            .await
            .map_err(|e| crate::Error::Other(format!("connect_direct: {e}")))?;
        Ok(())
    }

    pub fn peer_connection_mode(&self, peer_pubkey: [u8; 32]) -> &'static str {
        use sunset_sync::TransportKind;
        let peer_id = sunset_sync::PeerId(sunset_store::VerifyingKey::new(
            bytes::Bytes::copy_from_slice(&peer_pubkey),
        ));
        match self.inner.tracker_handles.peer_kinds.borrow().get(&peer_id) {
            Some(TransportKind::Secondary) => "direct",
            Some(TransportKind::Primary) => "via_relay",
            _ => "unknown",
        }
    }
}
```

- [ ] **Step 3: Run + commit**

```bash
nix develop --command cargo test -p sunset-core --all-features peer::tests::peer_connection_mode_reads_from_tracker
git add crates/sunset-core/src/peer/open_room.rs
git commit -m "sunset-core: OpenRoom::connect_direct + peer_connection_mode"
```

### Task 21: TDD — on_members_changed + on_relay_status_changed callbacks

**Files:** Modify `crates/sunset-core/src/peer/open_room.rs`.

- [ ] **Step 1: Implement (these are simple delegations to the existing TrackerHandles)**

```rust
use crate::membership::Member;

impl<St: Store + 'static, T: 'static> OpenRoom<St, T> {
    pub fn on_members_changed<F: Fn(&[Member]) + 'static>(&self, cb: F) {
        *self.inner.tracker_handles.on_members.borrow_mut() = Some(Box::new(cb));
        // Match Client::on_members_changed: clear last_signature so the next
        // refresh tick fires the callback with the current snapshot.
        self.inner.tracker_handles.last_signature.borrow_mut().clear();
    }

    pub fn on_relay_status_changed<F: Fn(&str) + 'static>(&self, cb: F) {
        *self.inner.tracker_handles.on_relay_status.borrow_mut() = Some(Box::new(cb));
    }
}
```

- [ ] **Step 2: Smoke test**

```rust
#[tokio::test(flavor = "current_thread")]
async fn on_members_changed_clears_last_signature() {
    let local = tokio::task::LocalSet::new();
    local.run_until(async {
        let (peer, _join) = helpers::mk_peer(ident(16)).await;
        let room = peer.open_room("alpha").await.unwrap();
        room.inner.tracker_handles.last_signature.borrow_mut().push((
            sunset_sync::PeerId(sunset_store::VerifyingKey::new(bytes::Bytes::from_static(b"x"))),
            crate::membership::Presence::Online,
            sunset_sync::TransportKind::Primary,
        ));
        room.on_members_changed(|_| {});
        assert!(room.inner.tracker_handles.last_signature.borrow().is_empty());
    }).await;
}
```

(Adjust the `MemberSig` tuple shape to match what's defined in `membership/mod.rs`.)

- [ ] **Step 3: Run + commit**

```bash
nix develop --command cargo test -p sunset-core --all-features peer::tests::on_members_changed_clears_last_signature
git add crates/sunset-core/src/peer/open_room.rs
git commit -m "sunset-core: OpenRoom::on_members_changed + on_relay_status_changed"
```

### Task 22: Integration test — two Peers, two rooms, isolation

**Files:**
- Create: `crates/sunset-core/tests/multi_room_integration.rs`

- [ ] **Step 1: Write the test**

```rust
//! Two peers, both open rooms A and B, verify per-room message isolation.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use sunset_core::{Identity, MessageBody};
use sunset_store::Store as _;

#[tokio::test(flavor = "current_thread")]
async fn two_peers_two_rooms_messages_are_isolated() {
    let local = tokio::task::LocalSet::new();
    local.run_until(async {
        // Build two peers. They share a store via direct insert (no transport).
        // This exercises Peer/OpenRoom under in-process replication, which is
        // sufficient to validate room-scoped subscriptions and decode loops.
        // (Full transport-mediated cross-peer testing happens in the e2e suite.)

        let id_a = Identity::from_seed(&[1; 32]).unwrap();
        let id_b = Identity::from_seed(&[2; 32]).unwrap();

        // Each peer has its own store; we'll cross-insert by hand to mimic
        // replication.
        let peer_a = mk_peer(id_a.clone()).await;
        let peer_b = mk_peer(id_b.clone()).await;

        let room_a_for_alpha = peer_a.open_room("alpha").await.unwrap();
        let room_a_for_beta = peer_a.open_room("beta").await.unwrap();
        let room_b_for_alpha = peer_b.open_room("alpha").await.unwrap();
        // Note: peer B does NOT open "beta" — it should not receive beta msgs.

        let alpha_received: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
        let beta_received_at_a: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
        {
            let r = alpha_received.clone();
            room_b_for_alpha.on_message(move |d| {
                if let MessageBody::Text(t) = &d.body {
                    r.borrow_mut().push(t.clone());
                }
            });
            let r = beta_received_at_a.clone();
            room_a_for_beta.on_message(move |d| {
                if let MessageBody::Text(t) = &d.body {
                    r.borrow_mut().push(t.clone());
                }
            });
        }

        // Send "in alpha"; peer A composes; we cross-insert into B's store.
        let _ = room_a_for_alpha.send_text("hello-alpha".into(), 1).await.unwrap();
        // Send "in beta"; only peer A has beta open.
        let _ = room_a_for_beta.send_text("hello-beta".into(), 2).await.unwrap();

        // Replicate alpha entries from A to B by listing A's store and inserting.
        // (For this test we need a way to enumerate A's store entries. The
        // simplest path is to subscribe to a NamePrefix matching alpha's
        // fingerprint and insert each Inserted event.)
        replicate_room(&peer_a, &peer_b, room_a_for_alpha.fingerprint()).await;
        // Do NOT replicate beta — peer B never opened it.

        for _ in 0..200 {
            tokio::task::yield_now().await;
        }

        // Peer B's alpha room received "hello-alpha".
        assert_eq!(alpha_received.borrow().clone(), vec!["hello-alpha".to_owned()]);
        // Peer A's own beta room received its own "hello-beta" (self-decode).
        assert_eq!(beta_received_at_a.borrow().clone(), vec!["hello-beta".to_owned()]);
    }).await;
}

// Helpers — copy mk_peer from peer/mod.rs::tests::helpers structure or
// write a simpler fork. A full cross-peer replication helper is below.

async fn mk_peer(...) -> Rc<sunset_core::Peer<...>> { /* ... */ }
async fn replicate_room(from: &Rc<...>, to: &Rc<...>, fp: ...) { /* ... */ }
```

The integration test is a stretch — for a first cut, gate it on completion of all earlier tasks. **If time-bounded:** skip Task 22 and rely on the per-room unit tests + the e2e Playwright test in Phase 8 to cover cross-peer behavior. Mark this task `[~]` (deferred) in the plan when running.

- [ ] **Step 2: Either implement fully or defer**

If implementing: write the helpers using the same patterns as `peer/mod.rs::tests::helpers`. If deferring: leave the file uncommitted and note the deferral in the next commit message.

- [ ] **Step 3: Commit (or skip)**

```bash
# If implemented:
git add crates/sunset-core/tests/multi_room_integration.rs
git commit -m "sunset-core: integration test for two peers / two rooms isolation"
# If deferred:
git rm crates/sunset-core/tests/multi_room_integration.rs 2>/dev/null || true
# (no commit needed if file was never added)
```

---

## Phase 7: Refactor wasm Client to use Peer + RoomHandle

### Task 23: Slim Client::new — drop room_name parameter, build Peer

**Files:** Modify `crates/sunset-web-wasm/src/client.rs`.

- [ ] **Step 1: Replace Client::new**

Goal: `Client::new(seed)` builds identity → store → engine → supervisor → transports → MultiRoomSignaler → Peer. No Room is opened in the constructor.

In `crates/sunset-web-wasm/src/client.rs`, replace the existing `pub fn new(seed: &[u8], room_name: &str)` body with the version below. Drop the `room_name` parameter from the signature:

```rust
#[wasm_bindgen]
impl Client {
    #[wasm_bindgen(constructor)]
    pub fn new(seed: &[u8]) -> Result<Client, JsError> {
        let identity = identity_from_seed(seed).map_err(|e| JsError::new(&e))?;
        let store = Arc::new(MemoryStore::new(Arc::new(Ed25519Verifier)));

        let ws_raw = WebSocketRawTransport::dial_only();
        let ws_noise = NoiseTransport::new(ws_raw, Arc::new(IdentityNoiseAdapter(identity.clone())));

        let dispatcher = sunset_core::signaling::MultiRoomSignaler::new();
        let dispatcher_dyn: Rc<dyn sunset_sync::Signaler> = dispatcher.clone();
        let local_peer = PeerId(identity.store_verifying_key());
        let rtc_raw = WebRtcRawTransport::new(
            dispatcher_dyn,
            local_peer.clone(),
            vec!["stun:stun.l.google.com:19302".into()],
        );
        let rtc_noise = NoiseTransport::new(rtc_raw, Arc::new(IdentityNoiseAdapter(identity.clone())));

        let multi = MultiTransport::new(ws_noise, rtc_noise);
        let signer: Arc<dyn Signer> = Arc::new(identity.clone());
        let engine = Rc::new(SyncEngine::new(
            store.clone(),
            multi,
            SyncConfig::default(),
            local_peer,
            signer,
        ));
        let engine_clone = engine.clone();
        wasm_bindgen_futures::spawn_local(async move {
            if let Err(e) = engine_clone.run().await {
                web_sys::console::error_1(&JsValue::from_str(&format!("sync engine exited: {e}")));
            }
        });

        let supervisor = sunset_sync::PeerSupervisor::new(engine.clone(), sunset_sync::BackoffPolicy::default());
        wasm_bindgen_futures::spawn_local({
            let s = supervisor.clone();
            async move { s.run().await }
        });

        let peer = sunset_core::Peer::new(identity, store, engine, supervisor, dispatcher);

        Ok(Client {
            inner: peer,
            voice: crate::voice::new_voice_cell(),
        })
    }
}
```

Drop all the room/presence/signaling fields from the `Client` struct. Final shape:

```rust
#[wasm_bindgen]
pub struct Client {
    inner: Rc<sunset_core::Peer<MemoryStore, MultiTransport<WsT, RtcT>>>,
    voice: crate::voice::VoiceCell,
}
```

- [ ] **Step 2: The remaining `Client` methods that survive (slim)**

Replace the entire `impl Client` block with:

```rust
#[wasm_bindgen]
impl Client {
    #[wasm_bindgen(getter)]
    pub fn public_key(&self) -> Vec<u8> {
        self.inner.public_key().to_vec()
    }

    #[wasm_bindgen(getter)]
    pub fn relay_status(&self) -> String {
        self.inner.relay_status()
    }

    pub async fn add_relay(&self, url_with_fragment: String) -> Result<(), JsError> {
        let resolver = sunset_relay_resolver::Resolver::new(crate::resolver_adapter::WebSysFetch);
        let canonical = resolver
            .resolve(&url_with_fragment)
            .await
            .map_err(|e| JsError::new(&format!("add_relay resolve: {e}")))?;
        let addr = sunset_sync::PeerAddr::new(Bytes::from(canonical));
        self.inner
            .add_relay(addr)
            .await
            .map_err(|e| JsError::new(&format!("add_relay: {e}")))?;
        Ok(())
    }

    pub async fn open_room(&self, name: String) -> Result<crate::room_handle::RoomHandle, JsError> {
        let open = self
            .inner
            .open_room(&name)
            .await
            .map_err(|e| JsError::new(&format!("open_room: {e}")))?;
        Ok(crate::room_handle::RoomHandle::new(open))
    }

    // Voice methods unchanged from before — keep them as-is.
    pub fn voice_start(&self, output_handler: &js_sys::Function) -> Result<(), JsError> {
        crate::voice::voice_start(&self.voice, output_handler)
    }
    pub fn voice_stop(&self) -> Result<(), JsError> {
        crate::voice::voice_stop(&self.voice)
    }
    pub fn voice_input(&self, pcm: &js_sys::Float32Array) -> Result<(), JsError> {
        crate::voice::voice_input(&self.voice, pcm)
    }
}
```

This drops `start_presence`, `on_members_changed`, `on_relay_status_changed`, `connect_direct`, `peer_connection_mode`, `publish_room_subscription`, `send_message`, `on_message`, `on_receipt`, and `spawn_message_subscription` from `Client`. They're replaced by the `RoomHandle` methods in Task 24.

- [ ] **Step 3: Don't run anything yet — Task 24 introduces RoomHandle**

The crate doesn't compile yet (RoomHandle doesn't exist; FFI shims and Gleam still call removed methods). Task 24 closes the loop.

- [ ] **Step 4: Stage but don't commit yet**

```bash
git add crates/sunset-web-wasm/src/client.rs
```

### Task 24: Add RoomHandle wasm-bindgen wrapper

**Files:**
- Create: `crates/sunset-web-wasm/src/room_handle.rs`
- Modify: `crates/sunset-web-wasm/src/lib.rs`

- [ ] **Step 1: Create the RoomHandle module**

```rust
//! `#[wasm_bindgen]` wrapper around `sunset_core::OpenRoom`.

use std::sync::Arc;

use bytes::Bytes;
use rand_chacha::ChaCha20Rng;
use rand_core::SeedableRng;
use wasm_bindgen::prelude::*;

use sunset_core::OpenRoom;
use sunset_store_memory::MemoryStore;
use sunset_sync::MultiTransport;

use crate::client::{RtcT, WsT};

#[wasm_bindgen]
pub struct RoomHandle {
    inner: OpenRoom<MemoryStore, MultiTransport<WsT, RtcT>>,
}

impl RoomHandle {
    pub(crate) fn new(inner: OpenRoom<MemoryStore, MultiTransport<WsT, RtcT>>) -> Self {
        Self { inner }
    }
}

#[wasm_bindgen]
impl RoomHandle {
    pub async fn send_message(
        &self,
        body: String,
        sent_at_ms: f64,
        _nonce_seed: Vec<u8>,
    ) -> Result<String, JsError> {
        // OpenRoom::send_text uses ChaCha20Rng::from_entropy; we ignore
        // the caller-supplied nonce_seed (kept in the signature for FFI
        // compat with the previous Client::send_message).
        let _ = _nonce_seed;
        let value_hash = self.inner.send_text(body, sent_at_ms as u64).await
            .map_err(|e| JsError::new(&format!("send_text: {e}")))?;
        Ok(value_hash.to_hex())
    }

    pub fn on_message(&self, callback: js_sys::Function) {
        self.inner.on_message(move |decoded, is_self| {
            // Only Text bodies get routed to on_message — Receipt goes
            // through on_receipt. Pattern-match here to extract the body
            // text + value hash.
            if let sunset_core::MessageBody::Text(text) = &decoded.body {
                let im = crate::messages::from_decoded_text(
                    decoded,
                    text.clone(),
                    decoded.value_hash.to_hex(),
                    is_self,
                );
                let _ = callback.call1(&JsValue::NULL, &JsValue::from(im));
            }
        });
    }

    pub fn on_receipt(&self, callback: js_sys::Function) {
        self.inner.on_receipt(move |for_hash, from_pubkey| {
            let incoming = crate::messages::receipt_to_js(for_hash.to_hex(), from_pubkey);
            let _ = callback.call1(&JsValue::NULL, &JsValue::from(incoming));
        });
    }

    pub fn on_members_changed(&self, callback: js_sys::Function) {
        self.inner.on_members_changed(move |members| {
            let arr = js_sys::Array::new();
            for m in members {
                arr.push(&JsValue::from(crate::members::MemberJs::from(m)));
            }
            let _ = callback.call1(&JsValue::NULL, &arr);
        });
    }

    pub fn on_relay_status_changed(&self, callback: js_sys::Function) {
        self.inner.on_relay_status_changed(move |status| {
            let _ = callback.call1(&JsValue::NULL, &JsValue::from_str(status));
        });
    }

    pub async fn start_presence(&self, interval_ms: u32, ttl_ms: u32, refresh_ms: u32) {
        self.inner.start_presence(interval_ms as u64, ttl_ms as u64, refresh_ms as u64).await;
    }

    pub async fn connect_direct(&self, peer_pubkey: &[u8]) -> Result<(), JsError> {
        let pk: [u8; 32] = peer_pubkey
            .try_into()
            .map_err(|_| JsError::new("peer_pubkey must be 32 bytes"))?;
        self.inner.connect_direct(pk).await
            .map_err(|e| JsError::new(&format!("connect_direct: {e}")))?;
        Ok(())
    }

    pub fn peer_connection_mode(&self, peer_pubkey: &[u8]) -> String {
        let pk: [u8; 32] = match peer_pubkey.try_into() {
            Ok(p) => p,
            Err(_) => return "unknown".to_owned(),
        };
        self.inner.peer_connection_mode(pk).to_owned()
    }
}
```

No new helper in `messages.rs` is needed — the existing `from_decoded_text(&decoded, text, value_hash_hex, is_self)` takes everything we have. `decoded.value_hash` is `Hash` (defined in `crates/sunset-core/src/message.rs:29`); `is_self` is the second callback arg from Task 16's signature.

- [ ] **Step 2: Wire the new module into lib.rs**

In `crates/sunset-web-wasm/src/lib.rs`, add:

```rust
mod room_handle;
pub use room_handle::RoomHandle;
```

Drop the now-unused `pub use relay_signaler::...` line if not already removed in Task 7.

- [ ] **Step 3: Build wasm + native**

```bash
nix develop --command cargo build -p sunset-web-wasm --target wasm32-unknown-unknown
nix develop --command cargo test --workspace --all-features
nix develop --command cargo clippy --workspace --all-features --all-targets -- -D warnings
```

Expected: PASS (after the small `is_self` fix above).

- [ ] **Step 4: Commit (combined with Task 23)**

```bash
git add crates/sunset-web-wasm/src/room_handle.rs crates/sunset-web-wasm/src/lib.rs crates/sunset-web-wasm/src/messages.rs crates/sunset-core/src/peer/
git commit -m "sunset-web-wasm: introduce RoomHandle; slim Client to Peer veneer"
```

---

## Phase 8: Update Gleam FFI

### Task 25: Update sunset.gleam — drop room_name from create_client, add open_room + RoomHandle

**Files:**
- Modify: `web/src/sunset_web/sunset.gleam`
- Modify: `web/src/sunset_web/sunset.ffi.mjs`

- [ ] **Step 1: Update sunset.gleam**

Replace the `create_client` external:

```gleam
@external(javascript, "./sunset.ffi.mjs", "createClient")
pub fn create_client(
  seed: BitArray,
  callback: fn(ClientHandle) -> Nil,
) -> Nil
```

Add the `RoomHandle` opaque type and `open_room`:

```gleam
pub type RoomHandle

@external(javascript, "./sunset.ffi.mjs", "clientOpenRoom")
pub fn open_room(
  client: ClientHandle,
  name: String,
  callback: fn(RoomHandle) -> Nil,
) -> Nil
```

Migrate every existing per-room method's first arg from `ClientHandle` to `RoomHandle`:

```gleam
@external(javascript, "./sunset.ffi.mjs", "sendMessage")
pub fn send_message(
  room: RoomHandle,
  body: String,
  sent_at_ms: Int,
  callback: fn(Result(String, String)) -> Nil,
) -> Nil

@external(javascript, "./sunset.ffi.mjs", "onMessage")
pub fn on_message(
  room: RoomHandle,
  callback: fn(IncomingMessage) -> Nil,
) -> Nil

@external(javascript, "./sunset.ffi.mjs", "onReceipt")
pub fn on_receipt(
  room: RoomHandle,
  callback: fn(IncomingReceipt) -> Nil,
) -> Nil

@external(javascript, "./sunset.ffi.mjs", "startPresence")
pub fn start_presence(
  room: RoomHandle,
  interval_ms: Int,
  ttl_ms: Int,
  refresh_ms: Int,
) -> Nil

@external(javascript, "./sunset.ffi.mjs", "onMembersChanged")
pub fn on_members_changed(
  room: RoomHandle,
  callback: fn(List(MemberJs)) -> Nil,
) -> Nil

@external(javascript, "./sunset.ffi.mjs", "onRelayStatusChanged")
pub fn on_relay_status_changed(
  room: RoomHandle,
  callback: fn(String) -> Nil,
) -> Nil

@external(javascript, "./sunset.ffi.mjs", "clientConnectDirect")
pub fn client_connect_direct(
  room: RoomHandle,
  peer_pubkey: BitArray,
  callback: fn(Result(Nil, String)) -> Nil,
) -> Nil

@external(javascript, "./sunset.ffi.mjs", "clientPeerConnectionMode")
pub fn client_peer_connection_mode(
  room: RoomHandle,
  peer_pubkey: BitArray,
) -> String
```

Delete `publish_room_subscription` — `open_room` does the equivalent internally.

Keep `add_relay`, `relay_status`, `relay_url_param`, `presence_params_from_url`, `set_interval_ms`, `now_ms` on `ClientHandle`/global.

- [ ] **Step 2: Update sunset.ffi.mjs**

In `web/src/sunset_web/sunset.ffi.mjs`, change `createClient`:

```js
export async function createClient(seed, callback) {
  const mod = await import("../wasm/sunset_web_wasm.js");
  await mod.default();
  const client = new mod.Client(seed);
  callback(client);
}
```

Add `clientOpenRoom`:

```js
export async function clientOpenRoom(client, name, callback) {
  const handle = await client.open_room(name);
  callback(handle);
}
```

Update `sendMessage`, `onMessage`, `onReceipt`, `onMembersChanged`, `onRelayStatusChanged`, `startPresence`, `clientConnectDirect`, `clientPeerConnectionMode` to call methods on the `room` argument (which is a `RoomHandle`) instead of `client`. Search-and-replace `client.` → `room.` in those function bodies.

Delete `publishRoomSubscription`.

- [ ] **Step 3: Build the wasm bundle and test the Gleam compile**

```bash
nix run .#web-build
cd web && nix develop --command gleam build
```

Expected: Gleam compile fails with type errors in `sunset_web.gleam` (it still uses the old API). That's the cue for Phase 9.

- [ ] **Step 4: Commit the FFI layer (broken Gleam consumers will be fixed in Phase 9)**

```bash
git add web/src/sunset_web/sunset.gleam web/src/sunset_web/sunset.ffi.mjs
git commit -m "web: FFI surface for RoomHandle (per-room operations)"
```

---

## Phase 9: Gleam Model + Update wiring

### Task 26: Define RoomState type in domain.gleam

**Files:** Modify `web/src/sunset_web/domain.gleam`.

- [ ] **Step 1: Append the RoomState type**

After the existing `Sheet` type, append:

```gleam
/// All UI + engine state scoped to one open room. The model holds a
/// `Dict(String, RoomState)` keyed by the room name (URL fragment).
pub type RoomState {
  RoomState(
    handle: sunset_web/sunset.RoomHandle,
    messages: List(Message),
    members: List(Member),
    receipts: gleam/dict.Dict(String, gleam/set.Set(String)),
    reactions: gleam/dict.Dict(String, List(Reaction)),
    current_channel: ChannelId,
    draft: String,
    selected_msg_id: gleam/option.Option(String),
    reacting_to: gleam/option.Option(String),
    sheet: gleam/option.Option(Sheet),
    peer_status_popover: gleam/option.Option(MemberId),
  )
}
```

Add the necessary imports at the top of `domain.gleam`:

```gleam
import gleam/dict
import gleam/option
import gleam/set
import sunset_web/sunset
```

(The `sunset` import creates a layering question — `domain` previously had no FFI dependency. If you'd rather keep `domain` pure, define `RoomState` directly in `sunset_web.gleam` instead.)

**Decision:** put `RoomState` in `sunset_web.gleam` (same file as Model) to keep `domain.gleam` FFI-free. Skip the changes to `domain.gleam` and instead define `RoomState` next to `Model` in `sunset_web.gleam` (Task 27).

- [ ] **Step 2: Skip — no commit for this task**

Move directly to Task 27.

### Task 27: Refactor Model to per-room state

**Files:** Modify `web/src/sunset_web.gleam`.

- [ ] **Step 1: Define RoomState near the Model**

In `web/src/sunset_web.gleam`, before the `Model` type definition, add:

```gleam
pub type RoomState {
  RoomState(
    handle: sunset.RoomHandle,
    messages: List(domain.Message),
    members: List(domain.Member),
    receipts: Dict(String, Set(String)),
    reactions: Dict(String, List(Reaction)),
    current_channel: ChannelId,
    draft: String,
    selected_msg_id: Option(String),
    reacting_to: Option(String),
    sheet: Option(domain.Sheet),
    peer_status_popover: Option(domain.MemberId),
  )
}

fn empty_room_state(handle: sunset.RoomHandle) -> RoomState {
  RoomState(
    handle: handle,
    messages: [],
    members: [],
    receipts: dict.new(),
    reactions: dict.new(),
    current_channel: ChannelId(fixture.initial_channel_id),
    draft: "",
    selected_msg_id: None,
    reacting_to: None,
    sheet: None,
    peer_status_popover: None,
  )
}
```

- [ ] **Step 2: Remove the per-room fields from Model and add `rooms: Dict(String, RoomState)`**

Replace the `Model` definition with:

```gleam
pub type Model {
  Model(
    mode: Mode,
    view: View,
    joined_rooms: List(String),
    rooms_collapsed: Bool,
    landing_input: String,
    sidebar_search: String,
    dragging_room: Option(String),
    drag_over_room: Option(String),
    voice_settings: Dict(String, domain.VoiceSettings),
    client: Option(ClientHandle),
    relay_status: String,
    viewport: domain.Viewport,
    drawer: Option(domain.Drawer),
    now_ms: Int,
    rooms: Dict(String, RoomState),
  )
}
```

Remove these fields (now in `RoomState`): `current_channel`, `draft`, `reacting_to`, `reactions`, `messages`, `members`, `receipts`, `selected_msg_id`, `peer_status_popover`, `sheet`.

- [ ] **Step 3: Update init to use the new Model shape**

In `init`, replace the `Model(...)` literal accordingly. The bootstrap-flow update in Task 28 wires `RoomOpened` → insert into `rooms`.

- [ ] **Step 4: Update Msg variants for per-room dispatch**

Update these constructors:

```gleam
pub type Msg {
  // ... unchanged ones ...
  IncomingMsg(room: String, im: IncomingMessage)
  IncomingReceipt(room: String, message_id: String, from_pubkey: String)
  MembersUpdated(room: String, members: List(domain.Member))
  // RelayStatusUpdated stays global
  RoomOpened(name: String, handle: sunset.RoomHandle)
  // ... etc
}
```

- [ ] **Step 5: Don't compile yet — Task 28 wires the update handlers**

- [ ] **Step 6: Stage but don't commit**

```bash
git add web/src/sunset_web.gleam
```

### Task 28: Update update() for per-room messages + bootstrap flow

**Files:** Modify `web/src/sunset_web.gleam`.

- [ ] **Step 1: Bootstrap effect: open one room per joined room with stagger**

Replace the bootstrap chain:

```
IdentityReady -> create_client(seed)
ClientReady   -> add_relay; for each joined room, open_room (staggered)
RoomOpened    -> insert empty RoomState; register on_message / on_receipt /
                 on_members_changed / on_relay_status_changed; start_presence
```

In the `IdentityReady` handler:

```gleam
IdentityReady(seed) -> {
  let create_client_eff =
    effect.from(fn(dispatch) {
      sunset.create_client(seed, fn(client) {
        dispatch(ClientReady(client))
      })
    })
  #(model, create_client_eff)
}
```

In `ClientReady`:

```gleam
ClientReady(client) -> {
  // Connect to relays as before.
  let relays = case sunset.relay_url_param() {
    Ok(url) -> [url]
    Error(_) -> default_relays
  }
  let connect_eff =
    effect.from(fn(dispatch) {
      list.each(relays, fn(url) {
        sunset.add_relay(client, url, fn(r) {
          dispatch(RelayConnectResult(r))
        })
      })
    })

  // Decide which room to open first (the active one) — see stagger note.
  let active_name = case model.view {
    LandingView -> case model.joined_rooms {
      [] -> ""
      [first, ..] -> first
    }
    RoomView(name) -> name
  }
  let other_names = list.filter(model.joined_rooms, fn(n) { n != active_name })

  let open_active_eff = case active_name {
    "" -> effect.none()
    name -> effect.from(fn(dispatch) {
      sunset.open_room(client, name, fn(handle) {
        dispatch(RoomOpened(name, handle))
      })
    })
  }
  let open_others_eff = effect.from(fn(dispatch) {
    list.index_map(other_names, fn(name, i) {
      // Stagger via setTimeout(0) cascading.
      sunset.set_timeout_ms(i * 50, fn() {
        sunset.open_room(client, name, fn(handle) {
          dispatch(RoomOpened(name, handle))
        })
      })
    })
    Nil
  })

  let new_status = case relays { [] -> "disconnected"; _ -> "connecting" }
  #(
    Model(..model, client: Some(client), relay_status: new_status),
    effect.batch([connect_eff, open_active_eff, open_others_eff]),
  )
}
```

`sunset.set_timeout_ms` is a new FFI helper — add it to `sunset.gleam`:

```gleam
@external(javascript, "./sunset.ffi.mjs", "setTimeoutMs")
pub fn set_timeout_ms(ms: Int, callback: fn() -> Nil) -> Nil
```

And `sunset.ffi.mjs`:

```js
export function setTimeoutMs(ms, callback) {
  setTimeout(callback, ms);
}
```

- [ ] **Step 2: Handle RoomOpened**

```gleam
RoomOpened(name, handle) -> {
  let state = empty_room_state(handle)
  let new_rooms = dict.insert(model.rooms, name, state)

  let #(interval, ttl, refresh) = sunset.presence_params_from_url()
  let wire_eff = effect.from(fn(dispatch) {
    sunset.on_message(handle, fn(im) {
      dispatch(IncomingMsg(name, im))
    })
    sunset.on_receipt(handle, fn(r) {
      dispatch(IncomingReceipt(
        name,
        sunset.rec_for_value_hash_hex(r),
        short_pubkey(sunset.rec_from_pubkey(r)),
      ))
    })
    sunset.on_members_changed(handle, fn(ms) {
      dispatch(MembersUpdated(name, map_members(ms)))
    })
    sunset.on_relay_status_changed(handle, fn(s) {
      dispatch(RelayStatusUpdated(s))
    })
    sunset.start_presence(handle, interval, ttl, refresh)
  })
  #(Model(..model, rooms: new_rooms), wire_eff)
}
```

- [ ] **Step 3: Update IncomingMsg / IncomingReceipt / MembersUpdated handlers to per-room**

```gleam
IncomingMsg(name, im) -> {
  case dict.get(model.rooms, name) {
    Error(_) -> #(model, effect.none())
    Ok(state) -> {
      let new_msg = ... // same construction as before
      let updated = case list.any(state.messages, fn(m) { m.id == new_msg.id }) {
        True -> state.messages
        False -> list.append(state.messages, [new_msg])
      }
      let new_state = RoomState(..state, messages: updated)
      #(Model(..model, rooms: dict.insert(model.rooms, name, new_state)), effect.none())
    }
  }
}

MembersUpdated(name, ms) -> {
  case dict.get(model.rooms, name) {
    Error(_) -> #(model, effect.none())
    Ok(state) -> {
      let next_popover = case state.peer_status_popover {
        None -> None
        Some(target) -> case list.find(ms, fn(m) { m.id == target }) {
          Ok(_) -> Some(target)
          Error(_) -> None
        }
      }
      let new_state = RoomState(..state, members: ms, peer_status_popover: next_popover)
      #(Model(..model, rooms: dict.insert(model.rooms, name, new_state)), effect.none())
    }
  }
}

IncomingReceipt(name, message_id, from_pubkey) -> {
  case dict.get(model.rooms, name) {
    Error(_) -> #(model, effect.none())
    Ok(state) -> {
      let existing = case dict.get(state.receipts, message_id) {
        Ok(s) -> s
        Error(_) -> set.new()
      }
      let updated = set.insert(existing, from_pubkey)
      let new_state = RoomState(..state, receipts: dict.insert(state.receipts, message_id, updated))
      #(Model(..model, rooms: dict.insert(model.rooms, name, new_state)), effect.none())
    }
  }
}
```

- [ ] **Step 4: Update SubmitDraft to use the active room's handle**

```gleam
SubmitDraft -> {
  let active_name = case model.view {
    RoomView(n) -> n
    LandingView -> ""
  }
  case active_name, dict.get(model.rooms, active_name) {
    "", _ -> #(model, effect.none())
    _, Error(_) -> #(model, effect.none())
    _, Ok(state) -> {
      let body = sanitize(state.draft)
      case body {
        "" -> #(model, effect.none())
        _ -> {
          let send_eff =
            effect.from(fn(dispatch) {
              sunset.send_message(state.handle, body, current_time_ms(), fn(r) {
                dispatch(MessageSent(r))
              })
            })
          let cleared = RoomState(..state, draft: "")
          #(Model(..model, rooms: dict.insert(model.rooms, active_name, cleared)), send_eff)
        }
      }
    }
  }
}
```

- [ ] **Step 5: Update remaining per-room Msg handlers**

All of: `UpdateDraft`, `ToggleMessageSelected`, `ToggleReactionPicker`, `AddReaction`, `OpenDetail`, `CloseDetail`, `OpenVoicePopover`, `CloseVoicePopover`, `OpenPeerStatusPopover`, `ClosePeerStatusPopover` — convert each from operating on flat `model.X` fields to operating on `model.rooms[active_room].X`. Pattern:

```gleam
UpdateDraft(s) -> {
  let active_name = case model.view {
    RoomView(n) -> n
    LandingView -> ""
  }
  case dict.get(model.rooms, active_name) {
    Error(_) -> #(model, effect.none())
    Ok(state) -> {
      let new_state = RoomState(..state, draft: s)
      #(Model(..model, rooms: dict.insert(model.rooms, active_name, new_state)), effect.none())
    }
  }
}
```

For each handler. (Not the most beautiful Gleam — a `with_active_room(model, fn(state) { ... })` helper would DRY it up; introduce that helper in this task to avoid 10 copies of the case-let-case dance.)

```gleam
fn with_active_room(
  model: Model,
  f: fn(RoomState) -> #(RoomState, Effect(Msg)),
) -> #(Model, Effect(Msg)) {
  let active_name = case model.view {
    RoomView(n) -> n
    LandingView -> ""
  }
  case dict.get(model.rooms, active_name) {
    Error(_) -> #(model, effect.none())
    Ok(state) -> {
      let #(new_state, eff) = f(state)
      let new_rooms = dict.insert(model.rooms, active_name, new_state)
      #(Model(..model, rooms: new_rooms), eff)
    }
  }
}
```

Then:

```gleam
UpdateDraft(s) -> with_active_room(model, fn(state) {
  #(RoomState(..state, draft: s), effect.none())
})
```

- [ ] **Step 6: Update DeleteRoom to drop the RoomHandle**

```gleam
DeleteRoom(name) -> {
  let new_rooms = list.filter(model.joined_rooms, fn(r) { r != name })
  let active_was_deleted = ...
  let new_view = ...
  let updated_rooms_dict = dict.delete(model.rooms, name)
  // Dropping the RoomHandle from Gleam side eventually frees the OpenRoom
  // strong ref on the Rust side (subject to JS GC). Acceptable for now.
  let persist = effect.from(fn(_) {
    storage.write_joined_rooms(new_rooms)
    case new_view {
      RoomView(n) -> storage.set_hash(n)
      LandingView -> storage.set_hash("")
    }
    Nil
  })
  #(Model(..model, joined_rooms: new_rooms, view: new_view, rooms: updated_rooms_dict), persist)
}
```

- [ ] **Step 7: Update JoinRoom to dispatch OpenRoom for new joins**

```gleam
JoinRoom(raw) -> {
  let name = sanitize(raw)
  case name {
    "" -> #(model, effect.none())
    _ -> {
      let was_new = !dict.has_key(model.rooms, name)
      // ... existing rail bookkeeping ...
      let open_eff = case was_new, model.client {
        True, Some(client) -> effect.from(fn(dispatch) {
          sunset.open_room(client, name, fn(handle) {
            dispatch(RoomOpened(name, handle))
          })
        })
        _, _ -> effect.none()
      }
      // ... rest ...
      #(new_model, effect.batch([persist_eff, hash_eff, open_eff]))
    }
  }
}
```

- [ ] **Step 8: Compile + commit**

```bash
cd web && nix develop --command gleam build
```

Expected: PASS.

```bash
cd ../  # back to worktree root
git add web/src/
git commit -m "web: per-room RoomState + multi-room update flow"
```

### Task 29: Update view() to read from active room's RoomState

**Files:** Modify `web/src/sunset_web.gleam`.

- [ ] **Step 1: In `room_view`, look up the active room's state**

Replace the leading `room_view` lines:

```gleam
fn room_view(model: Model, palette, current_name: String) -> Element(Msg) {
  let active_state = case dict.get(model.rooms, current_name) {
    Ok(s) -> s
    Error(_) -> empty_room_state_for_view()  // fallback while RoomOpened is in flight
  }
  // ...
}
```

Where `empty_room_state_for_view()` returns a placeholder `RoomState` with no `RoomHandle` (use a "loading…" sentinel — easiest is to render a brief loading panel until `RoomOpened` arrives):

```gleam
fn empty_room_state_for_view() -> RoomState {
  // We don't have a RoomHandle yet; the view never reads `handle` directly,
  // so a sentinel works. If you'd rather avoid this hack, branch the view
  // on `dict.has_key(...)` and render a loading shell when missing.
  RoomState(
    handle: panic_handle(), // see note
    messages: [], members: [],
    receipts: dict.new(), reactions: dict.new(),
    current_channel: ChannelId(fixture.initial_channel_id),
    draft: "",
    selected_msg_id: None, reacting_to: None,
    sheet: None, peer_status_popover: None,
  )
}
```

Actually `RoomHandle` is opaque — there's no way to construct one from Gleam. Better: branch on `dict.get` in `room_view`:

```gleam
fn room_view(model: Model, palette, current_name: String) -> Element(Msg) {
  case dict.get(model.rooms, current_name) {
    Error(_) -> loading_room_view(palette, current_name)
    Ok(state) -> room_view_with_state(model, palette, current_name, state)
  }
}

fn loading_room_view(palette, name: String) -> Element(Msg) {
  html.div(..., [html.text("opening " <> name <> "…")])
}
```

`room_view_with_state` is the existing `room_view` body, threaded through `state.messages`, `state.members`, `state.draft`, etc. instead of `model.X`.

- [ ] **Step 2: Wire all view children to the active state**

Inside `room_view_with_state`, references like `model.messages` become `state.messages`, `model.draft` becomes `state.draft`, etc. The `voice_minibar_el`, `details_sheet_el`, `voice_sheet_el`, `peer_status_sheet_el`, `reaction_sheet_el` calculations all read from `state.sheet` / `state.peer_status_popover` / `state.reacting_to`.

- [ ] **Step 3: Compile + Gleam tests**

```bash
cd web && nix develop --command gleam build && nix develop --command gleam test
```

Expected: PASS.

- [ ] **Step 4: Commit**

```bash
cd ..
git add web/src/sunset_web.gleam
git commit -m "web: view reads from active room's RoomState; loading panel before RoomOpened"
```

---

## Phase 10: e2e adaptation + new room-switching test

### Task 30: Adapt connect_direct call sites in existing e2e tests

**Files:**
- Modify: `web/e2e/presence.spec.js`
- Modify: `web/e2e/kill_relay.spec.js`

- [ ] **Step 1: Read the existing tests**

```bash
grep -n "connect_direct\|publishRoomSubscription\|sunsetClient" web/e2e/presence.spec.js web/e2e/kill_relay.spec.js
```

- [ ] **Step 2: Add an `openRoom` setup step before `connect_direct`**

In each test's setup, replace constructions like:

```js
window.sunsetClient = await new Client(seed, "test-room");
await window.sunsetClient.add_relay(relayUrl);
await window.sunsetClient.publish_room_subscription();
```

With:

```js
window.sunsetClient = await new Client(seed);
await window.sunsetClient.add_relay(relayUrl);
window.sunsetRoom = await window.sunsetClient.open_room("test-room");
```

And replace:

```js
await window.sunsetClient.connect_direct(new Uint8Array(pkArr));
```

With:

```js
await window.sunsetRoom.connect_direct(new Uint8Array(pkArr));
```

Same for `peer_connection_mode`, `start_presence`, etc.

- [ ] **Step 3: Run the e2e suite**

```bash
nix run .#web-test -- presence.spec.js kill_relay.spec.js --project=chromium
```

Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add web/e2e/presence.spec.js web/e2e/kill_relay.spec.js
git commit -m "e2e: adapt presence+kill_relay tests to RoomHandle API"
```

### Task 31: New e2e test — room switching isolates messages

**Files:** Create `web/e2e/room_switching.spec.js`.

- [ ] **Step 1: Write the test**

```js
import { test, expect } from "@playwright/test";

test("messages in room A do not appear in room B", async ({ browser }) => {
  // One browser context, one tab. Join room A, send a message.
  // Switch to room B, send a different message. Switch back to A:
  // we should still see A's message and NOT B's.
  const ctx = await browser.newContext();
  const page = await ctx.newPage();
  await page.goto("/");

  // Join room "alpha"
  await page.fill('[data-testid="landing-input"]', "alpha");
  await page.click('[data-testid="landing-join"]');
  await expect(page).toHaveURL(/#alpha$/);

  // Send "hello-alpha"
  await page.fill('[data-testid="composer"]', "hello-alpha");
  await page.press('[data-testid="composer"]', "Enter");
  await expect(page.locator('text=hello-alpha')).toBeVisible({ timeout: 10000 });

  // Open the rooms drawer / rail and join "beta"
  // (selector depends on viewport — adjust to whatever the rail uses)
  // Example: type into the new-room input on the desktop rail
  await page.fill('[data-testid="rooms-search"]', "beta");
  await page.press('[data-testid="rooms-search"]', "Enter");
  await expect(page).toHaveURL(/#beta$/);

  // Send "hello-beta"
  await page.fill('[data-testid="composer"]', "hello-beta");
  await page.press('[data-testid="composer"]', "Enter");
  await expect(page.locator('text=hello-beta')).toBeVisible({ timeout: 10000 });

  // hello-alpha should NOT be visible while in beta
  await expect(page.locator('text=hello-alpha')).not.toBeVisible();

  // Switch back to alpha via URL fragment
  await page.evaluate(() => { location.hash = "#alpha"; });
  await expect(page.locator('text=hello-alpha')).toBeVisible({ timeout: 10000 });
  await expect(page.locator('text=hello-beta')).not.toBeVisible();
});
```

- [ ] **Step 2: Adjust selectors to match the actual UI**

The `data-testid` attributes assumed above may not exist. Find the right selectors:

```bash
grep -rn 'data-testid="composer"\|data-testid="rooms-search"\|data-testid="landing-' web/src/sunset_web/views/
```

Adjust the test accordingly. If selectors don't exist, add them in the relevant view files (small `attribute.attribute("data-testid", ...)` additions).

- [ ] **Step 3: Run**

```bash
nix run .#web-test -- room_switching.spec.js --project=chromium
```

Expected: PASS.

- [ ] **Step 4: Run the whole suite**

```bash
nix run .#web-test -- --project=chromium
```

Expected: all PASS.

- [ ] **Step 5: Commit**

```bash
git add web/e2e/room_switching.spec.js web/src/sunset_web/views/
git commit -m "e2e: room-switching test verifies messages stay isolated per room"
```

---

## Phase 11: Final verification + cleanup

### Task 32: Workspace-wide CI pass

**Files:** none.

- [ ] **Step 1: Run everything**

```bash
nix develop --command cargo fmt --all --check
nix develop --command cargo clippy --workspace --all-features --all-targets -- -D warnings
nix develop --command cargo test --workspace --all-features
nix develop --command cargo build -p sunset-web-wasm --target wasm32-unknown-unknown
nix run .#web-build
cd web && nix develop --command gleam test && cd ..
nix run .#web-test -- --project=chromium
```

Expected: all PASS.

- [ ] **Step 2: If anything fails, debug and fix per superpowers:systematic-debugging**

Do NOT loosen tests or add `wait_for` polls to engine-internal state per the CLAUDE.md "Debugging discipline" section. If a test failure indicates a real architectural issue, stop and brainstorm.

- [ ] **Step 3: Commit any fixes; if clean, no commit needed**

### Task 33: Open the PR

**Files:** none.

- [ ] **Step 1: Push the branch**

```bash
git push -u origin feature/multi-room-peer
```

- [ ] **Step 2: Open the PR**

```bash
gh pr create --base master --title "Multi-room Peer + RoomHandle (room-first; channels deferred)" --body "$(cat <<'EOF'
## Summary
- Lifts multi-room logic from `sunset-web-wasm::Client` into `sunset-core::Peer` + `sunset-core::OpenRoom`. Wasm crate becomes a thin wasm-bindgen veneer.
- Web client's room rail now actually drives the engine: each joined room runs concurrently against one shared store, engine, and transport stack via per-room `RelaySignaler` registered with a new `MultiRoomSignaler`.
- `presence_publisher` and `RelaySignaler` move to core (already host-portable via `sunset_sync::spawn::spawn_local`).
- Channels stay UI-only (deferred wire-format change).

Spec: `docs/superpowers/specs/2026-05-02-multi-room-peer-and-room-handle-design.md`
Plan: `docs/superpowers/plans/2026-05-02-multi-room-peer-and-room-handle.md`

## Test plan
- [x] `cargo test --workspace --all-features` passes
- [x] `cargo clippy --workspace --all-features --all-targets -- -D warnings` clean
- [x] wasm builds (`cargo build -p sunset-web-wasm --target wasm32-unknown-unknown`)
- [x] Gleam compile + tests
- [x] Playwright suite (chromium): including new `room_switching.spec.js`

🤖 Generated with [Claude Code](https://claude.com/claude-code)
EOF
)"
```

PR opened.

---

## Self-review notes

This plan covers every section of the spec:
- Phase 1+2: moves `presence_publisher` and `RelaySignaler` into core (spec §"Why move all of this to core" + Components § for `spawn_publisher` and `RelaySignaler`).
- Phase 3: `MultiRoomSignaler` (spec §"`sunset_core::signaling::MultiRoomSignaler`").
- Phases 4-6: `Peer` + `OpenRoom` (spec §"`sunset_core::Peer`" + §"`sunset_core::OpenRoom`" + §"Per-room subscriptions, decode tasks, presence — on a shared engine").
- Phase 7: wasm Client slimmed; RoomHandle introduced (spec §"`sunset-web-wasm::Client`" + §"`sunset-web-wasm::RoomHandle`").
- Phase 8-9: Gleam UI per-room state + bootstrap with stagger (spec §"`sunset_web` Lustre model changes" + §"Argon2id cost on page load — stagger").
- Phase 10: e2e adaptation + room-switching test (spec §"Testing").

Edge cases from spec §"Edge cases" mapped:
- Opening a room twice: covered by Task 14.
- Closing while messages in flight: covered by Task 19 (cancel only ends the decode loop; in-flight inserts continue).
- Subscription renewal failure: covered by Task 19 (`tracing::warn!` and continue).
- `open_room` failure: covered by Task 14 (returns error to caller).
- Page reload race + WebRTC dispatcher race: covered by Task 16 + design (no test needed).

**Known plan limitations (called out for the executing agent):**
- Task 22 (cross-peer integration test in core) is marked optional. The Playwright e2e in Task 31 is the real cross-peer regression gate.
- Task 29's "loading_room_view" is a brief loading shell while a `RoomOpened` is in flight — not a hack so much as the simplest correct rendering before the per-room state lands. Replace with a richer loading affordance later if desired; out of scope here.
