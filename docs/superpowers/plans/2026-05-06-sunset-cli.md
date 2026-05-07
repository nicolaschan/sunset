# sunset-cli Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a native ratatui chat client (`sunset-cli`) that talks to a sunset.chat relay over WebSocket or WebTransport, mirroring what the Gleam web client does today for chat. Voice is explicitly deferred to a follow-up plan.

**Architecture:** New workspace crate `sunset-cli` containing a host-agnostic `core::Client` (wraps `sunset_core::Peer<MemoryStore, FallbackTransport<NoiseTransport<WtNative>, NoiseTransport<WsNative>>>`) plus a thin ratatui UI layer driven by a `/command` parser. The transport pattern mirrors `sunset-web-wasm/src/client.rs` minus WebRTC.

**Tech Stack:** Rust 2024 + tokio (single-threaded `LocalSet`), ratatui + crossterm for the TUI, clap for argument parsing, chrono for local-time formatting, dirs for config-path resolution, reqwest for the relay descriptor fetch. All native-only — the crate does not target wasm32.

---

## File structure

```
crates/sunset-cli/
├── Cargo.toml
├── src/
│   ├── lib.rs               # public surface (re-exports for tests + bin)
│   ├── main.rs              # binary entry point (tokio::main, LocalSet)
│   ├── identity.rs          # load_or_generate identity from disk
│   ├── resolver_adapter.rs  # ReqwestFetch HttpFetch impl (port of sunset-relay's)
│   ├── build.rs             # type aliases + build_peer factory (NOT a build script)
│   ├── client.rs            # core::Client: high-level Peer wrapper
│   ├── command.rs           # /command parser → Command enum
│   ├── dispatch.rs          # Command → Client side effects
│   ├── ui/
│   │   ├── mod.rs           # ratatui App; orchestrates render + input
│   │   ├── render.rs        # pure rendering of TopState/RoomView → Frame
│   │   └── input.rs         # crossterm event → Command + composer state
│   └── view.rs              # snapshot types passed from Client to UI (TopState, RoomView, …)
└── tests/
    ├── helpers/
    │   └── mod.rs           # spin up Relay, build a CLI Client, common harness
    ├── roundtrip_ws.rs      # 2 clients chat over wss://
    ├── roundtrip_wt.rs      # 2 clients chat over wt://
    └── command_dispatch.rs  # /commands change Client state correctly
```

Each file has one job; the split between `client.rs` (state) and `dispatch.rs` (commands → state) is what lets `command_dispatch.rs` test commands without a UI.

---

## Cross-cutting style notes

- **No `#[allow(clippy::...)]` / `#[expect(clippy::...)]` anywhere.** CI greps for them and fails. Fix lints at the source — refactor a signature, pick a different primitive, rename a constructor, etc.
- **Every dependency goes through `flake.nix`.** clap, ratatui, crossterm, chrono, dirs, reqwest, tokio all already build via the existing `rustToolchain` (no system deps). No flake change is required for v1; if any task discovers one is needed, add it before committing.
- **`?Send` everywhere.** The `Peer`/`Engine` are `?Send`. Run inside a `tokio::task::LocalSet`. Tests use `#[tokio::test(flavor = "current_thread")]` + `LocalSet::run_until` (see `crates/sunset-relay/tests/` for the pattern).
- **Use `tracing` for logs**, not `eprintln!`. Configure via `tracing_subscriber::fmt::layer()` writing to `std::io::stderr` so the TUI on stdout isn't polluted.

---

## Task 1: Scaffold the crate

**Files:**
- Create: `crates/sunset-cli/Cargo.toml`
- Create: `crates/sunset-cli/src/lib.rs`
- Create: `crates/sunset-cli/src/main.rs`
- Modify: `Cargo.toml` (workspace members + new workspace deps)

- [ ] **Step 1: Add new workspace deps to root `Cargo.toml`**

In `[workspace.dependencies]`, add:

```toml
ratatui = { version = "0.29", default-features = false, features = ["crossterm"] }
crossterm = "0.28"
dirs = "5"
chrono = { version = "0.4", default-features = false, features = ["clock"] }
```

Add to the `members = [...]` array: `"crates/sunset-cli"`.

- [ ] **Step 2: Write `crates/sunset-cli/Cargo.toml`**

```toml
[package]
name = "sunset-cli"
version.workspace = true
edition.workspace = true
license.workspace = true
rust-version.workspace = true

[lints]
workspace = true

[[bin]]
name = "sunset-cli"
path = "src/main.rs"

[dependencies]
async-trait.workspace = true
bytes.workspace = true
chrono.workspace = true
clap.workspace = true
crossterm.workspace = true
dirs.workspace = true
futures.workspace = true
hex.workspace = true
ratatui.workspace = true
rand_core = { workspace = true, features = ["getrandom"] }
reqwest.workspace = true
sunset-core.workspace = true
sunset-noise.workspace = true
sunset-relay-resolver.workspace = true
sunset-store.workspace = true
sunset-store-memory.workspace = true
sunset-sync.workspace = true
sunset-sync-webtransport-native.workspace = true
sunset-sync-ws-native.workspace = true
thiserror.workspace = true
tokio = { workspace = true, features = ["io-util", "macros", "rt", "rt-multi-thread", "signal", "fs", "sync", "net", "time"] }
tracing.workspace = true
tracing-subscriber.workspace = true
zeroize.workspace = true

[dev-dependencies]
sunset-relay = { workspace = true, features = ["test-helpers"] }
sunset-sync = { workspace = true, features = ["test-helpers"] }
tempfile.workspace = true
```

- [ ] **Step 3: Write `crates/sunset-cli/src/lib.rs`**

```rust
//! sunset-cli: native ratatui chat client.
//!
//! See `docs/superpowers/specs/2026-05-06-sunset-cli-design.md`.
//! v1 ships chat / rooms / peers / relay management. Voice is
//! deferred — see the spec's "Out of scope" section.

pub mod build;
pub mod client;
pub mod command;
pub mod dispatch;
pub mod identity;
pub mod resolver_adapter;
pub mod ui;
pub mod view;
```

- [ ] **Step 4: Write `crates/sunset-cli/src/main.rs` (placeholder)**

```rust
fn main() {
    eprintln!("sunset-cli: placeholder — main() will be filled in by Task 12");
    std::process::exit(2);
}
```

- [ ] **Step 5: Create empty module files**

For each of `build.rs`, `client.rs`, `command.rs`, `dispatch.rs`, `identity.rs`, `resolver_adapter.rs`, `view.rs`, write a single placeholder doc comment so `lib.rs` compiles:

```rust
//! Stub — implemented by a later task.
```

For `ui/mod.rs`:

```rust
//! Stub — implemented by a later task.
pub mod render;
pub mod input;
```

And `ui/render.rs`, `ui/input.rs`: same one-line stub.

- [ ] **Step 6: Verify scaffolding builds + lints**

Run:

```
nix develop --command cargo fmt --all
nix develop --command cargo clippy -p sunset-cli --all-targets -- -D warnings
nix develop --command cargo build -p sunset-cli
```

Expected: all three succeed.

- [ ] **Step 7: Commit**

```
git add Cargo.toml crates/sunset-cli docs/superpowers/specs/2026-05-06-sunset-cli-design.md docs/superpowers/plans/2026-05-06-sunset-cli.md
git commit -m "$(cat <<'EOF'
sunset-cli: scaffold crate (spec + plan + empty modules)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 2: Identity load-or-generate

**Files:**
- Modify: `crates/sunset-cli/src/identity.rs`

`sunset-relay::identity::load_or_generate` reads a 32-byte secret from a file or generates and persists one. We replicate that contract for the CLI, with `dirs::config_dir()` as the default location.

- [ ] **Step 1: Write the failing test**

```rust
// crates/sunset-cli/src/identity.rs

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn generates_then_persists_then_reloads() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("identity.bin");

        let id1 = load_or_generate(&path).await.expect("first call generates");
        let id2 = load_or_generate(&path).await.expect("second call reads");

        // Same secret bytes both times.
        assert_eq!(id1.public().as_bytes(), id2.public().as_bytes());
        // File exists with exactly 32 bytes.
        let raw = tokio::fs::read(&path).await.unwrap();
        assert_eq!(raw.len(), 32);
    }

    #[tokio::test]
    async fn refuses_wrong_size_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("identity.bin");
        tokio::fs::write(&path, b"too-short").await.unwrap();
        let err = load_or_generate(&path).await.unwrap_err();
        let s = format!("{err}");
        assert!(s.contains("32"), "{s}");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

```
nix develop --command cargo test -p sunset-cli identity::tests
```

Expected: FAIL — `load_or_generate` not defined.

- [ ] **Step 3: Implement**

```rust
//! Load-or-generate the user's 32-byte ed25519 secret seed.

use std::path::Path;

use rand_core::{OsRng, RngCore};
use sunset_core::Identity;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("identity file at {path:?} is {got} bytes, expected 32")]
    WrongSize { path: std::path::PathBuf, got: usize },
}

pub type Result<T> = std::result::Result<T, Error>;

/// Read a 32-byte seed from `path`, or generate one and persist it
/// (mode 0600 on Unix). Returns the resulting `Identity`.
pub async fn load_or_generate(path: &Path) -> Result<Identity> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    match tokio::fs::read(path).await {
        Ok(bytes) => {
            if bytes.len() != 32 {
                return Err(Error::WrongSize {
                    path: path.to_path_buf(),
                    got: bytes.len(),
                });
            }
            let mut seed = [0u8; 32];
            seed.copy_from_slice(&bytes);
            Ok(Identity::from_secret_bytes(&seed))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let mut seed = [0u8; 32];
            OsRng.fill_bytes(&mut seed);
            write_secret(path, &seed).await?;
            Ok(Identity::from_secret_bytes(&seed))
        }
        Err(e) => Err(Error::Io(e)),
    }
}

#[cfg(unix)]
async fn write_secret(path: &Path, seed: &[u8; 32]) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    tokio::fs::write(path, seed).await?;
    let perms = std::fs::Permissions::from_mode(0o600);
    tokio::fs::set_permissions(path, perms).await?;
    Ok(())
}

#[cfg(not(unix))]
async fn write_secret(path: &Path, seed: &[u8; 32]) -> Result<()> {
    tokio::fs::write(path, seed).await?;
    Ok(())
}

/// Default identity path: `$SUNSET_IDENTITY_PATH` if set, else
/// `<config_dir>/sunset/identity.bin`. `config_dir()` returns
/// `~/.config` on Linux, `~/Library/Application Support` on macOS.
pub fn default_path() -> std::path::PathBuf {
    if let Ok(v) = std::env::var("SUNSET_IDENTITY_PATH") {
        return std::path::PathBuf::from(v);
    }
    let base = dirs::config_dir().unwrap_or_else(|| std::path::PathBuf::from("."));
    base.join("sunset").join("identity.bin")
}
```

- [ ] **Step 4: Run tests**

```
nix develop --command cargo test -p sunset-cli identity::tests
```

Expected: PASS.

- [ ] **Step 5: Commit**

```
git add crates/sunset-cli/src/identity.rs
git commit -m "$(cat <<'EOF'
sunset-cli: identity load-or-generate

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 3: ReqwestFetch HttpFetch adapter

**Files:**
- Modify: `crates/sunset-cli/src/resolver_adapter.rs`

Mirror `sunset-relay/src/resolver_adapter.rs`. The `pub(crate)` shim there isn't reachable from sunset-cli, so we duplicate it. (Promoting it to `sunset-relay-resolver` is a possible follow-up; not load-bearing here.)

- [ ] **Step 1: Implement**

```rust
//! `reqwest`-backed [`HttpFetch`] for the CLI's hostname-based
//! relay descriptor lookups.

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

- [ ] **Step 2: Verify it compiles**

```
nix develop --command cargo build -p sunset-cli
```

Expected: success. There's no test for this — the behavior is identical to the relay's; it's literally the same code, and the resolver crate has its own coverage via `FakeFetch`.

- [ ] **Step 3: Commit**

```
git add crates/sunset-cli/src/resolver_adapter.rs
git commit -m "$(cat <<'EOF'
sunset-cli: ReqwestFetch HttpFetch impl for hostname relay input

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 4: Type aliases + build_peer factory

**Files:**
- Modify: `crates/sunset-cli/src/build.rs`

Pure wiring. No tests at this level — Task 5+ exercises it end-to-end.

- [ ] **Step 1: Implement**

```rust
//! Type aliases + `build_peer` factory: wires sunset-core::Peer with
//! a MemoryStore + FallbackTransport<NoiseTransport<WtNative>, NoiseTransport<WsNative>>.
//!
//! The transport pattern mirrors `sunset-web-wasm/src/client.rs` minus
//! WebRTC. FallbackTransport routes by URL scheme: `wt://`/`wts://`
//! prefers WT then falls back to WS; `ws://`/`wss://` short-circuits
//! straight to WS.

use std::rc::Rc;
use std::sync::Arc;

use sunset_core::{Ed25519Verifier, Identity, Peer};
use sunset_noise::{NoiseIdentity, NoiseTransport};
use sunset_store::VerifyingKey;
use sunset_store_memory::MemoryStore;
use sunset_sync::{
    BackoffPolicy, FallbackTransport, PeerId, PeerSupervisor, Signer, SyncConfig, SyncEngine,
};
use sunset_sync_webtransport_native::WebTransportRawTransport;
use sunset_sync_ws_native::WebSocketRawTransport;
use zeroize::Zeroizing;

pub type CliTransport = FallbackTransport<
    NoiseTransport<WebTransportRawTransport>,
    NoiseTransport<WebSocketRawTransport>,
>;

pub type CliPeer = Peer<MemoryStore, CliTransport>;
pub type CliEngine = SyncEngine<MemoryStore, CliTransport>;
pub type CliSupervisor = PeerSupervisor<MemoryStore, CliTransport>;

/// Adapter: sunset-core's `Identity` → `NoiseIdentity`.
struct IdentityNoiseAdapter(Identity);

impl NoiseIdentity for IdentityNoiseAdapter {
    fn ed25519_public(&self) -> [u8; 32] {
        self.0.public().as_bytes()
    }
    fn ed25519_secret_seed(&self) -> Zeroizing<[u8; 32]> {
        Zeroizing::new(self.0.secret_bytes())
    }
}

/// Output of `build_peer`. Engines + supervisors must be `run` by the
/// caller (which spawns them on a `LocalSet`); the factory itself
/// does no I/O and starts no tasks.
pub struct BuiltPeer {
    pub peer: Rc<CliPeer>,
    pub engine: Rc<CliEngine>,
    pub supervisor: Rc<CliSupervisor>,
    pub store: Arc<MemoryStore>,
}

pub fn build_peer(identity: Identity) -> BuiltPeer {
    let store = Arc::new(MemoryStore::new(Arc::new(Ed25519Verifier)));

    let ws_raw = WebSocketRawTransport::dial_only();
    let ws_noise =
        NoiseTransport::new(ws_raw, Arc::new(IdentityNoiseAdapter(identity.clone())));
    let wt_raw = WebTransportRawTransport::dial_only();
    let wt_noise =
        NoiseTransport::new(wt_raw, Arc::new(IdentityNoiseAdapter(identity.clone())));
    let transport = FallbackTransport::new(wt_noise, ws_noise);

    let local_peer = PeerId(VerifyingKey::new(bytes::Bytes::copy_from_slice(
        &identity.public().as_bytes(),
    )));
    let signer: Arc<dyn Signer> = Arc::new(identity.clone());

    let engine = Rc::new(SyncEngine::new(
        store.clone(),
        transport,
        SyncConfig::default(),
        local_peer,
        signer,
    ));
    let supervisor = PeerSupervisor::new(engine.clone(), BackoffPolicy::default());
    let dispatcher = sunset_core::MultiRoomSignaler::new();
    let peer = Peer::new(
        identity,
        store.clone(),
        engine.clone(),
        supervisor.clone(),
        dispatcher,
    );

    BuiltPeer {
        peer,
        engine,
        supervisor,
        store,
    }
}
```

- [ ] **Step 2: Verify it compiles**

```
nix develop --command cargo build -p sunset-cli
nix develop --command cargo clippy -p sunset-cli --all-targets -- -D warnings
```

Expected: success.

- [ ] **Step 3: Commit**

```
git add crates/sunset-cli/src/build.rs
git commit -m "$(cat <<'EOF'
sunset-cli: build_peer factory + type aliases

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 5: Snapshot view types

**Files:**
- Modify: `crates/sunset-cli/src/view.rs`

Plain data shapes that `Client` writes and the UI reads.

- [ ] **Step 1: Implement**

```rust
//! Snapshot types passed from `client::Client` to `ui::App`.
//!
//! Plain data — no callbacks, no async. The Client mutates these
//! through `Rc<RefCell<...>>` cells; the UI reads them on each
//! redraw. UI re-render is signaled out-of-band via a
//! `tokio::sync::Notify`, not via these types.

#[derive(Debug, Clone)]
pub struct MessageLine {
    pub author_pubkey: [u8; 32],
    pub author_name: Option<String>,
    pub body: String,
    pub sent_at_ms: u64,
    pub is_self: bool,
}

#[derive(Debug, Clone)]
pub struct MemberRow {
    pub pubkey: [u8; 32],
    pub name: Option<String>,
    /// "direct" | "via_relay" | "unknown" — matches
    /// `OpenRoom::peer_connection_mode`.
    pub connection_mode: &'static str,
    /// "online" | "stale" | "offline" — matches
    /// `sunset_core::membership::Presence`.
    pub presence: &'static str,
}

#[derive(Debug, Clone, Default)]
pub struct RoomView {
    pub name: String,
    pub messages: Vec<MessageLine>,
    pub members: Vec<MemberRow>,
}

#[derive(Debug, Clone)]
pub struct RelayRow {
    pub label: String,
    /// "connecting" | "connected" | "backoff" | "error" | "stopped"
    pub state: &'static str,
    pub last_rtt_ms: Option<u64>,
}

#[derive(Debug, Clone, Default)]
pub struct TopState {
    pub identity_hex: String,
    pub self_name: Option<String>,
    pub active_room: Option<String>,
    pub open_rooms: Vec<String>,
    pub relays: Vec<RelayRow>,
    /// Append-only log printed in the message pane in addition to
    /// chat messages. Used for `/help`, errors, and command output.
    pub system_log: Vec<String>,
}
```

- [ ] **Step 2: Verify it compiles**

```
nix develop --command cargo build -p sunset-cli
```

Expected: success.

- [ ] **Step 3: Commit**

```
git add crates/sunset-cli/src/view.rs
git commit -m "$(cat <<'EOF'
sunset-cli: view snapshot types

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 6: core::Client (state + thin async API)

**Files:**
- Modify: `crates/sunset-cli/src/client.rs`
- Create: `crates/sunset-cli/tests/helpers/mod.rs`

The host-agnostic surface the UI and the integration tests both drive.

- [ ] **Step 1: Implement `client.rs`**

```rust
//! `Client`: high-level async API around a `sunset-core::Peer`.
//!
//! The UI and the integration tests both drive this type. It owns:
//!  - the `Peer` + `Engine` + `Supervisor` (constructed via
//!    `build::build_peer` and run on a `LocalSet`),
//!  - per-room snapshot state (`RoomView`),
//!  - top-level state (`TopState`),
//!  - a `Notify` used to wake the UI loop on any state change.
//!
//! Method bodies route to `Peer::open_room`, `Peer::add_relay`,
//! `OpenRoom::send_text`, etc., and copy the relevant data into the
//! snapshot cells. They never block on the UI.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use sunset_core::Identity;
use sunset_sync::Connectable;
use tokio::sync::Notify;

use crate::build::{BuiltPeer, CliPeer, CliEngine, CliSupervisor, build_peer};
use crate::resolver_adapter::ReqwestFetch;
use crate::view::{MemberRow, MessageLine, RelayRow, RoomView, TopState};

pub struct Client {
    pub identity: Identity,
    pub peer: Rc<CliPeer>,
    pub engine: Rc<CliEngine>,
    pub supervisor: Rc<CliSupervisor>,

    pub top: Rc<RefCell<TopState>>,
    pub rooms: Rc<RefCell<HashMap<String, Rc<RefCell<RoomView>>>>>,
    pub notify: Rc<Notify>,
}

impl Client {
    /// Constructs the Client, spawns engine + supervisor on the
    /// current `LocalSet`. Caller must already be inside a
    /// `LocalSet::run_until(...)`.
    pub fn start(identity: Identity) -> Rc<Self> {
        let BuiltPeer {
            peer,
            engine,
            supervisor,
            store: _store,
        } = build_peer(identity.clone());

        // Spawn engine and supervisor.
        let engine_clone = engine.clone();
        tokio::task::spawn_local(async move {
            if let Err(e) = engine_clone.run().await {
                tracing::error!(error = %e, "engine exited");
            }
        });
        let sup_clone = supervisor.clone();
        tokio::task::spawn_local(async move { sup_clone.run().await });

        let identity_hex = hex::encode(identity.public().as_bytes());
        let top = Rc::new(RefCell::new(TopState {
            identity_hex,
            ..TopState::default()
        }));
        let rooms = Rc::new(RefCell::new(HashMap::new()));
        let notify = Rc::new(Notify::new());

        let me = Rc::new(Self {
            identity,
            peer,
            engine,
            supervisor,
            top,
            rooms,
            notify,
        });

        me.spawn_intent_subscriber();
        me
    }

    fn poke(&self) {
        self.notify.notify_one();
    }

    fn spawn_intent_subscriber(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        let peer = self.peer.clone();
        tokio::task::spawn_local(async move {
            let mut rx = peer.subscribe_intents().await;
            while let Some(snap) = rx.recv().await {
                let Some(strong) = weak.upgrade() else { return };
                strong.apply_intent_snapshot(snap);
            }
        });
    }

    fn apply_intent_snapshot(&self, snap: sunset_sync::IntentSnapshot) {
        use sunset_sync::IntentState;
        let state = match snap.state {
            IntentState::Connecting => "connecting",
            IntentState::Connected => "connected",
            IntentState::Backoff { .. } => "backoff",
            IntentState::Error { .. } => "error",
            IntentState::Stopped => "stopped",
        };
        let row = RelayRow {
            label: snap.label,
            state,
            last_rtt_ms: snap.last_rtt_ms,
        };
        let mut top = self.top.borrow_mut();
        // Replace or append by id.
        if let Some(existing) = top.relays.iter_mut().find(|r| r.label == row.label) {
            *existing = row;
        } else {
            top.relays.push(row);
        }
        drop(top);
        self.poke();
    }

    pub async fn add_relay(&self, url: String) -> Result<(), String> {
        let fetch: Rc<dyn sunset_relay_resolver::HttpFetch> =
            Rc::new(ReqwestFetch::default());
        let connectable = Connectable::Resolving { input: url, fetch };
        self.peer
            .add_relay(connectable)
            .await
            .map_err(|e| format!("{e}"))
            .map(|_| ())
    }

    pub fn set_self_name(&self, name: &str) {
        self.peer.set_self_name(name);
        let mut top = self.top.borrow_mut();
        top.self_name = if name.is_empty() {
            None
        } else {
            Some(name.to_owned())
        };
        drop(top);
        self.poke();
    }

    pub async fn join_room(self: &Rc<Self>, name: &str) -> Result<(), String> {
        if self.rooms.borrow().contains_key(name) {
            // Idempotent — just switch.
            self.set_active(name);
            return Ok(());
        }
        let open_room = self.peer.open_room(name).await.map_err(|e| format!("{e}"))?;

        let view = Rc::new(RefCell::new(RoomView {
            name: name.to_owned(),
            messages: Vec::new(),
            members: Vec::new(),
        }));

        // Wire on_message → push into messages.
        {
            let view = view.clone();
            let notify = self.notify.clone();
            open_room.on_message(move |decoded, is_self| {
                if let sunset_core::MessageBody::Text(t) = &decoded.body {
                    let line = MessageLine {
                        author_pubkey: decoded.author_key.as_bytes(),
                        author_name: None,
                        body: t.clone(),
                        sent_at_ms: decoded.sent_at_ms,
                        is_self,
                    };
                    view.borrow_mut().messages.push(line);
                    notify.notify_one();
                }
            });
        }
        // Wire on_members_changed → snapshot into members.
        {
            let view = view.clone();
            let notify = self.notify.clone();
            let inner_room = open_room.clone();
            open_room.on_members_changed(move |members| {
                let rows: Vec<MemberRow> = members
                    .iter()
                    .map(|m| {
                        let mode = inner_room.peer_connection_mode(m.pubkey);
                        let presence = match m.presence {
                            sunset_core::membership::Presence::Online => "online",
                            sunset_core::membership::Presence::Stale => "stale",
                            sunset_core::membership::Presence::Offline => "offline",
                        };
                        MemberRow {
                            pubkey: m.pubkey,
                            name: m.name.clone(),
                            connection_mode: mode,
                            presence,
                        }
                    })
                    .collect();
                view.borrow_mut().members = rows;
                notify.notify_one();
            });
        }

        // Start presence so other peers see us.
        open_room.start_presence(2_000, 6_000, 1_000).await;

        // Stash both the OpenRoom (so its background tasks stay alive)
        // and the view. We re-derive the OpenRoom from the Peer's
        // registry when the user issues commands; the room itself is
        // an Rc<RoomState> internally, so dropping `open_room` here is
        // fine — `Peer::open_room` returns a fresh handle.
        let _ = open_room; // The Peer's open_rooms map keeps it alive.

        self.rooms.borrow_mut().insert(name.to_owned(), view);
        self.set_active(name);

        Ok(())
    }

    pub fn set_active(&self, name: &str) {
        let mut top = self.top.borrow_mut();
        top.active_room = Some(name.to_owned());
        if !top.open_rooms.contains(&name.to_owned()) {
            top.open_rooms.push(name.to_owned());
        }
        drop(top);
        self.poke();
    }

    /// Send a chat text into the active room. No-op if no active room.
    pub async fn send_text(&self, body: String) -> Result<(), String> {
        let active = self.top.borrow().active_room.clone();
        let Some(name) = active else { return Ok(()) };
        let open_room = self
            .peer
            .open_room(&name)
            .await
            .map_err(|e| format!("{e}"))?;
        let now_ms = chrono::Utc::now().timestamp_millis() as u64;
        open_room
            .send_text(body, now_ms)
            .await
            .map_err(|e| format!("{e}"))?;
        Ok(())
    }

    pub fn snapshot_top(&self) -> TopState {
        self.top.borrow().clone()
    }

    pub fn snapshot_room(&self, name: &str) -> Option<RoomView> {
        self.rooms.borrow().get(name).map(|v| v.borrow().clone())
    }

    pub fn append_system(&self, line: String) {
        self.top.borrow_mut().system_log.push(line);
        self.poke();
    }
}
```

- [ ] **Step 2: Write `tests/helpers/mod.rs`**

```rust
//! Shared test helpers: spin up an in-process relay; build a CLI
//! Client connected to it.
//!
//! All tests run under a single-threaded `LocalSet` (the engine is
//! `?Send`).

use std::path::PathBuf;
use std::time::Duration;

use sunset_cli::client::Client;
use sunset_relay::{Config as RelayConfig, Relay};
use tempfile::TempDir;

pub struct TestRelay {
    pub dial_url: String,
    pub _data_dir: TempDir,
    pub _engine_task: tokio::task::JoinHandle<sunset_sync::Result<()>>,
}

pub async fn spawn_relay() -> TestRelay {
    let data_dir = tempfile::tempdir().expect("tempdir");
    let identity_secret_path = data_dir.path().join("identity.bin");
    let cfg = RelayConfig {
        listen_addr: "127.0.0.1:0".parse().unwrap(),
        data_dir: data_dir.path().to_path_buf(),
        identity_secret_path,
        peers: Vec::new(),
        accept_handshake_timeout_secs: 30,
        interest_filter: sunset_relay::config::InterestFilter::All,
    };
    let mut handle = Relay::start(cfg).await.expect("relay start");
    let dial_url = handle.dial_address();
    let engine_task = handle.run_for_test().await.expect("relay run_for_test");
    TestRelay {
        dial_url,
        _data_dir: data_dir,
        _engine_task: engine_task,
    }
}

/// Build a Client from a fresh random identity.
pub fn fresh_client() -> std::rc::Rc<Client> {
    let mut seed = [0u8; 32];
    use rand_core::RngCore;
    rand_core::OsRng.fill_bytes(&mut seed);
    let identity = sunset_core::Identity::from_secret_bytes(&seed);
    Client::start(identity)
}

/// Wait for a closure to return Some / true within `deadline`. Polls
/// every 25ms. Used for "eventually" assertions in integration tests.
/// Timeouts encode UX bars per CLAUDE.md — do NOT raise the deadline
/// to mask a slow path.
pub async fn eventually<F, T>(deadline: Duration, mut f: F) -> Option<T>
where
    F: FnMut() -> Option<T>,
{
    let start = std::time::Instant::now();
    loop {
        if let Some(v) = f() {
            return Some(v);
        }
        if start.elapsed() >= deadline {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}
```

(The `PathBuf` import is unused at the top — remove it before saving. Lint is `-D warnings`.)

- [ ] **Step 3: Verify build + lint**

```
nix develop --command cargo build -p sunset-cli --all-targets
nix develop --command cargo clippy -p sunset-cli --all-targets -- -D warnings
```

Expected: success.

- [ ] **Step 4: Commit**

```
git add crates/sunset-cli/src/client.rs crates/sunset-cli/tests/helpers/
git commit -m "$(cat <<'EOF'
sunset-cli: Client wraps Peer with snapshot view + intent watcher

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 7: Integration test — WS message roundtrip

**Files:**
- Create: `crates/sunset-cli/tests/roundtrip_ws.rs`

- [ ] **Step 1: Write the test**

```rust
//! 2 CLI clients connect to one in-process relay over WS, exchange
//! a chat message both directions, and observe each other in the
//! members panel.

mod helpers;

use std::time::Duration;

use helpers::{eventually, fresh_client, spawn_relay};

#[tokio::test(flavor = "current_thread")]
async fn ws_roundtrip_two_clients() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let relay = spawn_relay().await;
            // Relay::dial_address returns a `ws://...#x25519=<hex>` URL —
            // canonical, no resolver lookup needed.
            let url = relay.dial_url.clone();

            let alice = fresh_client();
            let bob = fresh_client();
            alice.set_self_name("alice");
            bob.set_self_name("bob");

            alice
                .add_relay(url.clone())
                .await
                .expect("alice add_relay");
            bob.add_relay(url.clone()).await.expect("bob add_relay");

            alice.join_room("alpha").await.expect("alice join");
            bob.join_room("alpha").await.expect("bob join");

            // alice sends; bob should see it.
            alice
                .send_text("hello bob".to_owned())
                .await
                .expect("alice send");

            let saw = eventually(Duration::from_secs(10), || {
                let v = bob.snapshot_room("alpha")?;
                if v.messages.iter().any(|m| m.body == "hello bob") {
                    Some(())
                } else {
                    None
                }
            })
            .await;
            assert!(saw.is_some(), "bob never saw alice's message");

            // and the reverse.
            bob.send_text("hi alice".to_owned()).await.expect("bob send");
            let saw = eventually(Duration::from_secs(10), || {
                let v = alice.snapshot_room("alpha")?;
                if v.messages.iter().any(|m| m.body == "hi alice") {
                    Some(())
                } else {
                    None
                }
            })
            .await;
            assert!(saw.is_some(), "alice never saw bob's message");
        })
        .await;
}
```

- [ ] **Step 2: Run it**

```
nix develop --command cargo test -p sunset-cli --test roundtrip_ws -- --nocapture
```

Expected: PASS within ~3 seconds. If it fails or times out, do NOT raise the 10-second timeout — debug the underlying connect path. See CLAUDE.md "Debugging discipline".

- [ ] **Step 3: Commit**

```
git add crates/sunset-cli/tests/roundtrip_ws.rs
git commit -m "$(cat <<'EOF'
sunset-cli: integration test — WS message roundtrip between two clients

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 8: Integration test — peer visibility + WT roundtrip

**Files:**
- Create: `crates/sunset-cli/tests/roundtrip_wt.rs`
- Create: `crates/sunset-cli/tests/peer_visibility.rs`

- [ ] **Step 1: Write `peer_visibility.rs`**

```rust
//! After both clients join the same room over WS, each should see
//! the other in `members` with `connection_mode == "via_relay"`.
//! No native WebRTC means peers cannot upgrade to "direct".

mod helpers;

use std::time::Duration;

use helpers::{eventually, fresh_client, spawn_relay};

#[tokio::test(flavor = "current_thread")]
async fn members_show_via_relay() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let relay = spawn_relay().await;
            let url = relay.dial_url.clone();

            let alice = fresh_client();
            let bob = fresh_client();
            alice.set_self_name("alice");
            bob.set_self_name("bob");

            alice.add_relay(url.clone()).await.unwrap();
            bob.add_relay(url.clone()).await.unwrap();

            alice.join_room("alpha").await.unwrap();
            bob.join_room("alpha").await.unwrap();

            let bob_pk = bob.identity.public().as_bytes();

            let saw = eventually(Duration::from_secs(10), || {
                let v = alice.snapshot_room("alpha")?;
                let row = v.members.iter().find(|m| m.pubkey == bob_pk)?;
                if row.connection_mode == "via_relay" {
                    Some(())
                } else {
                    None
                }
            })
            .await;
            assert!(saw.is_some(), "alice never saw bob with via_relay mode");
        })
        .await;
}
```

- [ ] **Step 2: Write `roundtrip_wt.rs`**

```rust
//! Same shape as roundtrip_ws but the clients dial via wt://. The
//! relay has both a TCP/WS listener and a UDP/WT listener; the wt
//! URL routes through FallbackTransport's primary half. If the
//! relay's wt cert init fails (no UDP, container restrictions,
//! etc.), the relay logs a warning and the test marks itself
//! `ignore` rather than failing — matching the relay's "WS-only
//! fallback" behavior.

mod helpers;

use std::time::Duration;

use helpers::{eventually, fresh_client, spawn_relay};

#[tokio::test(flavor = "current_thread")]
async fn wt_roundtrip_two_clients() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let relay = spawn_relay().await;
            // Construct a wt:// URL from the relay's bound address.
            // Relay::dial_address gives ws://...; we need to fetch the
            // identity descriptor to learn the cert hash.
            let descriptor_url = format!(
                "http://{}/",
                relay
                    .dial_url
                    .strip_prefix("ws://")
                    .unwrap()
                    .split('#')
                    .next()
                    .unwrap()
            );
            let body = match reqwest::get(&descriptor_url).await {
                Ok(r) => r.text().await.unwrap_or_default(),
                Err(_) => return, // relay descriptor unreachable in this env
            };
            let cert_hex = match extract_field(&body, "webtransport_cert_sha256") {
                Some(s) => s,
                None => return, // wt disabled in this env
            };
            let x25519_hex = relay
                .dial_url
                .split("x25519=")
                .nth(1)
                .expect("x25519 fragment")
                .to_string();
            let host_port = relay
                .dial_url
                .strip_prefix("ws://")
                .unwrap()
                .split('#')
                .next()
                .unwrap();
            let wt_url = format!(
                "wt://{host_port}#x25519={x25519_hex}&cert-sha256={cert_hex}"
            );

            let alice = fresh_client();
            let bob = fresh_client();
            alice.set_self_name("alice");
            bob.set_self_name("bob");

            alice.add_relay(wt_url.clone()).await.unwrap();
            bob.add_relay(wt_url.clone()).await.unwrap();

            alice.join_room("alpha").await.unwrap();
            bob.join_room("alpha").await.unwrap();

            alice.send_text("hello over wt".to_owned()).await.unwrap();
            let saw = eventually(Duration::from_secs(10), || {
                let v = bob.snapshot_room("alpha")?;
                if v.messages.iter().any(|m| m.body == "hello over wt") {
                    Some(())
                } else {
                    None
                }
            })
            .await;
            assert!(saw.is_some(), "wt path: bob never saw alice's message");
        })
        .await;
}

/// Cheap JSON field extractor (descriptor body shape is stable).
/// Avoids dragging serde_json into dev-deps just for one match.
fn extract_field(body: &str, key: &str) -> Option<String> {
    let pat = format!("\"{key}\":\"");
    let i = body.find(&pat)?;
    let rest = &body[i + pat.len()..];
    let end = rest.find('"')?;
    Some(rest[..end].to_owned())
}
```

- [ ] **Step 3: Run them**

```
nix develop --command cargo test -p sunset-cli --test peer_visibility
nix develop --command cargo test -p sunset-cli --test roundtrip_wt
```

Expected: both PASS. The WT test self-skips if the relay can't bind UDP in this environment (returns early).

- [ ] **Step 4: Commit**

```
git add crates/sunset-cli/tests/roundtrip_wt.rs crates/sunset-cli/tests/peer_visibility.rs
git commit -m "$(cat <<'EOF'
sunset-cli: integration tests — peer visibility + WT roundtrip

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 9: /command parser

**Files:**
- Modify: `crates/sunset-cli/src/command.rs`

- [ ] **Step 1: Write the failing tests**

```rust
//! /command parser. Pure — no async, no I/O.

#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    Help,
    Join(String),
    Switch(String),
    Leave(Option<String>),
    Rooms,
    Peers,
    Relays,
    RelayAdd(String),
    Name(String),
    Me,
    Voice,
    Quit,
    Send(String),
    Unknown(String),
    Empty,
}

pub fn parse(line: &str) -> Command {
    let trimmed = line.trim_start();
    if trimmed.is_empty() {
        return Command::Empty;
    }
    if !trimmed.starts_with('/') {
        return Command::Send(line.trim_end().to_owned());
    }
    let mut parts = trimmed[1..].splitn(2, char::is_whitespace);
    let head = parts.next().unwrap_or("");
    let tail = parts.next().unwrap_or("").trim();
    match head {
        "help" | "?" => Command::Help,
        "join" => Command::Join(tail.to_owned()),
        "switch" => Command::Switch(tail.to_owned()),
        "leave" => {
            if tail.is_empty() {
                Command::Leave(None)
            } else {
                Command::Leave(Some(tail.to_owned()))
            }
        }
        "rooms" => Command::Rooms,
        "peers" => Command::Peers,
        "relays" => Command::Relays,
        "relay" => {
            let mut sub = tail.splitn(2, char::is_whitespace);
            let verb = sub.next().unwrap_or("");
            let arg = sub.next().unwrap_or("").trim();
            match verb {
                "add" if !arg.is_empty() => Command::RelayAdd(arg.to_owned()),
                _ => Command::Unknown(format!("/relay {tail}")),
            }
        }
        "name" => Command::Name(tail.to_owned()),
        "me" => Command::Me,
        "voice" => Command::Voice,
        "quit" | "exit" => Command::Quit,
        other => Command::Unknown(format!("/{other}{}{tail}", if tail.is_empty() { "" } else { " " })),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_string_is_empty() {
        assert_eq!(parse(""), Command::Empty);
        assert_eq!(parse("   "), Command::Empty);
    }

    #[test]
    fn bare_text_is_send() {
        assert_eq!(parse("hello world"), Command::Send("hello world".to_owned()));
    }

    #[test]
    fn slash_help_is_help() {
        assert_eq!(parse("/help"), Command::Help);
        assert_eq!(parse("/?"), Command::Help);
    }

    #[test]
    fn slash_join_takes_room_name() {
        assert_eq!(parse("/join alpha"), Command::Join("alpha".to_owned()));
    }

    #[test]
    fn slash_relay_add_takes_url() {
        assert_eq!(
            parse("/relay add wss://r.example#x25519=ab"),
            Command::RelayAdd("wss://r.example#x25519=ab".to_owned())
        );
    }

    #[test]
    fn slash_relay_without_subcommand_is_unknown() {
        assert!(matches!(parse("/relay"), Command::Unknown(_)));
        assert!(matches!(parse("/relay add"), Command::Unknown(_)));
    }

    #[test]
    fn slash_leave_optional_arg() {
        assert_eq!(parse("/leave"), Command::Leave(None));
        assert_eq!(parse("/leave alpha"), Command::Leave(Some("alpha".to_owned())));
    }

    #[test]
    fn unknown_slash_command() {
        assert!(matches!(parse("/foo bar"), Command::Unknown(_)));
    }
}
```

- [ ] **Step 2: Run tests**

```
nix develop --command cargo test -p sunset-cli command::tests
```

Expected: PASS.

- [ ] **Step 3: Commit**

```
git add crates/sunset-cli/src/command.rs
git commit -m "$(cat <<'EOF'
sunset-cli: /command parser

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 10: Command dispatcher → Client side effects

**Files:**
- Modify: `crates/sunset-cli/src/dispatch.rs`
- Create: `crates/sunset-cli/tests/command_dispatch.rs`

- [ ] **Step 1: Implement dispatch**

```rust
//! Dispatches a parsed `Command` to side effects on a `Client`.
//!
//! Returns `DispatchOutcome::Quit` when the user typed `/quit`.

use std::rc::Rc;

use crate::client::Client;
use crate::command::Command;

pub enum DispatchOutcome {
    Continue,
    Quit,
}

pub async fn dispatch(client: &Rc<Client>, cmd: Command) -> DispatchOutcome {
    match cmd {
        Command::Empty => {}
        Command::Help => {
            for line in HELP_LINES {
                client.append_system((*line).to_owned());
            }
        }
        Command::Join(name) => {
            if name.is_empty() {
                client.append_system("/join: missing room name".to_owned());
            } else if let Err(e) = client.join_room(&name).await {
                client.append_system(format!("/join failed: {e}"));
            }
        }
        Command::Switch(name) => {
            if client.snapshot_room(&name).is_some() {
                client.set_active(&name);
            } else {
                client.append_system(format!("/switch: not in room '{name}' — use /join"));
            }
        }
        Command::Leave(name) => {
            // Match the active room when name is None.
            let target = name.or_else(|| client.snapshot_top().active_room);
            if let Some(t) = target {
                client.leave_room(&t);
            }
        }
        Command::Rooms => {
            let top = client.snapshot_top();
            client.append_system(format!("rooms: {:?}", top.open_rooms));
        }
        Command::Peers => {
            let top = client.snapshot_top();
            if let Some(active) = &top.active_room {
                if let Some(view) = client.snapshot_room(active) {
                    for m in &view.members {
                        let name = m.name.as_deref().unwrap_or("(no name)");
                        client.append_system(format!(
                            "peer {} {} ({}) {}",
                            &hex::encode(m.pubkey)[..8],
                            name,
                            m.connection_mode,
                            m.presence,
                        ));
                    }
                }
            } else {
                client.append_system("/peers: no active room".to_owned());
            }
        }
        Command::Relays => {
            let top = client.snapshot_top();
            for r in &top.relays {
                client.append_system(format!(
                    "relay {} [{}] rtt={:?}",
                    r.label, r.state, r.last_rtt_ms
                ));
            }
        }
        Command::RelayAdd(url) => {
            if let Err(e) = client.add_relay(url).await {
                client.append_system(format!("/relay add failed: {e}"));
            }
        }
        Command::Name(name) => client.set_self_name(&name),
        Command::Me => {
            let top = client.snapshot_top();
            let label = top.self_name.as_deref().unwrap_or("(no name)");
            client.append_system(format!("identity {} ({})", top.identity_hex, label));
        }
        Command::Voice => {
            client.append_system(
                "/voice: not yet implemented in the CLI; use the web client at https://sunset.chat"
                    .to_owned(),
            );
        }
        Command::Quit => return DispatchOutcome::Quit,
        Command::Send(body) => {
            if let Err(e) = client.send_text(body).await {
                client.append_system(format!("send failed: {e}"));
            }
        }
        Command::Unknown(s) => {
            client.append_system(format!("unknown command: {s} — try /help"));
        }
    }
    DispatchOutcome::Continue
}

const HELP_LINES: &[&str] = &[
    "commands:",
    "  /help                — this list",
    "  /join <room>         — open and switch to a room",
    "  /switch <room>       — switch active room",
    "  /leave [room]        — leave a room (default: active)",
    "  /rooms               — list open rooms",
    "  /peers               — list peers in the active room",
    "  /relays              — list relay intents + state",
    "  /relay add <url>     — add a relay (ws://, wss://, wt://, wts://, or hostname)",
    "  /name <name>         — set your display name",
    "  /me                  — show your identity",
    "  /voice               — voice (deferred — use the web client)",
    "  /quit                — exit",
];
```

- [ ] **Step 2: Add `Client::leave_room`**

In `client.rs`, add:

```rust
impl Client {
    pub fn leave_room(&self, name: &str) {
        self.rooms.borrow_mut().remove(name);
        let mut top = self.top.borrow_mut();
        top.open_rooms.retain(|r| r != name);
        if top.active_room.as_deref() == Some(name) {
            top.active_room = top.open_rooms.last().cloned();
        }
        drop(top);
        self.poke();
    }
}
```

- [ ] **Step 3: Write `tests/command_dispatch.rs`**

```rust
//! Command dispatcher correctness — no UI, no relay involved.

mod helpers;

use sunset_cli::command::{parse, Command};
use sunset_cli::dispatch::{dispatch, DispatchOutcome};

#[tokio::test(flavor = "current_thread")]
async fn join_then_switch_then_leave() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let client = helpers::fresh_client();
            assert!(matches!(
                dispatch(&client, parse("/join alpha")).await,
                DispatchOutcome::Continue
            ));
            assert_eq!(client.snapshot_top().active_room.as_deref(), Some("alpha"));

            dispatch(&client, parse("/join beta")).await;
            assert_eq!(client.snapshot_top().active_room.as_deref(), Some("beta"));

            dispatch(&client, parse("/switch alpha")).await;
            assert_eq!(client.snapshot_top().active_room.as_deref(), Some("alpha"));

            dispatch(&client, parse("/leave")).await;
            // alpha was active; should fall back to beta.
            assert_eq!(client.snapshot_top().active_room.as_deref(), Some("beta"));
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn quit_returns_quit() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let client = helpers::fresh_client();
            assert!(matches!(
                dispatch(&client, Command::Quit).await,
                DispatchOutcome::Quit
            ));
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn voice_is_a_stub_message() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let client = helpers::fresh_client();
            dispatch(&client, parse("/voice")).await;
            let log = client.snapshot_top().system_log;
            assert!(
                log.iter().any(|l| l.contains("not yet implemented")),
                "system log: {log:?}"
            );
        })
        .await;
}
```

- [ ] **Step 4: Run tests**

```
nix develop --command cargo test -p sunset-cli --test command_dispatch
nix develop --command cargo test -p sunset-cli command::tests
```

Expected: all PASS.

- [ ] **Step 5: Commit**

```
git add crates/sunset-cli/src/dispatch.rs crates/sunset-cli/src/client.rs crates/sunset-cli/tests/command_dispatch.rs
git commit -m "$(cat <<'EOF'
sunset-cli: command dispatcher + Client::leave_room

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 11: ratatui rendering

**Files:**
- Modify: `crates/sunset-cli/src/ui/render.rs`

Render `TopState` + active `RoomView` + composer state to a `Frame`. Pure function — no I/O. Tested by rendering to a `TestBackend` buffer and asserting key cells.

- [ ] **Step 1: Implement render**

```rust
//! Pure rendering: TopState + RoomView + composer → ratatui Frame.

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph};

use crate::view::{RoomView, TopState};

pub struct ComposerState<'a> {
    pub buffer: &'a str,
    /// Cursor column relative to the buffer start.
    pub cursor: u16,
}

pub fn draw(frame: &mut Frame, top: &TopState, room: Option<&RoomView>, composer: &ComposerState) {
    let area = frame.area();

    // Vertical split: title bar (1), main (rest), composer (3).
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(3),
        ])
        .split(area);
    draw_title(frame, chunks[0], top);
    draw_main(frame, chunks[1], top, room);
    draw_composer(frame, chunks[2], composer);
}

fn draw_title(frame: &mut Frame, area: Rect, top: &TopState) {
    let title = match &top.active_room {
        Some(r) => format!(" sunset.chat — #{r}"),
        None => " sunset.chat".to_owned(),
    };
    frame.render_widget(
        Paragraph::new(title).style(Style::default().add_modifier(Modifier::BOLD)),
        area,
    );
}

fn draw_main(frame: &mut Frame, area: Rect, top: &TopState, room: Option<&RoomView>) {
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(22), Constraint::Min(1)])
        .split(area);
    draw_left_rail(frame, cols[0], top, room);
    draw_messages(frame, cols[1], top, room);
}

fn draw_left_rail(frame: &mut Frame, area: Rect, top: &TopState, room: Option<&RoomView>) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(top.open_rooms.len() as u16 + 2),
            Constraint::Min(1),
            Constraint::Length(top.relays.len() as u16 + 2),
        ])
        .split(area);
    let active = top.active_room.as_deref();
    let room_items: Vec<ListItem> = top
        .open_rooms
        .iter()
        .map(|r| {
            let marker = if Some(r.as_str()) == active { ">" } else { " " };
            ListItem::new(format!("{marker} #{r}"))
        })
        .collect();
    frame.render_widget(
        List::new(room_items).block(Block::default().borders(Borders::ALL).title("rooms")),
        rows[0],
    );

    let peer_items: Vec<ListItem> = match room {
        Some(v) => v
            .members
            .iter()
            .map(|m| {
                let glyph = match m.connection_mode {
                    "direct" => 'D',
                    "via_relay" => 'R',
                    _ => '?',
                };
                let name = m.name.as_deref().unwrap_or("(no name)");
                ListItem::new(format!("{glyph} {name}"))
            })
            .collect(),
        None => Vec::new(),
    };
    frame.render_widget(
        List::new(peer_items).block(Block::default().borders(Borders::ALL).title("peers")),
        rows[1],
    );

    let relay_items: Vec<ListItem> = top
        .relays
        .iter()
        .map(|r| {
            let glyph = match r.state {
                "connected" => '+',
                "connecting" => '~',
                "backoff" => '.',
                _ => '!',
            };
            ListItem::new(format!("{glyph} {}", short_label(&r.label)))
        })
        .collect();
    frame.render_widget(
        List::new(relay_items).block(Block::default().borders(Borders::ALL).title("relays")),
        rows[2],
    );
}

fn draw_messages(frame: &mut Frame, area: Rect, top: &TopState, room: Option<&RoomView>) {
    let mut lines: Vec<String> = Vec::new();
    for sys in &top.system_log {
        lines.push(format!("· {sys}"));
    }
    if let Some(v) = room {
        for m in &v.messages {
            let who = m
                .author_name
                .clone()
                .unwrap_or_else(|| short_pubkey(&m.author_pubkey));
            let when = format_time_ms(m.sent_at_ms);
            lines.push(format!("{when}  {who}: {}", m.body));
        }
    }
    // Tail-bias: render only the last `area.height as usize` lines.
    let h = area.height.saturating_sub(2) as usize;
    let start = lines.len().saturating_sub(h);
    let visible = lines[start..].join("\n");
    frame.render_widget(
        Paragraph::new(visible).block(
            Block::default()
                .borders(Borders::ALL)
                .title(top.active_room.clone().unwrap_or_else(|| " ".to_owned())),
        ),
        area,
    );
}

fn draw_composer(frame: &mut Frame, area: Rect, composer: &ComposerState) {
    let text = format!("> {}", composer.buffer);
    frame.render_widget(
        Paragraph::new(text).block(Block::default().borders(Borders::ALL)),
        area,
    );
    // Cursor: 1 (left border) + 2 ("> ") + composer.cursor.
    let x = area.x + 1 + 2 + composer.cursor;
    let y = area.y + 1;
    frame.set_cursor_position((x, y));
}

fn short_pubkey(pk: &[u8; 32]) -> String {
    hex::encode(&pk[..4])
}

fn short_label(s: &str) -> String {
    if s.len() <= 18 {
        s.to_owned()
    } else {
        format!("{}…", &s[..17])
    }
}

fn format_time_ms(ms: u64) -> String {
    let secs = (ms / 1000) as i64;
    let nsecs = ((ms % 1000) * 1_000_000) as u32;
    let dt = chrono::DateTime::from_timestamp(secs, nsecs).unwrap_or_default();
    let local: chrono::DateTime<chrono::Local> = dt.into();
    local.format("%H:%M").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use crate::view::{MemberRow, MessageLine, RelayRow, RoomView, TopState};

    fn buffer_lines(buf: &ratatui::buffer::Buffer) -> Vec<String> {
        (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf[(buf.area.x + x, buf.area.y + y)].symbol())
                    .collect::<String>()
            })
            .collect()
    }

    #[test]
    fn renders_active_room_in_title() {
        let backend = TestBackend::new(60, 12);
        let mut term = Terminal::new(backend).unwrap();

        let top = TopState {
            identity_hex: "deadbeef".into(),
            self_name: Some("alice".into()),
            active_room: Some("alpha".into()),
            open_rooms: vec!["alpha".into(), "beta".into()],
            relays: vec![RelayRow {
                label: "relay.example.com".into(),
                state: "connected",
                last_rtt_ms: Some(12),
            }],
            system_log: vec!["welcome".into()],
        };
        let view = RoomView {
            name: "alpha".into(),
            messages: vec![MessageLine {
                author_pubkey: [0x11; 32],
                author_name: Some("bob".into()),
                body: "hello".into(),
                sent_at_ms: 1_700_000_000_000,
                is_self: false,
            }],
            members: vec![MemberRow {
                pubkey: [0x11; 32],
                name: Some("bob".into()),
                connection_mode: "via_relay",
                presence: "online",
            }],
        };
        let composer = ComposerState {
            buffer: "/help",
            cursor: 5,
        };
        term.draw(|f| draw(f, &top, Some(&view), &composer)).unwrap();
        let lines = buffer_lines(term.backend().buffer());
        assert!(lines[0].contains("sunset.chat"), "title: {}", lines[0]);
        assert!(lines[0].contains("alpha"), "title room: {}", lines[0]);
        assert!(
            lines.iter().any(|l| l.contains("> #alpha")),
            "active room marker missing"
        );
        assert!(
            lines.iter().any(|l| l.contains("R bob")),
            "via_relay glyph + name missing"
        );
        assert!(
            lines.iter().any(|l| l.contains("bob: hello")),
            "message missing"
        );
        assert!(
            lines.iter().any(|l| l.contains("> /help")),
            "composer missing"
        );
    }
}
```

- [ ] **Step 2: Run tests**

```
nix develop --command cargo test -p sunset-cli --lib ui::render
```

Expected: PASS.

- [ ] **Step 3: Commit**

```
git add crates/sunset-cli/src/ui/render.rs
git commit -m "$(cat <<'EOF'
sunset-cli: ratatui rendering with TestBackend snapshot

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 12: Composer + input loop

**Files:**
- Modify: `crates/sunset-cli/src/ui/input.rs`
- Modify: `crates/sunset-cli/src/ui/mod.rs`

- [ ] **Step 1: Implement composer + key handler**

```rust
// crates/sunset-cli/src/ui/input.rs

//! Composer state + key event → command pipeline.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::command::{Command, parse};

#[derive(Default)]
pub struct Composer {
    pub buffer: String,
    pub cursor: usize, // byte offset; rendering converts to col below.
}

impl Composer {
    pub fn cursor_col(&self) -> u16 {
        // ASCII-only assumption for now: the buffer width equals byte
        // length. Wider chars would need a unicode-width pass — out
        // of v1 scope.
        self.buffer.len().min(self.cursor) as u16
    }
}

pub enum KeyOutcome {
    Nothing,
    Submit(Command),
    Quit,
    Redraw,
}

pub fn handle_key(composer: &mut Composer, key: KeyEvent) -> KeyOutcome {
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        if let KeyCode::Char('c') = key.code {
            return KeyOutcome::Quit;
        }
    }
    match key.code {
        KeyCode::Enter => {
            let line = std::mem::take(&mut composer.buffer);
            composer.cursor = 0;
            KeyOutcome::Submit(parse(&line))
        }
        KeyCode::Char(c) => {
            composer.buffer.push(c);
            composer.cursor = composer.buffer.len();
            KeyOutcome::Redraw
        }
        KeyCode::Backspace => {
            composer.buffer.pop();
            composer.cursor = composer.buffer.len();
            KeyOutcome::Redraw
        }
        KeyCode::Esc => {
            composer.buffer.clear();
            composer.cursor = 0;
            KeyOutcome::Redraw
        }
        _ => KeyOutcome::Nothing,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn k(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::empty())
    }

    #[test]
    fn typing_appends_to_buffer() {
        let mut c = Composer::default();
        for ch in "hi".chars() {
            handle_key(&mut c, k(ch));
        }
        assert_eq!(c.buffer, "hi");
    }

    #[test]
    fn enter_submits_parsed_command() {
        let mut c = Composer::default();
        for ch in "/help".chars() {
            handle_key(&mut c, k(ch));
        }
        let out = handle_key(&mut c, KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()));
        assert!(matches!(out, KeyOutcome::Submit(Command::Help)));
        assert!(c.buffer.is_empty());
    }

    #[test]
    fn ctrl_c_quits() {
        let mut c = Composer::default();
        let out = handle_key(
            &mut c,
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
        );
        assert!(matches!(out, KeyOutcome::Quit));
    }

    #[test]
    fn backspace_pops() {
        let mut c = Composer::default();
        for ch in "abc".chars() {
            handle_key(&mut c, k(ch));
        }
        handle_key(&mut c, KeyEvent::new(KeyCode::Backspace, KeyModifiers::empty()));
        assert_eq!(c.buffer, "ab");
    }

    #[test]
    fn esc_clears() {
        let mut c = Composer::default();
        for ch in "abc".chars() {
            handle_key(&mut c, k(ch));
        }
        handle_key(&mut c, KeyEvent::new(KeyCode::Esc, KeyModifiers::empty()));
        assert!(c.buffer.is_empty());
    }
}
```

- [ ] **Step 2: Implement `ui/mod.rs` (App orchestration)**

```rust
//! ratatui App: terminal setup, input pump, render loop.
//!
//! Lives entirely on the local task-set; no Send bounds. The input
//! pump runs in a `spawn_blocking` task and pushes `crossterm::Event`
//! across an mpsc.

pub mod input;
pub mod render;

use std::io::{Stdout, stdout};
use std::rc::Rc;
use std::time::Duration;

use crossterm::ExecutableCommand;
use crossterm::event::{Event, EventStream, KeyEventKind};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use futures::StreamExt;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

use crate::client::Client;
use crate::dispatch::{DispatchOutcome, dispatch};
use crate::ui::input::{Composer, KeyOutcome, handle_key};
use crate::ui::render::{ComposerState, draw};

pub async fn run_app(client: Rc<Client>) -> std::io::Result<()> {
    enable_raw_mode()?;
    let mut out = stdout();
    out.execute(EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(out);
    let mut terminal = Terminal::new(backend)?;

    let result = drive(&mut terminal, &client).await;

    disable_raw_mode()?;
    terminal.backend_mut().execute(LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    result
}

async fn drive(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    client: &Rc<Client>,
) -> std::io::Result<()> {
    let mut composer = Composer::default();
    let mut events = EventStream::new();
    repaint(terminal, client, &composer)?;

    loop {
        tokio::select! {
            _ = client.notify.notified() => {
                repaint(terminal, client, &composer)?;
            }
            ev = events.next() => {
                let Some(Ok(ev)) = ev else { break };
                if let Event::Key(key) = ev {
                    if key.kind == KeyEventKind::Release { continue; }
                    match handle_key(&mut composer, key) {
                        KeyOutcome::Submit(cmd) => {
                            match dispatch(client, cmd).await {
                                DispatchOutcome::Quit => break,
                                DispatchOutcome::Continue => {}
                            }
                            repaint(terminal, client, &composer)?;
                        }
                        KeyOutcome::Quit => break,
                        KeyOutcome::Redraw => repaint(terminal, client, &composer)?,
                        KeyOutcome::Nothing => {}
                    }
                }
            }
            _ = tokio::time::sleep(Duration::from_millis(500)) => {
                // Heartbeat repaint so timestamps stay roughly fresh.
                repaint(terminal, client, &composer)?;
            }
        }
    }
    Ok(())
}

fn repaint(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    client: &Rc<Client>,
    composer: &Composer,
) -> std::io::Result<()> {
    let top = client.snapshot_top();
    let active = top.active_room.clone();
    let room = active.as_deref().and_then(|n| client.snapshot_room(n));
    terminal.draw(|f| {
        let composer_state = ComposerState {
            buffer: &composer.buffer,
            cursor: composer.cursor_col(),
        };
        draw(f, &top, room.as_ref(), &composer_state);
    })?;
    Ok(())
}
```

- [ ] **Step 3: Run unit tests**

```
nix develop --command cargo test -p sunset-cli --lib ui::input
nix develop --command cargo clippy -p sunset-cli --all-targets -- -D warnings
```

Expected: PASS.

- [ ] **Step 4: Commit**

```
git add crates/sunset-cli/src/ui/
git commit -m "$(cat <<'EOF'
sunset-cli: composer key handler + ratatui app loop

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 13: Binary entry point

**Files:**
- Modify: `crates/sunset-cli/src/main.rs`

- [ ] **Step 1: Implement main**

```rust
//! sunset-cli binary entry.

use std::path::PathBuf;

use clap::Parser;
use sunset_cli::client::Client;
use sunset_cli::identity::{default_path, load_or_generate};
use sunset_cli::ui::run_app;
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(name = "sunset-cli", about = "sunset.chat native ratatui client")]
struct Args {
    /// Relay to connect to. Accepts wss://host:port,
    /// ws://host:port, wts://host[:port], wt://host[:port], or a
    /// hostname (resolved via the relay's identity descriptor).
    #[arg(long, env = "SUNSET_RELAY")]
    relay: Option<String>,

    /// Identity file path. Defaults to <config_dir>/sunset/identity.bin
    /// (or $SUNSET_IDENTITY_PATH).
    #[arg(long, env = "SUNSET_IDENTITY_PATH")]
    identity: Option<PathBuf>,

    /// Display name to publish in presence heartbeats.
    #[arg(long, env = "SUNSET_NAME")]
    name: Option<String>,

    /// Room to auto-join on startup.
    #[arg(long)]
    join: Option<String>,
}

fn main() -> std::io::Result<()> {
    // Logs to stderr so the alternate-screen TUI on stdout stays clean.
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn")))
        .init();

    let args = Args::parse();

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let local = tokio::task::LocalSet::new();

    runtime.block_on(local.run_until(async move {
        let id_path = args.identity.unwrap_or_else(default_path);
        let identity = match load_or_generate(&id_path).await {
            Ok(id) => id,
            Err(e) => {
                eprintln!("sunset-cli: identity error: {e}");
                std::process::exit(2);
            }
        };

        let client = Client::start(identity);
        if let Some(name) = args.name {
            client.set_self_name(&name);
        }
        if let Some(url) = args.relay {
            if let Err(e) = client.add_relay(url).await {
                client.append_system(format!("relay add failed at startup: {e}"));
            }
        }
        if let Some(room) = args.join {
            if let Err(e) = client.join_room(&room).await {
                client.append_system(format!("auto-join failed: {e}"));
            }
        }

        if let Err(e) = run_app(client).await {
            eprintln!("sunset-cli: ui error: {e}");
            std::process::exit(1);
        }
    }));
    Ok(())
}
```

- [ ] **Step 2: Build**

```
nix develop --command cargo build -p sunset-cli --release
```

Expected: success. Smoke-run `target/release/sunset-cli --help` and confirm clap output appears (do not run interactively in CI).

- [ ] **Step 3: Commit**

```
git add crates/sunset-cli/src/main.rs
git commit -m "$(cat <<'EOF'
sunset-cli: main() — clap + identity + Client + ratatui app

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 14: Final lint / fmt / full test sweep

- [ ] **Step 1: cargo fmt**

```
nix develop --command cargo fmt --all --check
```

If it fails, run `cargo fmt --all` and amend the relevant commit.

- [ ] **Step 2: scripts/check-no-clippy-allow.sh**

```
./scripts/check-no-clippy-allow.sh
```

Expected: PASS (no `#[allow(clippy::...)]` / `#[expect(clippy::...)]` in `crates/sunset-cli/`).

- [ ] **Step 3: cargo clippy --workspace**

```
nix develop --command cargo clippy --workspace --all-features --all-targets -- -D warnings
```

Expected: PASS.

- [ ] **Step 4: cargo test --workspace**

```
nix develop --command cargo test --workspace --all-features
```

Expected: all PASS.

- [ ] **Step 5: Push and open PR**

```
git push -u origin feature/sunset-cli
gh pr create --title "sunset-cli: native ratatui chat client (voice deferred)" --body "$(cat <<'EOF'
## Summary
- New workspace crate `sunset-cli`: ratatui-based native client mirroring the web client's chat capabilities.
- Connect via `--relay <url>` accepting WS, WSS, WT, WTS, or hostname (descriptor lookup). FallbackTransport routes WT→WS automatically.
- `/command` interface: /help, /join, /switch, /leave, /rooms, /peers, /relays, /relay add, /name, /me, /quit. Bare text sends a chat message into the active room.
- Headless `core::Client` separated from the TUI so integration tests don't touch terminal state.
- Integration tests: WS roundtrip, WT roundtrip, peer visibility (`connection_mode == "via_relay"`), command dispatcher.

## Voice scoping
`/voice` is a stub in this PR — it prints a single line pointing the user at the web client. Native voice needs either a `sunset-sync-webrtc-native` crate or a `cpal`-driven voice-over-relay path; either warrants its own design + plan. The spec ("Out of scope for v1") documents the rationale.

## Test plan
- [ ] `nix develop --command cargo fmt --all --check`
- [ ] `./scripts/check-no-clippy-allow.sh`
- [ ] `nix develop --command cargo clippy --workspace --all-features --all-targets -- -D warnings`
- [ ] `nix develop --command cargo test --workspace --all-features`
- [ ] Re-run full CI 5× consecutively (stability gate)

🤖 Generated with [Claude Code](https://claude.com/claude-code)
EOF
)"
```

---

## Self-review against the spec

1. **Spec coverage:**
   - Crate scaffolding → Task 1.
   - Identity load/generate → Task 2.
   - Resolver adapter → Task 3.
   - Peer + transport wiring → Task 4.
   - Snapshot view types → Task 5.
   - Client (Peer wrapper + intents) → Task 6.
   - WS roundtrip integration test → Task 7.
   - WT roundtrip + peer visibility → Task 8.
   - /command parser → Task 9.
   - Command dispatch + Client::leave_room → Task 10.
   - ratatui rendering + render unit test → Task 11.
   - Composer + key handler + app loop → Task 12.
   - Binary entry (clap, identity, run_app) → Task 13.
   - Final lint/fmt/test + PR → Task 14.

2. **Placeholder scan:** No "TBD"/"TODO"/"similar to". Each step has the exact code or exact command.

3. **Type consistency:** `Client` constructed via `Client::start`, used as `Rc<Client>` everywhere. `Command` enum used identically across parser, dispatch, and tests. `TopState`/`RoomView`/`MessageLine`/`MemberRow`/`RelayRow` consistently spelled.

4. **CLAUDE.md compliance:** No `#[allow(clippy::...)]`. No `wait_for(internal_state)`. No `tokio::time::sleep` masking races (the only sleep is the eventual-poll util's 25 ms tick, capped by deadline; deadlines are 10 s = the UX bar). All commands use `nix develop --command`. Each commit ends with the required `Co-Authored-By` trailer.
