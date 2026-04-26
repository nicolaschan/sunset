# sunset-sync Core Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the `sunset-sync` crate — peer-to-peer replication of `sunset-store` data over a pluggable transport, including bootstrap, push, pull (digest exchange + fetch), and anti-entropy. Ships with an in-memory `TestTransport` so the entire plan is testable via `cargo test` without a browser.

**Architecture:** Two thin trait surfaces (`Transport` + `TransportConnection`) abstract the wire. The `SyncEngine<S, T>` owns one `Arc<dyn Store>` and one `Transport`, routes events between local store and connected peers, and stores peer subscriptions as KV entries under `_sunset-sync/subscribe` so they replicate via the same mechanism as application data (no `SubscribeRequest` wire message). Per-peer connection tasks decode incoming `SyncMessage`s and route them through the engine; an outbound pump subscribes to the local store and forwards events whose `(verifying_key, name)` matches a connected peer's filter. Catch-up uses bloom-based digest exchange + fetch; anti-entropy is the same machinery on a timer.

**Tech Stack:** Rust 2024 edition, `async-trait` (`?Send` futures for WASM compat), `postcard` (canonical wire format, frozen v1), `bytes`, `futures` (async streams), `tokio` (`sync`, `time`, `macros`, `rt`), `blake3` (bloom hashing — already a workspace dep).

**Spec:** [`docs/superpowers/specs/2026-04-25-sunset-store-and-sync-design.md`](../specs/2026-04-25-sunset-store-and-sync-design.md) — section "sunset-sync" (lines 243–412) plus "Cross-crate concerns" (line 414+).

**Parent architecture spec:** [`docs/superpowers/specs/2026-04-25-sunset-chat-architecture-design.md`](../specs/2026-04-25-sunset-chat-architecture-design.md)

**Predecessor plans (already merged):**
- [`2026-04-25-sunset-store-core-and-memory-backend.md`](2026-04-25-sunset-store-core-and-memory-backend.md) — Plan 1
- [`2026-04-26-sunset-store-fs-backend.md`](2026-04-26-sunset-store-fs-backend.md) — Plan 2

The `sunset-store` trait, types, error model, filter language, and conformance suite are merged. `sunset-store-memory` and `sunset-store-fs` backends are merged. **This plan does not modify any of those crates.**

**Out of scope (deferred to follow-up plans):**
- WebRTC transport implementation (Plan 5)
- IndexedDB backend (Plan 3)
- Reconnect / retry policies for dropped Transport connections (host-driven via re-`add_peer` in v1)
- Backpressure beyond unbounded mpsc buffering
- Multi-range partitioning of `DigestExchange` (single `DigestRange::All` in v1)
- Catch-up pagination
- Concrete schema for the trust-set KV entry (defined jointly with the identity subsystem in a later plan)

---

## File Structure

```
sunset/
├── Cargo.toml                                   # workspace root (modify: add member, deps)
└── crates/
    └── sunset-sync/                             # NEW crate
        ├── Cargo.toml
        ├── src/
        │   ├── lib.rs                           # module declarations + public re-exports
        │   ├── error.rs                         # Error + Result alias
        │   ├── types.rs                         # PeerId, PeerAddr, SyncConfig, TrustSet
        │   ├── reserved.rs                      # reserved name constants
        │   ├── transport.rs                     # Transport + TransportConnection traits
        │   ├── test_transport.rs                # InMemoryTransport + TestNetwork (test-helpers feature)
        │   ├── message.rs                       # SyncMessage enum + DigestRange + postcard helpers
        │   ├── digest.rs                        # BloomFilter + DigestRound machinery
        │   ├── subscription_registry.rs         # in-memory peer-filter index
        │   ├── peer.rs                          # per-peer connection task (Hello, msg dispatch)
        │   └── engine.rs                        # SyncEngine struct + new + add_peer + publish_subscription + set_trust + run
        └── tests/
            └── two_peer_sync.rs                 # integration test (alice ↔ bob via TestNetwork)
```

Boundaries:
- `error.rs` — single `Error` enum used everywhere in the crate.
- `types.rs` — pure data: `PeerId`, `PeerAddr`, `SyncConfig`, `TrustSet`. No async, no I/O.
- `reserved.rs` — namespace constants (`_sunset-sync/subscribe`, etc.).
- `transport.rs` — trait declarations only.
- `test_transport.rs` — in-memory implementation gated behind a `test-helpers` feature so production builds don't compile it.
- `message.rs` — `SyncMessage` wire enum + `DigestRange` + helpers; pure data.
- `digest.rs` — `BloomFilter` + `DigestRound` (the digest-exchange + fetch state machine, transport-agnostic).
- `subscription_registry.rs` — in-memory map of `PeerId -> Filter` rebuilt from store events. No I/O.
- `peer.rs` — async task that owns one `TransportConnection`, reads/writes `SyncMessage`s, dispatches to engine via mpsc.
- `engine.rs` — coordinator: owns store handle, transport, peer table, subscription registry, trust set, runs anti-entropy timer, hosts the public API (`new`/`add_peer`/`publish_subscription`/`set_trust`/`run`).

Each file has one clear responsibility, kept small enough to hold in one mental model.

---

## Cross-cutting design notes

These apply across multiple tasks. Read once before Task 1.

### Concurrency model

`SyncEngine::run()` is a single async function that runs as long as the engine is alive. It uses `tokio::select!` plus `tokio::task::spawn_local` for per-peer tasks. The caller is expected to invoke `run()` inside a `LocalSet` (native) or directly on a single-threaded WASM executor. **Do not** assume `Send` on engine state; everything is `?Send`.

The engine holds:
- `Arc<S>` where `S: Store` (the local store).
- `T` where `T: Transport` (owned by the engine; the engine is the sole `accept` driver).
- `tokio::sync::Mutex<EngineState>` for mutable state (peer table, subscription registry, trust set, current local-subscription handle). Held only across short, sync sections.

### Communication channels

- **Engine → per-peer task:** `mpsc::UnboundedSender<OutboundMsg>` per peer. The engine's outbound pump pushes `SyncMessage`s into the right peer's channel; the peer task drains it and writes to the transport.
- **Per-peer task → engine:** `mpsc::UnboundedSender<InboundEvent>` shared by all peer tasks. Each delivery carries `(PeerId, SyncMessage)` plus connection-lifecycle events (peer-disconnected). The engine's main `select!` arm consumes from this.
- **Public API → engine:** `mpsc::UnboundedSender<EngineCommand>` for `add_peer`, `set_trust`, `publish_subscription`. Each command carries a `oneshot::Sender<Result<()>>` for await-completion semantics in the caller.

All channels are unbounded for v1 — same trade-off as the store backends' subscription channels. Backpressure is a follow-up.

### Subscription model

Every peer publishes its current filter as a signed KV entry under `(local_pubkey, "_sunset-sync/subscribe")` with `value_hash = blake3(postcard(Filter))`. Other peers learn of it via the regular replication path. The engine maintains an in-memory `SubscriptionRegistry` (peer -> filter) by subscribing to local store events on `Filter::Namespace("_sunset-sync/subscribe")` and re-parsing on each insert/replace.

The engine's local store subscription combines two filters:
1. `Filter::Namespace(b"_sunset-sync/subscribe")` — to track peer subscriptions.
2. The union of all currently-known peer filters — for outbound push routing.

Whenever the registry changes (peer added, filter updated), the engine re-subscribes with a fresh union filter.

### Wire format

`SyncMessage` is postcard-encoded bytes. Length-prefix framing is the transport's responsibility (the transport's `recv_reliable` returns one whole message at a time; for stream-based transports, the transport implements its own framing layer).

For v1, frame format is **a single postcard-encoded `SyncMessage`** per `recv_reliable`/`send_reliable` call — no extra length prefix at this layer. Transports that need framing add it inside their own implementation.

### Trust filter placement

The trust filter check (`event.verifying_key ∈ trust_set`) happens **inside the per-peer task, before any store mutation**. Specifically: when an `EventDelivery` is received, the peer task iterates entries and discards ones whose `verifying_key` isn't trusted, then forwards the survivors to the engine for `store.insert`. The trust set is read from the engine via a non-blocking shared reference (`Arc<RwLock<TrustSet>>`).

### `TestTransport` shape

Tests construct a `TestNetwork`, then derive multiple `TestTransport`s from it (one per simulated peer). Each `TestTransport` has a unique `PeerAddr`. `connect(peer_addr)` looks up the matching transport in the network and creates a paired `TestConnection` on both sides; `accept()` awaits the next pending connection. Reliable channel uses `mpsc::UnboundedSender<Bytes>` per direction; unreliable channel is a no-op (returns `pending`) since v1 sync only uses reliable.

`TestNetwork` lives in `test_transport.rs` behind a feature gate (`test-helpers`) so production builds don't ship the test machinery.

### Error mapping

`sunset_sync::Error` wraps `sunset_store::Error` via `#[from]`. Transport / decode / protocol errors get their own variants. `Closed` for graceful shutdown.

---

## Tasks

### Task 1: Add `sunset-sync` crate skeleton

**Files:**
- Modify: `Cargo.toml` (workspace root: add member + workspace dep entry)
- Create: `crates/sunset-sync/Cargo.toml`
- Create: `crates/sunset-sync/src/lib.rs`
- Create: empty placeholder modules

- [ ] **Step 1: Edit `Cargo.toml` (workspace root)**

In the `[workspace]` `members` array, append `"crates/sunset-sync"`. In `[workspace.dependencies]`, append:

```toml
sunset-sync = { path = "crates/sunset-sync" }
```

(Other deps — `async-trait`, `bytes`, `futures`, `postcard`, `serde`, `thiserror`, `tokio`, `blake3`, `async-stream` — are already workspace deps.)

- [ ] **Step 2: Create `crates/sunset-sync/Cargo.toml`**

```toml
[package]
name = "sunset-sync"
version.workspace = true
edition.workspace = true
license.workspace = true
rust-version.workspace = true

[lints]
workspace = true

[dependencies]
async-trait.workspace = true
async-stream.workspace = true
blake3.workspace = true
bytes.workspace = true
futures.workspace = true
postcard.workspace = true
serde.workspace = true
sunset-store.workspace = true
thiserror.workspace = true
tokio = { workspace = true, features = ["sync", "rt", "macros", "time"] }

[dev-dependencies]
sunset-store = { workspace = true, features = ["test-helpers"] }
sunset-store-memory.workspace = true
tokio = { workspace = true, features = ["macros", "rt", "rt-multi-thread", "time", "sync"] }

[features]
test-helpers = []
```

The `test-helpers` feature gates `test_transport.rs` so it's available to integration tests + downstream consumers but not to production builds.

- [ ] **Step 3: Create `crates/sunset-sync/src/lib.rs`**

```rust
//! sunset-sync: peer-to-peer replication of sunset-store data over a pluggable
//! transport.
//!
//! See `docs/superpowers/specs/2026-04-25-sunset-store-and-sync-design.md` for design.

pub mod digest;
pub mod engine;
pub mod error;
pub mod message;
pub mod peer;
pub mod reserved;
pub mod subscription_registry;
pub mod transport;
pub mod types;

#[cfg(feature = "test-helpers")]
pub mod test_transport;

pub use engine::SyncEngine;
pub use error::{Error, Result};
pub use message::{DigestRange, SyncMessage};
pub use transport::{Transport, TransportConnection};
pub use types::{PeerAddr, PeerId, SyncConfig, TrustSet};
```

- [ ] **Step 4: Create empty placeholder files**

Each contains a single-line module doc comment so the build passes:

```rust
// crates/sunset-sync/src/error.rs
//! Error type — see Task 2.
```
```rust
// crates/sunset-sync/src/types.rs
//! Core types — see Task 3.
```
```rust
// crates/sunset-sync/src/reserved.rs
//! Reserved name constants — see Task 6.
```
```rust
// crates/sunset-sync/src/transport.rs
//! Transport traits — see Task 4.
```
```rust
// crates/sunset-sync/src/test_transport.rs
//! In-memory test transport — see Task 5.
```
```rust
// crates/sunset-sync/src/message.rs
//! Wire-protocol messages — see Task 6.
```
```rust
// crates/sunset-sync/src/digest.rs
//! Digest exchange machinery — see Task 7 + Task 14.
```
```rust
// crates/sunset-sync/src/subscription_registry.rs
//! Subscription registry — see Task 8.
```
```rust
// crates/sunset-sync/src/engine.rs
//! SyncEngine — see Tasks 9 + 11–15.

pub struct SyncEngine;
```
```rust
// crates/sunset-sync/src/peer.rs
//! Per-peer connection task — see Task 10.
```

`engine.rs` needs the `pub struct SyncEngine;` so `lib.rs`'s `pub use engine::SyncEngine;` compiles.

- [ ] **Step 5: Verify the build**

Run: `nix develop --command cargo build -p sunset-sync`
Expected: clean compile.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml crates/sunset-sync/
git commit -m "Scaffold sunset-sync crate with empty module placeholders"
```

---

### Task 2: Error type

**Files:**
- Modify: `crates/sunset-sync/src/error.rs`

- [ ] **Step 1: Write `error.rs`**

```rust
//! Error type for sunset-sync.

use thiserror::Error;

/// Result alias.
pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum Error {
    /// Transport layer reported a failure (connect, send, recv, close).
    #[error("transport: {0}")]
    Transport(String),

    /// Underlying store returned an error during sync work.
    #[error("store: {0}")]
    Store(#[from] sunset_store::Error),

    /// Failed to decode an incoming wire message.
    #[error("decode: {0}")]
    Decode(String),

    /// Protocol invariant violated by the remote (unexpected message,
    /// version mismatch, malformed digest, etc.).
    #[error("protocol: {0}")]
    Protocol(String),

    /// Per-peer error attributable to a specific peer.
    #[error("peer: {0}")]
    Peer(String),

    /// Engine has been closed (run() returned, channels dropped).
    #[error("closed")]
    Closed,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn store_error_converts_via_from() {
        let store_err = sunset_store::Error::Stale;
        let sync_err: Error = store_err.into();
        assert_eq!(sync_err, Error::Store(sunset_store::Error::Stale));
    }
}
```

- [ ] **Step 2: Run tests**

Run: `nix develop --command cargo test -p sunset-sync error::`
Expected: 1 PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/sunset-sync/src/error.rs
git commit -m "Add sunset_sync::Error with store conversion"
```

---

### Task 3: Core types

**Files:**
- Modify: `crates/sunset-sync/src/types.rs`

- [ ] **Step 1: Write `types.rs`**

```rust
//! Core types: PeerId, PeerAddr, SyncConfig, TrustSet.

use std::collections::HashSet;
use std::time::Duration;

use bytes::Bytes;
use serde::{Deserialize, Serialize};

use sunset_store::{Filter, VerifyingKey};

use crate::reserved;

/// A peer's identity. Currently transparent over `VerifyingKey` — the peer
/// is identified by its public key. Future schemes (e.g., a separate
/// transport-layer identity) can extend this without breaking callers.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PeerId(pub VerifyingKey);

impl PeerId {
    pub fn verifying_key(&self) -> &VerifyingKey { &self.0 }
}

/// Transport-specific peer address. The transport interprets these bytes
/// (e.g., a WebRTC SDP signaling endpoint, a TestNetwork peer name).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PeerAddr(pub Bytes);

impl PeerAddr {
    pub fn new(bytes: impl Into<Bytes>) -> Self {
        Self(bytes.into())
    }
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

/// Tunables for a `SyncEngine`. v1 uses fixed defaults; tuning is a follow-up.
#[derive(Clone, Debug)]
pub struct SyncConfig {
    pub protocol_version: u32,
    pub anti_entropy_interval: Duration,
    pub bloom_size_bits: usize,
    pub bloom_hash_fns: u32,
    /// Filter used for the bootstrap digest exchange (always
    /// `_sunset-sync/subscribe` namespace).
    pub bootstrap_filter: Filter,
}

impl Default for SyncConfig {
    fn default() -> Self {
        Self {
            protocol_version: 1,
            anti_entropy_interval: Duration::from_secs(30),
            bloom_size_bits: 4096,
            bloom_hash_fns: 4,
            bootstrap_filter: Filter::Namespace(reserved::SUBSCRIBE_NAME.into()),
        }
    }
}

/// Whose entries this peer is willing to accept on inbound sync. Set via
/// `SyncEngine::set_trust`. Default for v1 is `All` (accept anyone — typical
/// for an open chat room).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TrustSet {
    All,
    Whitelist(HashSet<VerifyingKey>),
}

impl TrustSet {
    pub fn contains(&self, vk: &VerifyingKey) -> bool {
        match self {
            TrustSet::All => true,
            TrustSet::Whitelist(set) => set.contains(vk),
        }
    }
}

impl Default for TrustSet {
    fn default() -> Self { TrustSet::All }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vk(b: &[u8]) -> VerifyingKey {
        VerifyingKey::new(Bytes::copy_from_slice(b))
    }

    #[test]
    fn trust_all_accepts_everyone() {
        let t = TrustSet::All;
        assert!(t.contains(&vk(b"alice")));
        assert!(t.contains(&vk(b"bob")));
    }

    #[test]
    fn trust_whitelist_only_accepts_listed() {
        let mut s = HashSet::new();
        s.insert(vk(b"alice"));
        let t = TrustSet::Whitelist(s);
        assert!(t.contains(&vk(b"alice")));
        assert!(!t.contains(&vk(b"bob")));
    }

    #[test]
    fn sync_config_default_is_v1() {
        let c = SyncConfig::default();
        assert_eq!(c.protocol_version, 1);
        assert_eq!(c.bloom_size_bits, 4096);
        assert_eq!(c.bloom_hash_fns, 4);
    }
}
```

Note: this file imports `crate::reserved`, which is empty until Task 6 lands. To make Task 3 standalone, fill in the `reserved.rs` constant inline in this task too (just the one constant — Task 6 will move it):

- [ ] **Step 2: Add the bootstrap-namespace constant to `reserved.rs` (one line)**

```rust
//! Reserved name constants for sunset-sync metadata.

/// Subscription filter entries are stored under this namespace.
pub const SUBSCRIBE_NAME: &[u8] = b"_sunset-sync/subscribe";
```

(Task 6 will add a few more reserved names and tests; this single constant is enough for now.)

- [ ] **Step 3: Run tests**

Run: `nix develop --command cargo test -p sunset-sync types::`
Expected: 3 PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/sunset-sync/src/types.rs crates/sunset-sync/src/reserved.rs
git commit -m "Add PeerId, PeerAddr, SyncConfig, TrustSet types"
```

---

### Task 4: Transport traits

**Files:**
- Modify: `crates/sunset-sync/src/transport.rs`

- [ ] **Step 1: Write `transport.rs`**

```rust
//! Transport trait surface that hosts implement (browser WebRTC, native
//! webrtc-rs, the in-memory `TestTransport`, etc.).

use async_trait::async_trait;
use bytes::Bytes;

use crate::error::Result;
use crate::types::{PeerAddr, PeerId};

/// A factory for inbound and outbound peer connections.
///
/// Implementations are `?Send`-compatible so they work in single-threaded
/// WASM as well as multi-threaded native runtimes.
#[async_trait(?Send)]
pub trait Transport {
    type Connection: TransportConnection;

    /// Initiate a connection to `addr`. Returns when the connection is
    /// established (handshake complete) or fails.
    async fn connect(&self, addr: PeerAddr) -> Result<Self::Connection>;

    /// Wait for the next inbound connection. Returns when one arrives.
    /// Implementations that don't accept inbound connections (e.g., a
    /// dial-only client) should return a future that never resolves.
    async fn accept(&self) -> Result<Self::Connection>;
}

/// One peer connection. Carries a reliable channel (used by sunset-sync) and
/// an unreliable channel (used by sunset-core for voice; a no-op for v1).
#[async_trait(?Send)]
pub trait TransportConnection {
    /// Send one message on the reliable channel. Whole-message framing is the
    /// transport's responsibility — `bytes` is one whole `SyncMessage`.
    async fn send_reliable(&self, bytes: Bytes) -> Result<()>;

    /// Receive one whole message from the reliable channel. Blocks until a
    /// message is available, the channel is closed, or an error occurs.
    async fn recv_reliable(&self) -> Result<Bytes>;

    /// Send one message on the unreliable channel (datagram-shaped).
    /// Reserved for sunset-core voice; sunset-sync does not use it.
    async fn send_unreliable(&self, bytes: Bytes) -> Result<()>;

    /// Receive one message from the unreliable channel. May return spurious
    /// errors if datagrams are lost in transit; callers should not rely on
    /// this for protocol state.
    async fn recv_unreliable(&self) -> Result<Bytes>;

    /// The peer's identity at the other end of this connection.
    fn peer_id(&self) -> PeerId;

    /// Close the connection. Subsequent send/recv calls return
    /// `Error::Transport("closed")` or similar.
    async fn close(&self) -> Result<()>;
}
```

(Note: the spec showed `close(self)` consuming. We use `&self` here so that the connection can be held inside an `Arc` or shared between concurrent tasks; the close is idempotent.)

- [ ] **Step 2: Verify the build**

Run: `nix develop --command cargo build -p sunset-sync`
Expected: clean compile.

- [ ] **Step 3: Commit**

```bash
git add crates/sunset-sync/src/transport.rs
git commit -m "Add Transport + TransportConnection trait declarations"
```

---

### Task 5: In-memory test transport

**Files:**
- Modify: `crates/sunset-sync/src/test_transport.rs`

The whole module is gated behind the `test-helpers` feature so production builds don't ship it. Tests in this same crate enable the feature via `dev-dependencies`.

- [ ] **Step 1: Write `test_transport.rs`**

```rust
//! In-memory `Transport` implementation for tests.
//!
//! `TestNetwork` is a registry that mediates between `TestTransport`s.
//! Each transport has a `PeerAddr`; calling `connect(addr)` looks up the
//! matching transport in the network and creates a paired `TestConnection`
//! on both sides. Unreliable channels are no-ops (return pending) since
//! sunset-sync v1 only uses reliable.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;

use async_trait::async_trait;
use bytes::Bytes;
use tokio::sync::{mpsc, oneshot};

use crate::error::{Error, Result};
use crate::transport::{Transport, TransportConnection};
use crate::types::{PeerAddr, PeerId};

/// Routing fabric shared by all `TestTransport`s in a test.
#[derive(Clone, Default)]
pub struct TestNetwork {
    /// Maps a peer's address to (peer_id, accept-queue sender). The peer_id is
    /// kept here so a connecting peer can learn the acceptor's identity
    /// before any application-layer handshake. This is a TEST-only convenience;
    /// production transports learn peer_id from the connection handshake.
    inboxes: Rc<RefCell<HashMap<PeerAddr, (PeerId, mpsc::UnboundedSender<ConnectRequest>)>>>,
}

impl TestNetwork {
    pub fn new() -> Self { Self::default() }

    /// Build a `TestTransport` with the given identity and address.
    /// Registering the address on the network makes it `connect`able.
    pub fn transport(&self, peer_id: PeerId, addr: PeerAddr) -> TestTransport {
        let (tx, rx) = mpsc::unbounded_channel::<ConnectRequest>();
        self.inboxes
            .borrow_mut()
            .insert(addr.clone(), (peer_id.clone(), tx));
        TestTransport {
            peer_id,
            addr,
            net: self.clone(),
            accept_rx: Rc::new(RefCell::new(rx)),
        }
    }
}

/// A connect-request crossing the network from initiator to acceptor.
struct ConnectRequest {
    /// Initiator's identity.
    from_peer: PeerId,
    /// Channel pair to install on the acceptor's side.
    /// (acceptor will send via `tx_to_initiator`; receive via `rx_from_initiator`.)
    tx_to_initiator: mpsc::UnboundedSender<Bytes>,
    rx_from_initiator: mpsc::UnboundedReceiver<Bytes>,
    /// Reply channel: acceptor signals "connection installed" so the
    /// initiator can complete `connect()`.
    ready: oneshot::Sender<()>,
}

#[derive(Clone)]
pub struct TestTransport {
    peer_id: PeerId,
    addr: PeerAddr,
    net: TestNetwork,
    accept_rx: Rc<RefCell<mpsc::UnboundedReceiver<ConnectRequest>>>,
}

impl TestTransport {
    pub fn peer_id(&self) -> &PeerId { &self.peer_id }
    pub fn addr(&self) -> &PeerAddr { &self.addr }
}

#[async_trait(?Send)]
impl Transport for TestTransport {
    type Connection = TestConnection;

    async fn connect(&self, addr: PeerAddr) -> Result<Self::Connection> {
        // Find the target's inbox AND identity.
        let (target_peer_id, inbox) = self
            .net
            .inboxes
            .borrow()
            .get(&addr)
            .cloned()
            .ok_or_else(|| Error::Transport(format!("no peer at {:?}", addr)))?;

        // Build the channel pair.
        let (tx_initiator_to_acceptor, rx_initiator_to_acceptor) =
            mpsc::unbounded_channel::<Bytes>();
        let (tx_acceptor_to_initiator, rx_acceptor_to_initiator) =
            mpsc::unbounded_channel::<Bytes>();
        let (ready_tx, ready_rx) = oneshot::channel::<()>();

        // Send the request to the acceptor side.
        inbox
            .send(ConnectRequest {
                from_peer: self.peer_id.clone(),
                // Acceptor uses tx_acceptor_to_initiator for its send;
                // rx_initiator_to_acceptor for its recv.
                tx_to_initiator: tx_acceptor_to_initiator,
                rx_from_initiator: rx_initiator_to_acceptor,
                ready: ready_tx,
            })
            .map_err(|_| Error::Transport("acceptor inbox closed".into()))?;

        // Wait for the acceptor to install its side.
        ready_rx
            .await
            .map_err(|_| Error::Transport("acceptor dropped without accepting".into()))?;

        // Initiator's connection: send via tx_initiator_to_acceptor, recv via rx_acceptor_to_initiator.
        // peer_id is the acceptor's identity (we looked it up above).
        Ok(TestConnection::new(
            target_peer_id,
            tx_initiator_to_acceptor,
            rx_acceptor_to_initiator,
        ))
    }

    async fn accept(&self) -> Result<Self::Connection> {
        let mut rx = self.accept_rx.borrow_mut();
        let req = rx
            .recv()
            .await
            .ok_or_else(|| Error::Transport("transport closed".into()))?;
        // Install our side and signal ready.
        let _ = req.ready.send(());
        Ok(TestConnection::new(
            req.from_peer,
            req.tx_to_initiator,
            req.rx_from_initiator,
        ))
    }
}

pub struct TestConnection {
    peer_id: PeerId,
    tx: mpsc::UnboundedSender<Bytes>,
    rx: RefCell<mpsc::UnboundedReceiver<Bytes>>,
}

impl TestConnection {
    fn new(
        peer_id: PeerId,
        tx: mpsc::UnboundedSender<Bytes>,
        rx: mpsc::UnboundedReceiver<Bytes>,
    ) -> Self {
        Self {
            peer_id,
            tx,
            rx: RefCell::new(rx),
        }
    }
}

#[async_trait(?Send)]
impl TransportConnection for TestConnection {
    async fn send_reliable(&self, bytes: Bytes) -> Result<()> {
        self.tx
            .send(bytes)
            .map_err(|_| Error::Transport("connection closed".into()))
    }

    async fn recv_reliable(&self) -> Result<Bytes> {
        self.rx
            .borrow_mut()
            .recv()
            .await
            .ok_or_else(|| Error::Transport("connection closed".into()))
    }

    async fn send_unreliable(&self, _bytes: Bytes) -> Result<()> {
        // No-op for v1; sunset-sync doesn't use the unreliable channel.
        Ok(())
    }

    async fn recv_unreliable(&self) -> Result<Bytes> {
        // Never resolves in v1 — sunset-sync doesn't read this.
        std::future::pending().await
    }

    fn peer_id(&self) -> PeerId {
        self.peer_id.clone()
    }

    async fn close(&self) -> Result<()> {
        // Drops on Drop; nothing to do explicitly. The next send/recv on
        // the other end will yield `connection closed`.
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use sunset_store::VerifyingKey;

    fn vk(b: &[u8]) -> VerifyingKey {
        VerifyingKey::new(Bytes::copy_from_slice(b))
    }

    #[tokio::test]
    async fn pair_can_send_and_recv() {
        let net = TestNetwork::new();
        let alice_addr = PeerAddr::new("alice");
        let bob_addr = PeerAddr::new("bob");
        let alice = net.transport(PeerId(vk(b"alice")), alice_addr.clone());
        let bob = net.transport(PeerId(vk(b"bob")), bob_addr.clone());

        let bob_accept = tokio::task::spawn_local(async move {
            bob.accept().await.unwrap()
        });

        let alice_conn = alice.connect(bob_addr).await.unwrap();
        let bob_conn = bob_accept.await.unwrap();

        alice_conn
            .send_reliable(Bytes::from_static(b"hello"))
            .await
            .unwrap();
        let got = bob_conn.recv_reliable().await.unwrap();
        assert_eq!(got, Bytes::from_static(b"hello"));

        bob_conn
            .send_reliable(Bytes::from_static(b"world"))
            .await
            .unwrap();
        let got = alice_conn.recv_reliable().await.unwrap();
        assert_eq!(got, Bytes::from_static(b"world"));
    }

    #[tokio::test]
    async fn connect_to_unknown_addr_errors() {
        let net = TestNetwork::new();
        let alice = net.transport(PeerId(vk(b"alice")), PeerAddr::new("alice"));
        let err = alice.connect(PeerAddr::new("nobody")).await.unwrap_err();
        assert!(matches!(err, Error::Transport(_)));
    }
}
```

The `tokio::task::spawn_local` in the test requires a `LocalSet`. `#[tokio::test]` by default uses a multi-threaded runtime that doesn't support `spawn_local`. Switch to `#[tokio::test(flavor = "current_thread")]` if needed, OR wrap the test body in a `LocalSet`:

```rust
let local = tokio::task::LocalSet::new();
local.run_until(async move { ... }).await;
```

Pick whichever is cleaner. (`flavor = "current_thread"` is the simplest; it makes `spawn_local` work without an explicit LocalSet.)

- [ ] **Step 2: Run tests**

Run: `nix develop --command cargo test -p sunset-sync --features test-helpers test_transport::`
Expected: 2 PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/sunset-sync/src/test_transport.rs
git commit -m "Add in-memory TestNetwork + TestTransport for sync integration tests"
```

---

### Task 6: Wire-protocol messages

**Files:**
- Modify: `crates/sunset-sync/src/message.rs`
- Modify: `crates/sunset-sync/src/reserved.rs` (extend with full reserved set + tests)

- [ ] **Step 1: Extend `reserved.rs`**

```rust
//! Reserved name constants for sunset-sync metadata.
//!
//! These name prefixes are reserved by *convention*. Application-layer code
//! (sunset-core, downstream consumers) does not write under these names,
//! so sunset-sync's interpretation of those entries isn't ambiguous. The
//! convention isn't enforced by the store — the store just verifies
//! signatures, and a peer with a valid signing key could in principle sign
//! an entry under any name. Defense against deliberately hostile values is
//! a separate concern handled by the trust filter.

/// Subscription filter entries — `(local_pubkey, "_sunset-sync/subscribe")`
/// stores a postcard-encoded `Filter` describing what events the peer wants.
pub const SUBSCRIBE_NAME: &[u8] = b"_sunset-sync/subscribe";

/// Optional liveness/health summaries (not used in v1).
#[allow(dead_code)]
pub const PEER_HEALTH_NAME: &[u8] = b"_sunset-sync/peer-health";

/// True if `name` is reserved for sunset-sync internal use.
pub fn is_reserved(name: &[u8]) -> bool {
    name.starts_with(b"_sunset-sync/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subscribe_name_is_reserved() {
        assert!(is_reserved(SUBSCRIBE_NAME));
    }

    #[test]
    fn application_names_are_not_reserved() {
        assert!(!is_reserved(b"chat/room/123"));
        assert!(!is_reserved(b"identity/alice"));
    }
}
```

- [ ] **Step 2: Write `message.rs`**

```rust
//! Wire-protocol messages exchanged between sunset-sync peers.
//!
//! All messages are postcard-encoded. The transport carries one whole
//! `SyncMessage` per `recv_reliable` / `send_reliable` call.

use bytes::Bytes;
use serde::{Deserialize, Serialize};

use sunset_store::{ContentBlock, Filter, Hash, SignedKvEntry, VerifyingKey};

use crate::error::{Error, Result};
use crate::types::PeerId;

/// A digest range for `DigestExchange`. v1 supports only `All` — the digest
/// covers every entry matching the filter, no partitioning. Future variants
/// (hash-prefix buckets, sequence-number ranges, hybrid) can be added
/// without breaking older peers because postcard tolerates new enum
/// variants on read by erroring at decode time, which the receiver maps to
/// `Error::Decode("unknown DigestRange")`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DigestRange {
    All,
}

/// Wire message types. Notably absent: `SubscribeRequest` — subscriptions
/// are KV entries that propagate via `EventDelivery` like any other event.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SyncMessage {
    Hello {
        protocol_version: u32,
        peer_id: PeerId,
    },
    EventDelivery {
        entries: Vec<SignedKvEntry>,
        blobs: Vec<ContentBlock>,
    },
    BlobRequest {
        hash: Hash,
    },
    BlobResponse {
        block: ContentBlock,
    },
    DigestExchange {
        filter: Filter,
        range: DigestRange,
        bloom: Bytes,
    },
    Fetch {
        entries: Vec<(VerifyingKey, Bytes)>,
    },
    Goodbye {},
}

impl SyncMessage {
    pub fn encode(&self) -> Result<Bytes> {
        postcard::to_stdvec(self)
            .map(Bytes::from)
            .map_err(|e| Error::Decode(format!("encode: {e}")))
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        postcard::from_bytes(bytes).map_err(|e| Error::Decode(format!("decode: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use sunset_store::VerifyingKey;

    fn vk(b: &[u8]) -> VerifyingKey {
        VerifyingKey::new(Bytes::copy_from_slice(b))
    }

    #[test]
    fn hello_postcard_roundtrip() {
        let m = SyncMessage::Hello {
            protocol_version: 1,
            peer_id: PeerId(vk(b"alice")),
        };
        let encoded = m.encode().unwrap();
        let decoded = SyncMessage::decode(&encoded).unwrap();
        assert_eq!(m, decoded);
    }

    #[test]
    fn digest_exchange_postcard_roundtrip() {
        let m = SyncMessage::DigestExchange {
            filter: Filter::Keyspace(vk(b"alice")),
            range: DigestRange::All,
            bloom: Bytes::from_static(&[0xff; 32]),
        };
        let encoded = m.encode().unwrap();
        let decoded = SyncMessage::decode(&encoded).unwrap();
        assert_eq!(m, decoded);
    }

    #[test]
    fn goodbye_postcard_roundtrip() {
        let m = SyncMessage::Goodbye {};
        let encoded = m.encode().unwrap();
        assert_eq!(SyncMessage::decode(&encoded).unwrap(), m);
    }

    #[test]
    fn decode_garbage_returns_decode_error() {
        let err = SyncMessage::decode(&[0xff, 0xff, 0xff, 0xff]).unwrap_err();
        assert!(matches!(err, Error::Decode(_)));
    }
}
```

- [ ] **Step 3: Run tests**

Run: `nix develop --command cargo test -p sunset-sync message:: reserved::`
Expected: 6 PASS (4 message + 2 reserved).

- [ ] **Step 4: Commit**

```bash
git add crates/sunset-sync/src/message.rs crates/sunset-sync/src/reserved.rs
git commit -m "Add SyncMessage wire enum, DigestRange, reserved name constants"
```

---

### Task 7: Bloom filter

**Files:**
- Modify: `crates/sunset-sync/src/digest.rs`

- [ ] **Step 1: Write `digest.rs`** (bloom filter only; the digest-round state machine comes in Task 14)

```rust
//! Bloom filter for `DigestExchange` and the digest-round state machine.

use bytes::Bytes;

/// A simple bloom filter backed by a fixed-size byte vector.
///
/// `num_bits` MUST be a multiple of 8 (the byte vector's length is
/// `num_bits / 8`). `num_hashes` controls the false-positive rate. v1 uses
/// fixed defaults from `SyncConfig` (4096 bits, 4 hashes).
#[derive(Clone, Debug)]
pub struct BloomFilter {
    bits: Vec<u8>,
    num_bits: usize,
    num_hashes: u32,
}

impl BloomFilter {
    pub fn new(num_bits: usize, num_hashes: u32) -> Self {
        debug_assert!(
            num_bits % 8 == 0 && num_bits > 0,
            "num_bits must be a positive multiple of 8"
        );
        Self {
            bits: vec![0u8; num_bits / 8],
            num_bits,
            num_hashes,
        }
    }

    pub fn from_bytes(bytes: Bytes, num_hashes: u32) -> Self {
        let num_bits = bytes.len() * 8;
        Self {
            bits: bytes.to_vec(),
            num_bits,
            num_hashes,
        }
    }

    pub fn to_bytes(&self) -> Bytes {
        Bytes::copy_from_slice(&self.bits)
    }

    pub fn num_bits(&self) -> usize { self.num_bits }
    pub fn num_hashes(&self) -> u32 { self.num_hashes }

    pub fn insert(&mut self, item: &[u8]) {
        for h in 0..self.num_hashes {
            let bit = self.bit_index(item, h);
            let (byte, mask) = (bit / 8, 1u8 << (bit % 8));
            self.bits[byte] |= mask;
        }
    }

    pub fn contains(&self, item: &[u8]) -> bool {
        for h in 0..self.num_hashes {
            let bit = self.bit_index(item, h);
            let (byte, mask) = (bit / 8, 1u8 << (bit % 8));
            if self.bits[byte] & mask == 0 {
                return false;
            }
        }
        true
    }

    /// Bit index for the `h`th hash of `item`. Uses blake3 with the hash
    /// index as a 4-byte salt prefix.
    fn bit_index(&self, item: &[u8], h: u32) -> usize {
        let mut hasher = blake3::Hasher::new();
        hasher.update(&h.to_le_bytes());
        hasher.update(item);
        let digest = hasher.finalize();
        let bytes = digest.as_bytes();
        // Take the first 8 bytes of the hash, modulo num_bits.
        let mut idx = [0u8; 8];
        idx.copy_from_slice(&bytes[..8]);
        (u64::from_le_bytes(idx) as usize) % self.num_bits
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_then_contains() {
        let mut b = BloomFilter::new(4096, 4);
        b.insert(b"alice");
        assert!(b.contains(b"alice"));
    }

    #[test]
    fn contains_false_for_unset() {
        let b = BloomFilter::new(4096, 4);
        assert!(!b.contains(b"alice"));
    }

    #[test]
    fn bytes_roundtrip() {
        let mut b = BloomFilter::new(4096, 4);
        b.insert(b"alice");
        b.insert(b"bob");
        let bytes = b.to_bytes();
        let b2 = BloomFilter::from_bytes(bytes, 4);
        assert!(b2.contains(b"alice"));
        assert!(b2.contains(b"bob"));
        assert!(!b2.contains(b"carol"));
    }

    #[test]
    fn empty_filter_contains_nothing() {
        let b = BloomFilter::new(4096, 4);
        for item in [b"a".as_ref(), b"b".as_ref(), b"c".as_ref()] {
            assert!(!b.contains(item));
        }
    }
}
```

- [ ] **Step 2: Run tests**

Run: `nix develop --command cargo test -p sunset-sync digest::`
Expected: 4 PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/sunset-sync/src/digest.rs
git commit -m "Add BloomFilter with blake3-keyed hashing"
```

---

### Task 8: Subscription registry

**Files:**
- Modify: `crates/sunset-sync/src/subscription_registry.rs`

The registry maps `PeerId -> Filter`, populated by parsing entries in the local store under `Filter::Namespace("_sunset-sync/subscribe")`. It is owned by the `SyncEngine` and updated whenever a relevant store event arrives.

- [ ] **Step 1: Write `subscription_registry.rs`**

```rust
//! In-memory tracker mapping `PeerId -> Filter`, built from KV entries
//! under `_sunset-sync/subscribe`.

use std::collections::HashMap;

use sunset_store::{Filter, SignedKvEntry, VerifyingKey};

use crate::error::Error;
use crate::types::PeerId;

#[derive(Default, Debug)]
pub struct SubscriptionRegistry {
    /// Peer's verifying key → declared filter. The peer's PeerId is
    /// `PeerId(verifying_key)` so the map is effectively
    /// `PeerId -> Filter`.
    by_peer: HashMap<VerifyingKey, Filter>,
}

impl SubscriptionRegistry {
    pub fn new() -> Self { Self::default() }

    /// Replace the filter for `vk` with `filter`. (LWW happens at the
    /// store layer; the registry just reflects whatever currently lives
    /// at `(vk, _sunset-sync/subscribe)`.)
    pub fn insert(&mut self, vk: VerifyingKey, filter: Filter) {
        self.by_peer.insert(vk, filter);
    }

    /// Remove `vk`'s registration (e.g., on TTL expiration).
    pub fn remove(&mut self, vk: &VerifyingKey) {
        self.by_peer.remove(vk);
    }

    /// All currently-registered peer filters.
    pub fn iter(&self) -> impl Iterator<Item = (&VerifyingKey, &Filter)> {
        self.by_peer.iter()
    }

    /// All `PeerId`s whose filter matches the given `(vk, name)`.
    pub fn peers_matching<'a>(
        &'a self,
        vk: &'a VerifyingKey,
        name: &'a [u8],
    ) -> impl Iterator<Item = PeerId> + 'a {
        self.by_peer.iter().filter_map(move |(peer_vk, filter)| {
            if filter.matches(vk, name) {
                Some(PeerId(peer_vk.clone()))
            } else {
                None
            }
        })
    }

    /// Union of all currently-registered filters. Returns `None` if no
    /// peers are registered. Used by the engine to subscribe to the local
    /// store with a single filter that covers all peer interests.
    pub fn union_filter(&self) -> Option<Filter> {
        if self.by_peer.is_empty() {
            None
        } else {
            Some(Filter::Union(
                self.by_peer.values().cloned().collect(),
            ))
        }
    }

    pub fn len(&self) -> usize { self.by_peer.len() }
    pub fn is_empty(&self) -> bool { self.by_peer.is_empty() }
}

/// Decode a `SignedKvEntry` whose value is supposed to be a postcard-encoded
/// `Filter`, given the corresponding `ContentBlock`. Returns `Error::Decode`
/// on parse failure.
pub fn parse_subscription_entry(
    entry: &SignedKvEntry,
    block: &sunset_store::ContentBlock,
) -> std::result::Result<Filter, Error> {
    if entry.value_hash != block.hash() {
        return Err(Error::Protocol(
            "subscription entry value_hash does not match supplied ContentBlock".into(),
        ));
    }
    postcard::from_bytes(&block.data)
        .map_err(|e| Error::Decode(format!("subscription filter: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use sunset_store::{ContentBlock, Filter, VerifyingKey};

    fn vk(b: &[u8]) -> VerifyingKey {
        VerifyingKey::new(Bytes::copy_from_slice(b))
    }

    #[test]
    fn insert_and_lookup() {
        let mut r = SubscriptionRegistry::new();
        r.insert(vk(b"alice"), Filter::Keyspace(vk(b"chat-1")));
        let peers: Vec<_> = r.peers_matching(&vk(b"chat-1"), b"k").collect();
        assert_eq!(peers, vec![PeerId(vk(b"alice"))]);
    }

    #[test]
    fn no_match_returns_empty() {
        let mut r = SubscriptionRegistry::new();
        r.insert(vk(b"alice"), Filter::Keyspace(vk(b"chat-1")));
        let peers: Vec<_> = r.peers_matching(&vk(b"chat-2"), b"k").collect();
        assert!(peers.is_empty());
    }

    #[test]
    fn union_filter_combines_all_peers() {
        let mut r = SubscriptionRegistry::new();
        r.insert(vk(b"alice"), Filter::Keyspace(vk(b"chat-1")));
        r.insert(vk(b"bob"), Filter::Keyspace(vk(b"chat-2")));
        let union = r.union_filter().unwrap();
        assert!(union.matches(&vk(b"chat-1"), b"k"));
        assert!(union.matches(&vk(b"chat-2"), b"k"));
        assert!(!union.matches(&vk(b"chat-3"), b"k"));
    }

    #[test]
    fn union_empty_returns_none() {
        let r = SubscriptionRegistry::new();
        assert!(r.union_filter().is_none());
    }

    #[test]
    fn parse_subscription_decodes_filter() {
        let filter = Filter::Keyspace(vk(b"chat-1"));
        let bytes = postcard::to_stdvec(&filter).unwrap();
        let block = ContentBlock {
            data: Bytes::from(bytes),
            references: vec![],
        };
        let entry = SignedKvEntry {
            verifying_key: vk(b"alice"),
            name: Bytes::from_static(b"_sunset-sync/subscribe"),
            value_hash: block.hash(),
            priority: 1,
            expires_at: None,
            signature: Bytes::from_static(b"sig"),
        };
        let parsed = parse_subscription_entry(&entry, &block).unwrap();
        assert_eq!(parsed, filter);
    }

    #[test]
    fn parse_subscription_rejects_wrong_block() {
        let filter = Filter::Keyspace(vk(b"chat-1"));
        let bytes = postcard::to_stdvec(&filter).unwrap();
        let block_a = ContentBlock { data: Bytes::from(bytes), references: vec![] };
        let block_b = ContentBlock { data: Bytes::from_static(b"different"), references: vec![] };
        let entry = SignedKvEntry {
            verifying_key: vk(b"alice"),
            name: Bytes::from_static(b"_sunset-sync/subscribe"),
            value_hash: block_a.hash(),
            priority: 1,
            expires_at: None,
            signature: Bytes::from_static(b"sig"),
        };
        let err = parse_subscription_entry(&entry, &block_b).unwrap_err();
        assert!(matches!(err, Error::Protocol(_)));
    }
}
```

- [ ] **Step 2: Run tests**

Run: `nix develop --command cargo test -p sunset-sync subscription_registry::`
Expected: 6 PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/sunset-sync/src/subscription_registry.rs
git commit -m "Add SubscriptionRegistry mapping PeerId -> Filter"
```

---

### Task 9: SyncEngine skeleton

**Files:**
- Modify: `crates/sunset-sync/src/engine.rs`

Build the public-API surface and the internal command channel without yet running the engine loop. `run()` is a stub that returns immediately with `Ok(())`.

- [ ] **Step 1: Write `engine.rs`**

```rust
//! `SyncEngine` — the top-level coordinator.

use std::collections::HashMap;
use std::sync::Arc;

use bytes::Bytes;
use sunset_store::{Filter, Store, VerifyingKey};
use tokio::sync::{Mutex, mpsc, oneshot};

use crate::error::{Error, Result};
use crate::reserved;
use crate::subscription_registry::SubscriptionRegistry;
use crate::transport::Transport;
use crate::types::{PeerAddr, PeerId, SyncConfig, TrustSet};

/// A command sent from the public API into the running engine.
pub(crate) enum EngineCommand {
    AddPeer {
        addr: PeerAddr,
        ack: oneshot::Sender<Result<()>>,
    },
    PublishSubscription {
        filter: Filter,
        ttl: std::time::Duration,
        ack: oneshot::Sender<Result<()>>,
    },
    SetTrust {
        trust: TrustSet,
        ack: oneshot::Sender<Result<()>>,
    },
}

/// Mutable state inside the engine. Held under a `tokio::sync::Mutex` so
/// command processing and per-peer task callbacks can both update it.
pub(crate) struct EngineState {
    pub trust: TrustSet,
    pub registry: SubscriptionRegistry,
    /// Per-peer outbound message senders.
    pub peer_outbound: HashMap<PeerId, mpsc::UnboundedSender<crate::message::SyncMessage>>,
}

pub struct SyncEngine<S: Store, T: Transport> {
    store: Arc<S>,
    transport: Arc<T>,
    config: SyncConfig,
    /// Local peer's identity. Required for signing `_sunset-sync/subscribe`
    /// entries.
    local_peer: PeerId,
    state: Arc<Mutex<EngineState>>,
    cmd_tx: mpsc::UnboundedSender<EngineCommand>,
    /// Held inside `run()`. `new()` creates the (tx, rx) pair; `run()`
    /// takes the rx out via Mutex<Option<...>>.
    cmd_rx: Arc<Mutex<Option<mpsc::UnboundedReceiver<EngineCommand>>>>,
}

impl<S: Store + 'static, T: Transport + 'static> SyncEngine<S, T> {
    pub fn new(
        store: Arc<S>,
        transport: T,
        config: SyncConfig,
        local_peer: PeerId,
    ) -> Self {
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        Self {
            store,
            transport: Arc::new(transport),
            config,
            local_peer,
            state: Arc::new(Mutex::new(EngineState {
                trust: TrustSet::default(),
                registry: SubscriptionRegistry::new(),
                peer_outbound: HashMap::new(),
            })),
            cmd_tx,
            cmd_rx: Arc::new(Mutex::new(Some(cmd_rx))),
        }
    }

    /// Initiate an outbound connection to `addr`. Returns when the connection
    /// is established + Hello-exchanged, or fails.
    pub async fn add_peer(&self, addr: PeerAddr) -> Result<()> {
        let (ack, rx) = oneshot::channel();
        self.cmd_tx
            .send(EngineCommand::AddPeer { addr, ack })
            .map_err(|_| Error::Closed)?;
        rx.await.map_err(|_| Error::Closed)?
    }

    /// Publish this peer's subscription filter. Writes a signed KV entry
    /// under `(local_peer, "_sunset-sync/subscribe")` with `value_hash =
    /// blake3(postcard(filter))` and priority = unix-timestamp-now,
    /// expires_at = priority + ttl.
    ///
    /// **Note:** v1 uses a stub signature (empty bytes) — the
    /// `sunset_store::AcceptAllVerifier` accepts everything. When a real
    /// signing scheme lands (sunset-core / identity subsystem), this
    /// function will sign the entry with the local key.
    pub async fn publish_subscription(
        &self,
        filter: Filter,
        ttl: std::time::Duration,
    ) -> Result<()> {
        let (ack, rx) = oneshot::channel();
        self.cmd_tx
            .send(EngineCommand::PublishSubscription { filter, ttl, ack })
            .map_err(|_| Error::Closed)?;
        rx.await.map_err(|_| Error::Closed)?
    }

    /// Replace the trust set. Subsequent inbound events are filtered
    /// against the new set.
    pub async fn set_trust(&self, trust: TrustSet) -> Result<()> {
        let (ack, rx) = oneshot::channel();
        self.cmd_tx
            .send(EngineCommand::SetTrust { trust, ack })
            .map_err(|_| Error::Closed)?;
        rx.await.map_err(|_| Error::Closed)?
    }

    /// Run the engine until it's closed. This is a long-running future
    /// that drives the `select!` loop, per-peer tasks (via `spawn_local`),
    /// and the anti-entropy timer.
    ///
    /// Caller must invoke this inside a `LocalSet` (native) or directly on
    /// a single-threaded executor (WASM).
    pub async fn run(&self) -> Result<()> {
        // Take ownership of the command receiver. If `run()` is called
        // twice, the second call observes None and returns Error::Closed.
        let mut cmd_rx = self
            .cmd_rx
            .lock()
            .await
            .take()
            .ok_or(Error::Closed)?;

        // Tasks 11–15 fill in the loop body. For now, drain commands until
        // the channel closes.
        while let Some(cmd) = cmd_rx.recv().await {
            self.handle_command(cmd).await;
        }
        Ok(())
    }

    /// Stub command handler — replaced fully in Tasks 11–15.
    pub(crate) async fn handle_command(&self, cmd: EngineCommand) {
        match cmd {
            EngineCommand::AddPeer { ack, .. } => {
                let _ = ack.send(Err(Error::Protocol(
                    "add_peer not implemented (Task 13)".into(),
                )));
            }
            EngineCommand::PublishSubscription { filter, ttl, ack } => {
                let r = self.do_publish_subscription(filter, ttl).await;
                let _ = ack.send(r);
            }
            EngineCommand::SetTrust { trust, ack } => {
                self.state.lock().await.trust = trust;
                let _ = ack.send(Ok(()));
            }
        }
    }

    /// Real implementation of `publish_subscription`'s server side.
    async fn do_publish_subscription(
        &self,
        filter: Filter,
        ttl: std::time::Duration,
    ) -> Result<()> {
        use sunset_store::{ContentBlock, SignedKvEntry};

        let value = postcard::to_stdvec(&filter)
            .map_err(|e| Error::Decode(format!("encode filter: {e}")))?;
        let block = ContentBlock {
            data: Bytes::from(value),
            references: vec![],
        };
        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let entry = SignedKvEntry {
            verifying_key: self.local_peer.0.clone(),
            name: Bytes::from_static(reserved::SUBSCRIBE_NAME),
            value_hash: block.hash(),
            priority: now_secs,
            expires_at: Some(now_secs.saturating_add(ttl.as_secs())),
            // v1 stub signature; real signing lands in identity subsystem.
            signature: Bytes::new(),
        };
        self.store.insert(entry, Some(block)).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use sunset_store::{Filter, VerifyingKey};
    use sunset_store_memory::MemoryStore;

    use crate::test_transport::{TestNetwork, TestTransport};

    fn vk(b: &[u8]) -> VerifyingKey { VerifyingKey::new(Bytes::copy_from_slice(b)) }

    fn make_engine(
        addr: &str,
        peer_label: &[u8],
    ) -> SyncEngine<MemoryStore, TestTransport> {
        let net = TestNetwork::new();
        let local_peer = PeerId(vk(peer_label));
        let transport = net.transport(local_peer.clone(), PeerAddr::new(addr));
        let store = Arc::new(MemoryStore::new());
        SyncEngine::new(store, transport, SyncConfig::default(), local_peer)
    }

    #[tokio::test(flavor = "current_thread")]
    async fn set_trust_updates_state() {
        let engine = make_engine("alice", b"alice");
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let h = tokio::task::spawn_local({
                    let cmd_rx = engine.cmd_rx.clone();
                    let state = engine.state.clone();
                    async move {
                        let mut rx = cmd_rx.lock().await.take().unwrap();
                        while let Some(cmd) = rx.recv().await {
                            if let EngineCommand::SetTrust { trust, ack } = cmd {
                                state.lock().await.trust = trust;
                                let _ = ack.send(Ok(()));
                            }
                        }
                    }
                });
                let mut whitelist = std::collections::HashSet::new();
                whitelist.insert(vk(b"trusted"));
                engine.set_trust(TrustSet::Whitelist(whitelist.clone())).await.unwrap();
                let s = engine.state.lock().await;
                assert_eq!(s.trust, TrustSet::Whitelist(whitelist));
                drop(s);
                drop(engine.cmd_tx.clone());
                let _ = h.await;
            })
            .await;
    }
}
```

The test uses a hand-rolled command-handler loop because the real `run()` doesn't yet exist. Task 11 will replace this with a real run(). That makes the test fragile — but it's the right shape for a skeleton task.

- [ ] **Step 2: Run the test**

Run: `nix develop --command cargo test -p sunset-sync --features test-helpers engine::`
Expected: 1 PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/sunset-sync/src/engine.rs
git commit -m "Add SyncEngine skeleton with command channel and stub run loop"
```

---

### Task 10: Per-peer connection task

**Files:**
- Modify: `crates/sunset-sync/src/peer.rs`

The per-peer task owns one `TransportConnection` and runs concurrent send + receive loops. Inbound `SyncMessage`s are forwarded to the engine via an `mpsc::UnboundedSender<InboundEvent>`. Outbound messages are pulled from a per-peer mpsc.

- [ ] **Step 1: Write `peer.rs`**

```rust
//! Per-peer connection task.

use std::rc::Rc;

use bytes::Bytes;
use tokio::sync::mpsc;

use crate::error::{Error, Result};
use crate::message::SyncMessage;
use crate::transport::TransportConnection;
use crate::types::PeerId;

/// An event emitted by a per-peer task to the engine.
pub(crate) enum InboundEvent {
    /// Hello received; the peer's identity is now known.
    PeerHello {
        peer_id: PeerId,
        protocol_version: u32,
    },
    /// A SyncMessage arrived (other than Hello).
    Message {
        from: PeerId,
        message: SyncMessage,
    },
    /// The peer's connection closed (graceful or error).
    Disconnected {
        peer_id: PeerId,
        reason: String,
    },
}

/// Drive a single peer's connection.
///
/// Sends our `Hello`, waits for the peer's `Hello`, then runs concurrent
/// recv + send loops until the connection drops.
///
/// `outbound_rx` is the receiver half of the per-peer outbound channel —
/// the engine sends `SyncMessage`s into this channel and they are written
/// to the transport here.
///
/// `inbound_tx` is the shared sender into the engine's main inbound queue.
///
/// `local_protocol_version` is `SyncConfig::protocol_version`.
/// `local_peer` is the engine's local PeerId.
pub(crate) async fn run_peer<C: TransportConnection + 'static>(
    conn: Rc<C>,
    local_peer: PeerId,
    local_protocol_version: u32,
    mut outbound_rx: mpsc::UnboundedReceiver<SyncMessage>,
    inbound_tx: mpsc::UnboundedSender<InboundEvent>,
) {
    // Send our Hello.
    let our_hello = SyncMessage::Hello {
        protocol_version: local_protocol_version,
        peer_id: local_peer.clone(),
    };
    if let Err(e) = send_message(&conn, &our_hello).await {
        let _ = inbound_tx.send(InboundEvent::Disconnected {
            peer_id: conn.peer_id(),
            reason: format!("send hello: {e}"),
        });
        return;
    }

    // Receive the peer's Hello.
    let peer_id = match recv_message(&conn).await {
        Ok(SyncMessage::Hello { protocol_version, peer_id }) => {
            if protocol_version != local_protocol_version {
                let _ = inbound_tx.send(InboundEvent::Disconnected {
                    peer_id,
                    reason: format!(
                        "protocol version mismatch: ours {} theirs {}",
                        local_protocol_version, protocol_version
                    ),
                });
                return;
            }
            let _ = inbound_tx.send(InboundEvent::PeerHello {
                peer_id: peer_id.clone(),
                protocol_version,
            });
            peer_id
        }
        Ok(other) => {
            let _ = inbound_tx.send(InboundEvent::Disconnected {
                peer_id: conn.peer_id(),
                reason: format!("expected Hello, got {:?}", other),
            });
            return;
        }
        Err(e) => {
            let _ = inbound_tx.send(InboundEvent::Disconnected {
                peer_id: conn.peer_id(),
                reason: format!("recv hello: {e}"),
            });
            return;
        }
    };

    // Concurrent recv + send loops.
    let recv_task = {
        let conn = conn.clone();
        let inbound_tx = inbound_tx.clone();
        let peer_id = peer_id.clone();
        async move {
            loop {
                match recv_message(&conn).await {
                    Ok(SyncMessage::Goodbye {}) => {
                        let _ = inbound_tx.send(InboundEvent::Disconnected {
                            peer_id: peer_id.clone(),
                            reason: "peer goodbye".into(),
                        });
                        break;
                    }
                    Ok(message) => {
                        if inbound_tx
                            .send(InboundEvent::Message {
                                from: peer_id.clone(),
                                message,
                            })
                            .is_err()
                        {
                            break;
                        }
                    }
                    Err(e) => {
                        let _ = inbound_tx.send(InboundEvent::Disconnected {
                            peer_id: peer_id.clone(),
                            reason: format!("recv: {e}"),
                        });
                        break;
                    }
                }
            }
        }
    };

    let send_task = {
        let conn = conn.clone();
        async move {
            while let Some(msg) = outbound_rx.recv().await {
                if let Err(_) = send_message(&conn, &msg).await {
                    break;
                }
            }
            // Channel closed — send Goodbye and close.
            let _ = send_message(&conn, &SyncMessage::Goodbye {}).await;
            let _ = conn.close().await;
        }
    };

    tokio::join!(recv_task, send_task);
}

async fn send_message<C: TransportConnection + ?Sized>(
    conn: &C,
    msg: &SyncMessage,
) -> Result<()> {
    let bytes = msg.encode()?;
    conn.send_reliable(bytes).await
}

async fn recv_message<C: TransportConnection + ?Sized>(conn: &C) -> Result<SyncMessage> {
    let bytes: Bytes = conn.recv_reliable().await?;
    SyncMessage::decode(&bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_transport::{TestNetwork, TestTransport};
    use crate::transport::Transport;
    use bytes::Bytes;
    use sunset_store::VerifyingKey;

    fn vk(b: &[u8]) -> VerifyingKey { VerifyingKey::new(Bytes::copy_from_slice(b)) }

    #[tokio::test(flavor = "current_thread")]
    async fn hello_exchange_succeeds() {
        let local = tokio::task::LocalSet::new();
        local.run_until(async {
            let net = TestNetwork::new();
            let alice = net.transport(PeerId(vk(b"alice")), PeerAddr_str("alice"));
            let bob = net.transport(PeerId(vk(b"bob")), PeerAddr_str("bob"));
            let bob_accept = tokio::task::spawn_local(async move {
                bob.accept().await.unwrap()
            });
            let alice_conn = alice.connect(PeerAddr_str("bob")).await.unwrap();
            let bob_conn = bob_accept.await.unwrap();

            let (a_out_tx, a_out_rx) = mpsc::unbounded_channel::<SyncMessage>();
            let (b_out_tx, b_out_rx) = mpsc::unbounded_channel::<SyncMessage>();
            let (a_in_tx, mut a_in_rx) = mpsc::unbounded_channel::<InboundEvent>();
            let (b_in_tx, mut b_in_rx) = mpsc::unbounded_channel::<InboundEvent>();

            tokio::task::spawn_local(run_peer(Rc::new(alice_conn), PeerId(vk(b"alice")), 1, a_out_rx, a_in_tx));
            tokio::task::spawn_local(run_peer(Rc::new(bob_conn), PeerId(vk(b"bob")), 1, b_out_rx, b_in_tx));

            // Each side observes the other's Hello.
            match a_in_rx.recv().await.unwrap() {
                InboundEvent::PeerHello { peer_id, protocol_version } => {
                    assert_eq!(peer_id, PeerId(vk(b"bob")));
                    assert_eq!(protocol_version, 1);
                }
                other => panic!("expected Hello, got {:?}", InboundDebug(&other)),
            }
            match b_in_rx.recv().await.unwrap() {
                InboundEvent::PeerHello { peer_id, protocol_version } => {
                    assert_eq!(peer_id, PeerId(vk(b"alice")));
                    assert_eq!(protocol_version, 1);
                }
                other => panic!("expected Hello, got {:?}", InboundDebug(&other)),
            }

            // Drop outbound senders → both peers send Goodbye and exit.
            drop(a_out_tx);
            drop(b_out_tx);
        }).await;
    }

    fn PeerAddr_str(s: &str) -> crate::types::PeerAddr {
        crate::types::PeerAddr::new(s)
    }

    /// `InboundEvent` doesn't derive Debug; this wrapper produces a brief description
    /// so panic messages are useful.
    struct InboundDebug<'a>(&'a InboundEvent);
    impl<'a> std::fmt::Debug for InboundDebug<'a> {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self.0 {
                InboundEvent::PeerHello { .. } => write!(f, "PeerHello"),
                InboundEvent::Message { .. } => write!(f, "Message"),
                InboundEvent::Disconnected { reason, .. } => write!(f, "Disconnected({})", reason),
            }
        }
    }
}
```

(If the `InboundDebug` shim feels heavy, just `#[derive(Debug)]` on `InboundEvent` itself — `SyncMessage` already derives Debug, and PeerId derives Debug.)

- [ ] **Step 2: Run tests**

Run: `nix develop --command cargo test -p sunset-sync --features test-helpers peer::`
Expected: 1 PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/sunset-sync/src/peer.rs
git commit -m "Add per-peer connection task with Hello exchange and bidirectional message dispatch"
```

---

### Task 11: Engine event loop + push flow

**Files:**
- Modify: `crates/sunset-sync/src/engine.rs`

Replace the stub `run()` with a real event loop:
- Drives `transport.accept()` for inbound connections
- Drives `cmd_rx` for `add_peer`, `publish_subscription`, `set_trust` commands
- Drives a per-peer `inbound_rx` channel (single shared receiver) for messages from per-peer tasks
- Drives a local store subscription that pushes matching events to the right peer

For Task 11 we wire **only the push flow**: when an entry arrives in the local store and matches a connected peer's filter, package it as `EventDelivery { entries: [entry], blobs: [] }` and send it to that peer's outbound channel. (Receiving + processing `EventDelivery` is Task 12.)

- [ ] **Step 1: Replace `engine.rs`'s `run()` and add helper methods**

Big diff. Key additions:

```rust
// In imports:
use std::time::Duration;
use sunset_store::{Event, Replay};
use crate::message::SyncMessage;
use crate::peer::{run_peer, InboundEvent};
use crate::transport::{Transport, TransportConnection};

// Replace `EngineState`'s peer_outbound type to use `mpsc::UnboundedSender<SyncMessage>`
// (already correct in Task 9). No change needed here.

// Replace SyncEngine::run with the real loop.
impl<S: Store + 'static, T: Transport + 'static> SyncEngine<S, T>
where
    T::Connection: 'static,
{
    pub async fn run(&self) -> Result<()> {
        let mut cmd_rx = self
            .cmd_rx
            .lock()
            .await
            .take()
            .ok_or(Error::Closed)?;

        // Channel for per-peer tasks to talk back to us.
        let (inbound_tx, mut inbound_rx) = mpsc::unbounded_channel::<InboundEvent>();

        // Local store subscription. Initially a `Filter::Namespace(_sunset-sync/subscribe)`;
        // refreshed whenever the registry changes (Task 14 expands the union).
        let mut local_sub = self
            .store
            .subscribe(
                Filter::Namespace(Bytes::from_static(reserved::SUBSCRIBE_NAME)),
                Replay::None,
            )
            .await?;

        loop {
            tokio::select! {
                // Inbound connection from the transport.
                maybe_conn = self.transport.accept() => {
                    match maybe_conn {
                        Ok(conn) => self.spawn_peer(conn, inbound_tx.clone()).await,
                        Err(e) => {
                            // Accept failure is usually fatal in v1.
                            return Err(e);
                        }
                    }
                }
                // Public-API command.
                Some(cmd) = cmd_rx.recv() => {
                    self.handle_command(cmd, &inbound_tx).await;
                }
                // Per-peer task event.
                Some(event) = inbound_rx.recv() => {
                    self.handle_inbound_event(event).await;
                }
                // Local store event — push flow.
                Some(item) = futures::StreamExt::next(&mut local_sub) => {
                    match item {
                        Ok(ev) => self.handle_local_store_event(ev).await,
                        Err(e) => return Err(Error::Store(e)),
                    }
                }
            }
        }
    }
}
```

(The actual `tokio::select!` body must be careful with mutable borrow rules — `local_sub` and `inbound_rx` are both `&mut`, and `cmd_rx` is `&mut`. They're all named locals so the select! macro handles it.)

Add the helper methods (full bodies):

```rust
impl<S: Store + 'static, T: Transport + 'static> SyncEngine<S, T>
where
    T::Connection: 'static,
{
    async fn spawn_peer(
        &self,
        conn: T::Connection,
        inbound_tx: mpsc::UnboundedSender<InboundEvent>,
    ) {
        let conn = std::rc::Rc::new(conn);
        let (out_tx, out_rx) = mpsc::unbounded_channel::<SyncMessage>();
        let local_peer = self.local_peer.clone();
        let proto = self.config.protocol_version;

        // Register the outbound sender under the connection's peer_id. For
        // the TestTransport this is the peer's actual identity (the network
        // tracks addr -> peer_id). For production transports that don't
        // surface peer_id until after a handshake, the connection should
        // either delay TransportConnection::peer_id() until it's authoritative
        // or accept a re-key on PeerHello — handle that when those transports
        // are added.
        let peer_id = conn.peer_id();
        self.state
            .lock()
            .await
            .peer_outbound
            .insert(peer_id, out_tx);

        tokio::task::spawn_local(run_peer(
            conn,
            local_peer,
            proto,
            out_rx,
            inbound_tx,
        ));
    }

    async fn handle_command(
        &self,
        cmd: EngineCommand,
        inbound_tx: &mpsc::UnboundedSender<InboundEvent>,
    ) {
        match cmd {
            EngineCommand::AddPeer { addr, ack } => {
                let r = self.do_add_peer(addr, inbound_tx.clone()).await;
                let _ = ack.send(r);
            }
            EngineCommand::PublishSubscription { filter, ttl, ack } => {
                let r = self.do_publish_subscription(filter, ttl).await;
                let _ = ack.send(r);
            }
            EngineCommand::SetTrust { trust, ack } => {
                self.state.lock().await.trust = trust;
                let _ = ack.send(Ok(()));
            }
        }
    }

    async fn do_add_peer(
        &self,
        addr: PeerAddr,
        inbound_tx: mpsc::UnboundedSender<InboundEvent>,
    ) -> Result<()> {
        let conn = self.transport.connect(addr).await?;
        self.spawn_peer(conn, inbound_tx).await;
        Ok(())
    }

    async fn handle_inbound_event(&self, event: InboundEvent) {
        match event {
            InboundEvent::PeerHello { .. } => {
                // The outbound channel was already registered under the
                // connection's peer_id in spawn_peer. PeerHello is just a
                // signal that the handshake completed; bootstrap fires from
                // here in Task 14.
            }
            InboundEvent::Message { from, message } => {
                self.handle_peer_message(from, message).await;
            }
            InboundEvent::Disconnected { peer_id, .. } => {
                self.state.lock().await.peer_outbound.remove(&peer_id);
            }
        }
    }

    async fn handle_peer_message(&self, from: PeerId, message: SyncMessage) {
        // Tasks 12–15 fill this in. For now, only Hello is a no-op (already
        // handled by handle_inbound_event), and other messages are ignored.
        let _ = (from, message);
    }

    async fn handle_local_store_event(&self, ev: Event) {
        // Push flow: route to peers whose filter matches.
        let entry = match ev {
            Event::Inserted(e) => e,
            Event::Replaced { new, .. } => new,
            // Expired / BlobAdded / BlobRemoved: not pushed in v1.
            _ => return,
        };
        // Look up the corresponding blob (best-effort).
        let blob = self
            .store
            .get_content(&entry.value_hash)
            .await
            .ok()
            .flatten();
        let msg = SyncMessage::EventDelivery {
            entries: vec![entry.clone()],
            blobs: blob.into_iter().collect(),
        };
        // Find matching peers and forward.
        let state = self.state.lock().await;
        for peer in state
            .registry
            .peers_matching(&entry.verifying_key, &entry.name)
        {
            if let Some(tx) = state.peer_outbound.get(&peer) {
                let _ = tx.send(msg.clone());
            }
        }
    }
}
```

(Subscriptions: in Task 11 we only subscribe to `Filter::Namespace(_sunset-sync/subscribe)`. Updating the registry on `_sunset-sync/subscribe` events arrives in Task 14 along with the union-filter resubscribe.)

- [ ] **Step 2: Add a smoke test**

```rust
// in engine.rs's tests mod:
#[tokio::test(flavor = "current_thread")]
async fn run_drains_set_trust_command() {
    let local = tokio::task::LocalSet::new();
    local.run_until(async {
        let engine = make_engine("alice", b"alice");
        let engine = Arc::new(engine);
        let h = tokio::task::spawn_local({
            let engine = engine.clone();
            async move { engine.run().await }
        });
        let mut wl = std::collections::HashSet::new();
        wl.insert(vk(b"trusted"));
        engine.set_trust(TrustSet::Whitelist(wl.clone())).await.unwrap();
        let s = engine.state.lock().await;
        assert_eq!(s.trust, TrustSet::Whitelist(wl));
        drop(s);
        // Drop cmd_tx to terminate run().
        // The engine holds the only cmd_tx; we can't drop it from outside.
        // Instead, abort the task.
        h.abort();
    }).await;
}
```

- [ ] **Step 3: Run tests**

Run: `nix develop --command cargo test -p sunset-sync --features test-helpers engine::`
Expected: existing tests pass + new test passes.

- [ ] **Step 4: Commit**

```bash
git add crates/sunset-sync/src/engine.rs
git commit -m "Wire SyncEngine event loop with accept, command, inbound, and local push paths"
```

---

### Task 12: Receive flow + trust filter

**Files:**
- Modify: `crates/sunset-sync/src/engine.rs`

Replace `handle_peer_message`'s no-op for `EventDelivery` with: trust-filter the entries, then `store.insert` them. Also handle `BlobRequest` / `BlobResponse` per Task 13.

For Task 12, only handle `EventDelivery` (and ignore others). Task 13 adds blob fetch.

- [ ] **Step 1: Implement EventDelivery handling**

```rust
async fn handle_peer_message(&self, from: PeerId, message: SyncMessage) {
    match message {
        SyncMessage::EventDelivery { entries, blobs } => {
            self.handle_event_delivery(from, entries, blobs).await;
        }
        // Tasks 13–15 handle BlobRequest, BlobResponse, DigestExchange, Fetch.
        _ => {}
    }
}

async fn handle_event_delivery(
    &self,
    _from: PeerId,
    entries: Vec<sunset_store::SignedKvEntry>,
    blobs: Vec<sunset_store::ContentBlock>,
) {
    // Trust filter.
    let trusted: Vec<_> = {
        let state = self.state.lock().await;
        entries
            .into_iter()
            .filter(|e| state.trust.contains(&e.verifying_key))
            .collect()
    };

    // Index blobs by hash so we can look up each entry's blob in O(1).
    use std::collections::HashMap;
    let blobs_by_hash: HashMap<_, _> = blobs
        .into_iter()
        .map(|b| (b.hash(), b))
        .collect();

    for entry in trusted {
        let blob = blobs_by_hash.get(&entry.value_hash).cloned();
        // We pass the blob if we have it; if not, the entry inserts as a
        // dangling ref and the engine will issue a BlobRequest later
        // (Task 13).
        match self.store.insert(entry.clone(), blob).await {
            Ok(()) => {
                // Successful insert. The store will fire an event on our
                // local subscription, which will trigger push flow to
                // other peers (transitive delivery).
            }
            Err(sunset_store::Error::Stale) => {
                // Already have a higher-priority version; drop silently.
            }
            Err(e) => {
                // Other errors are protocol violations or storage failures.
                // Log via stderr for v1; future work can route to a
                // structured-logging surface.
                eprintln!("sunset-sync: insert failed for entry from {:?}: {}", entry.verifying_key, e);
            }
        }
    }
}
```

- [ ] **Step 2: Add an integration-style test**

```rust
#[tokio::test(flavor = "current_thread")]
async fn event_delivery_inserts_trusted_entries() {
    use bytes::Bytes;
    use sunset_store::{ContentBlock, SignedKvEntry, Store as _};

    let local = tokio::task::LocalSet::new();
    local.run_until(async {
        let engine = Arc::new(make_engine("alice", b"alice"));

        // Build an entry from "trusted-writer" referencing a content block.
        let block = ContentBlock {
            data: Bytes::from_static(b"hello"),
            references: vec![],
        };
        let entry = SignedKvEntry {
            verifying_key: vk(b"trusted-writer"),
            name: Bytes::from_static(b"chat/k1"),
            value_hash: block.hash(),
            priority: 1,
            expires_at: None,
            signature: Bytes::new(),
        };

        // Default trust is All; deliver directly.
        engine
            .handle_event_delivery(
                PeerId(vk(b"some-peer")),
                vec![entry.clone()],
                vec![block],
            )
            .await;

        let stored = engine
            .store
            .get_entry(&vk(b"trusted-writer"), b"chat/k1")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored, entry);
    }).await;
}

#[tokio::test(flavor = "current_thread")]
async fn event_delivery_drops_untrusted_entries() {
    use bytes::Bytes;
    use sunset_store::{ContentBlock, SignedKvEntry, Store as _};

    let local = tokio::task::LocalSet::new();
    local.run_until(async {
        let engine = Arc::new(make_engine("alice", b"alice"));

        // Set trust to a specific whitelist.
        let mut wl = std::collections::HashSet::new();
        wl.insert(vk(b"trusted-writer"));
        engine.set_trust_direct(TrustSet::Whitelist(wl)).await;

        let block = ContentBlock { data: Bytes::from_static(b"x"), references: vec![] };
        let entry = SignedKvEntry {
            verifying_key: vk(b"untrusted-writer"),
            name: Bytes::from_static(b"chat/k1"),
            value_hash: block.hash(),
            priority: 1,
            expires_at: None,
            signature: Bytes::new(),
        };

        engine
            .handle_event_delivery(
                PeerId(vk(b"some-peer")),
                vec![entry],
                vec![block],
            )
            .await;

        let result = engine
            .store
            .get_entry(&vk(b"untrusted-writer"), b"chat/k1")
            .await
            .unwrap();
        assert!(result.is_none(), "untrusted entry should not be stored");
    }).await;
}

// Tiny helper for tests: bypass the command channel.
impl<S: Store + 'static, T: Transport + 'static> SyncEngine<S, T> {
    #[cfg(test)]
    pub(crate) async fn set_trust_direct(&self, trust: TrustSet) {
        self.state.lock().await.trust = trust;
    }
}
```

- [ ] **Step 3: Run tests**

Run: `nix develop --command cargo test -p sunset-sync --features test-helpers engine::`
Expected: previous tests still pass + 2 new tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/sunset-sync/src/engine.rs
git commit -m "Add EventDelivery handling: trust filter + store.insert"
```

---

### Task 13: Blob fetch (BlobRequest / BlobResponse)

**Files:**
- Modify: `crates/sunset-sync/src/engine.rs`

When `EventDelivery` arrives with an entry whose blob isn't supplied, request it via `BlobRequest`. When `BlobRequest` arrives, look up the blob locally and respond with `BlobResponse` (or drop if missing). When `BlobResponse` arrives, store it.

- [ ] **Step 1: Update `handle_peer_message`**

```rust
async fn handle_peer_message(&self, from: PeerId, message: SyncMessage) {
    match message {
        SyncMessage::EventDelivery { entries, blobs } => {
            self.handle_event_delivery(from, entries, blobs).await;
        }
        SyncMessage::BlobRequest { hash } => {
            self.handle_blob_request(from, hash).await;
        }
        SyncMessage::BlobResponse { block } => {
            self.handle_blob_response(block).await;
        }
        // Tasks 14–15 handle DigestExchange, Fetch.
        _ => {}
    }
}

async fn handle_blob_request(&self, from: PeerId, hash: sunset_store::Hash) {
    let block = match self.store.get_content(&hash).await {
        Ok(Some(b)) => b,
        Ok(None) => return, // we don't have it; drop silently.
        Err(_) => return, // I/O failure; drop silently for v1.
    };
    let state = self.state.lock().await;
    if let Some(tx) = state.peer_outbound.get(&from) {
        let _ = tx.send(SyncMessage::BlobResponse { block });
    }
}

async fn handle_blob_response(&self, block: sunset_store::ContentBlock) {
    // Idempotent insert; if we already have it, no-op.
    let _ = self.store.put_content(block).await;
}
```

- [ ] **Step 2: Update `handle_event_delivery` to issue BlobRequest for missing blobs**

In the for-loop, after `store.insert(entry, blob)`, if `blob` was `None` (i.e., the peer didn't ship the blob and we don't already have it locally), issue a `BlobRequest` to the sender:

```rust
for entry in trusted {
    let blob = blobs_by_hash.get(&entry.value_hash).cloned();
    let blob_was_supplied = blob.is_some();

    match self.store.insert(entry.clone(), blob).await {
        Ok(()) => { /* … */ }
        Err(sunset_store::Error::Stale) => { /* … */ }
        Err(e) => {
            eprintln!("sunset-sync: insert failed for entry from {:?}: {}", entry.verifying_key, e);
            continue;
        }
    }

    if !blob_was_supplied {
        // Check if we already have it (e.g., from an earlier round).
        let have = self
            .store
            .get_content(&entry.value_hash)
            .await
            .ok()
            .flatten()
            .is_some();
        if !have {
            // Issue a BlobRequest to the peer who sent us the entry.
            let state = self.state.lock().await;
            if let Some(tx) = state.peer_outbound.get(&from) {
                let _ = tx.send(SyncMessage::BlobRequest {
                    hash: entry.value_hash,
                });
            }
        }
    }
}
```

(Note `from` is the function parameter; rename or thread it through accordingly.)

- [ ] **Step 3: Add a test**

```rust
#[tokio::test(flavor = "current_thread")]
async fn blob_request_returns_existing_block() {
    use bytes::Bytes;
    use sunset_store::ContentBlock;

    let local = tokio::task::LocalSet::new();
    local.run_until(async {
        let engine = Arc::new(make_engine("alice", b"alice"));
        let block = ContentBlock {
            data: Bytes::from_static(b"data"),
            references: vec![],
        };
        let hash = block.hash();
        engine.store.put_content(block.clone()).await.unwrap();

        // Pre-register a fake outbound channel so handle_blob_request has somewhere to send.
        let (tx, mut rx) = mpsc::unbounded_channel::<SyncMessage>();
        engine
            .state
            .lock()
            .await
            .peer_outbound
            .insert(PeerId(vk(b"requester")), tx);

        engine
            .handle_blob_request(PeerId(vk(b"requester")), hash)
            .await;

        let response = rx.recv().await.unwrap();
        match response {
            SyncMessage::BlobResponse { block: got } => assert_eq!(got, block),
            other => panic!("expected BlobResponse, got {:?}", other),
        }
    }).await;
}

#[tokio::test(flavor = "current_thread")]
async fn blob_response_stores_block() {
    use bytes::Bytes;
    use sunset_store::ContentBlock;

    let local = tokio::task::LocalSet::new();
    local.run_until(async {
        let engine = Arc::new(make_engine("alice", b"alice"));
        let block = ContentBlock { data: Bytes::from_static(b"data"), references: vec![] };
        let hash = block.hash();
        engine.handle_blob_response(block.clone()).await;
        let got = engine.store.get_content(&hash).await.unwrap();
        assert_eq!(got, Some(block));
    }).await;
}
```

- [ ] **Step 4: Run tests**

Run: `nix develop --command cargo test -p sunset-sync --features test-helpers engine::`
Expected: all engine tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/sunset-sync/src/engine.rs
git commit -m "Wire BlobRequest / BlobResponse handling in SyncEngine"
```

---

### Task 14: DigestExchange + Fetch + bootstrap

**Files:**
- Modify: `crates/sunset-sync/src/digest.rs`
- Modify: `crates/sunset-sync/src/engine.rs`

Add the digest-round state machine. Each round is initiated by one peer (the "initiator") sending `DigestExchange { filter, range, bloom }`. The receiver (the "responder") computes its own digest over its local store entries matching `filter` within `range` and sends a corresponding `EventDelivery` of any entries it has but the bloom doesn't appear to contain.

Bootstrap: on `PeerHello`, the engine kicks off a digest round on `Filter::Namespace(_sunset-sync/subscribe)` toward the new peer. After bootstrap succeeds, the engine's local store sees the peer's subscription as a regular insert event, the SubscriptionRegistry updates, and the union filter is refreshed for push routing.

- [ ] **Step 1: Add `DigestRound` to `digest.rs`**

```rust
use bytes::Bytes;

use sunset_store::{Filter, SignedKvEntry, Store};

use crate::error::Result;
use crate::message::{DigestRange, SyncMessage};

/// Build a bloom filter over `(verifying_key, name, priority)` triples for
/// every entry in `store` matching `filter` within `range` (v1: All).
pub async fn build_digest<S: Store>(
    store: &S,
    filter: &Filter,
    _range: &DigestRange,
    bloom_size_bits: usize,
    bloom_hash_fns: u32,
) -> Result<BloomFilter> {
    use futures::StreamExt;
    let mut bloom = BloomFilter::new(bloom_size_bits, bloom_hash_fns);
    let mut iter = store.iter(filter.clone()).await?;
    while let Some(item) = iter.next().await {
        let entry = item?;
        bloom.insert(&digest_key(&entry));
    }
    Ok(bloom)
}

/// Canonical bytes used for bloom hashing of a `SignedKvEntry`. Includes
/// `(verifying_key, name, priority)` — sufficient to distinguish LWW
/// versions of the same key.
pub fn digest_key(entry: &SignedKvEntry) -> Bytes {
    use serde::Serialize;
    #[derive(Serialize)]
    struct Key<'a> {
        vk: &'a sunset_store::VerifyingKey,
        name: &'a [u8],
        priority: u64,
    }
    let key = Key {
        vk: &entry.verifying_key,
        name: &entry.name,
        priority: entry.priority,
    };
    Bytes::from(postcard::to_stdvec(&key).unwrap())
}

/// Walk the local store matching `filter` and return the entries whose
/// digest_key is NOT in `remote_bloom` — these are entries the remote is
/// missing.
pub async fn entries_missing_from_remote<S: Store>(
    store: &S,
    filter: &Filter,
    remote_bloom: &BloomFilter,
) -> Result<Vec<SignedKvEntry>> {
    use futures::StreamExt;
    let mut out = Vec::new();
    let mut iter = store.iter(filter.clone()).await?;
    while let Some(item) = iter.next().await {
        let entry = item?;
        if !remote_bloom.contains(&digest_key(&entry)) {
            out.push(entry);
        }
    }
    Ok(out)
}
```

(`SyncMessage` is imported but not used directly above — keep the import clean.)

- [ ] **Step 2: Add `DigestExchange` handling in `engine.rs`**

```rust
async fn handle_peer_message(&self, from: PeerId, message: SyncMessage) {
    match message {
        SyncMessage::EventDelivery { entries, blobs } => {
            self.handle_event_delivery(from, entries, blobs).await;
        }
        SyncMessage::BlobRequest { hash } => {
            self.handle_blob_request(from, hash).await;
        }
        SyncMessage::BlobResponse { block } => {
            self.handle_blob_response(block).await;
        }
        SyncMessage::DigestExchange { filter, range, bloom } => {
            self.handle_digest_exchange(from, filter, range, bloom).await;
        }
        SyncMessage::Fetch { .. } => {
            // v1: Fetch is a future-extension when DigestRange grows beyond
            // All; nothing to do today.
        }
        SyncMessage::Hello { .. } | SyncMessage::Goodbye { .. } => {
            // Handled by the per-peer task; engine ignores.
        }
    }
}

async fn handle_digest_exchange(
    &self,
    from: PeerId,
    filter: Filter,
    _range: DigestRange,
    bloom: Bytes,
) {
    let remote_bloom = crate::digest::BloomFilter::from_bytes(
        bloom,
        self.config.bloom_hash_fns,
    );
    let missing = match crate::digest::entries_missing_from_remote(
        &*self.store,
        &filter,
        &remote_bloom,
    )
    .await
    {
        Ok(v) => v,
        Err(e) => {
            eprintln!("sunset-sync: digest scan failed: {e}");
            return;
        }
    };
    if missing.is_empty() {
        return;
    }
    // Look up corresponding blobs (best-effort).
    let mut blobs = Vec::with_capacity(missing.len());
    for entry in &missing {
        if let Ok(Some(b)) = self.store.get_content(&entry.value_hash).await {
            blobs.push(b);
        }
    }
    let msg = SyncMessage::EventDelivery { entries: missing, blobs };
    let state = self.state.lock().await;
    if let Some(tx) = state.peer_outbound.get(&from) {
        let _ = tx.send(msg);
    }
}
```

- [ ] **Step 3: Bootstrap a digest exchange on PeerHello**

Update `handle_inbound_event`:

```rust
async fn handle_inbound_event(&self, event: InboundEvent) {
    match event {
        InboundEvent::PeerHello { peer_id, .. } => {
            // Fire bootstrap digest exchange.
            self.send_bootstrap_digest(&peer_id).await;
        }
        InboundEvent::Message { from, message } => {
            self.handle_peer_message(from, message).await;
        }
        InboundEvent::Disconnected { peer_id, .. } => {
            self.state.lock().await.peer_outbound.remove(&peer_id);
        }
    }
}

async fn send_bootstrap_digest(&self, to: &PeerId) {
    let bloom = match crate::digest::build_digest(
        &*self.store,
        &self.config.bootstrap_filter,
        &DigestRange::All,
        self.config.bloom_size_bits,
        self.config.bloom_hash_fns,
    )
    .await
    {
        Ok(b) => b,
        Err(_) => return,
    };
    let msg = SyncMessage::DigestExchange {
        filter: self.config.bootstrap_filter.clone(),
        range: DigestRange::All,
        bloom: bloom.to_bytes(),
    };
    let state = self.state.lock().await;
    if let Some(tx) = state.peer_outbound.get(to) {
        let _ = tx.send(msg);
    }
}
```

- [ ] **Step 4: Update SubscriptionRegistry on subscription-entry events**

When a `_sunset-sync/subscribe` entry is inserted into the local store (because a peer sent us their filter), update the registry. Modify `handle_local_store_event`:

```rust
async fn handle_local_store_event(&self, ev: Event) {
    let entry = match ev {
        Event::Inserted(e) => e,
        Event::Replaced { new, .. } => new,
        _ => return,
    };

    // If this is a subscription announcement, update the registry.
    if entry.name.as_ref() == reserved::SUBSCRIBE_NAME {
        if let Ok(Some(block)) = self.store.get_content(&entry.value_hash).await {
            if let Ok(filter) = crate::subscription_registry::parse_subscription_entry(&entry, &block) {
                self.state
                    .lock()
                    .await
                    .registry
                    .insert(entry.verifying_key.clone(), filter);
            }
        }
        // Don't re-broadcast subscription entries to other peers via push
        // flow — they should reach peers via their own digest exchanges
        // and the registry-driven push below. (Actually, for simplicity in
        // v1, we DO push them; transitive delivery is fine.)
    }

    // Push flow: route to peers whose filter matches.
    let state = self.state.lock().await;
    let blob = self.store.get_content(&entry.value_hash).await.ok().flatten();
    let msg = SyncMessage::EventDelivery {
        entries: vec![entry.clone()],
        blobs: blob.into_iter().collect(),
    };
    for peer in state.registry.peers_matching(&entry.verifying_key, &entry.name) {
        if let Some(tx) = state.peer_outbound.get(&peer) {
            let _ = tx.send(msg.clone());
        }
    }
    // Note: subscription entries match `Filter::Namespace(_sunset-sync/subscribe)`,
    // which a peer will subscribe to via its own publish_subscription only if
    // it explicitly cares. For v1 we rely on bootstrap + anti-entropy to spread them.
}
```

(The drop-and-relock pattern between mutex acquisitions is to keep the lock held for as short as possible; double-locking inside one fn is fine if it's brief.)

- [ ] **Step 5: Tests**

```rust
#[tokio::test(flavor = "current_thread")]
async fn digest_exchange_pushes_missing_entries_to_remote() {
    use bytes::Bytes;
    use sunset_store::{ContentBlock, SignedKvEntry, Store as _};

    let local = tokio::task::LocalSet::new();
    local.run_until(async {
        let engine = Arc::new(make_engine("alice", b"alice"));

        // Insert one entry locally.
        let block = ContentBlock { data: Bytes::from_static(b"x"), references: vec![] };
        let entry = SignedKvEntry {
            verifying_key: vk(b"writer"),
            name: Bytes::from_static(b"chat/k"),
            value_hash: block.hash(),
            priority: 1,
            expires_at: None,
            signature: Bytes::new(),
        };
        engine.store.insert(entry.clone(), Some(block.clone())).await.unwrap();

        // Pre-register a fake outbound channel for "remote".
        let (tx, mut rx) = mpsc::unbounded_channel::<SyncMessage>();
        engine.state.lock().await.peer_outbound.insert(PeerId(vk(b"remote")), tx);

        // Remote sends an empty bloom over a filter that matches the entry.
        let empty = crate::digest::BloomFilter::new(4096, 4);
        engine
            .handle_digest_exchange(
                PeerId(vk(b"remote")),
                Filter::Keyspace(vk(b"writer")),
                DigestRange::All,
                empty.to_bytes(),
            )
            .await;

        // Engine should respond with an EventDelivery containing `entry`.
        let msg = rx.recv().await.unwrap();
        match msg {
            SyncMessage::EventDelivery { entries, blobs } => {
                assert_eq!(entries.len(), 1);
                assert_eq!(entries[0], entry);
                assert_eq!(blobs.len(), 1);
                assert_eq!(blobs[0], block);
            }
            other => panic!("expected EventDelivery, got {:?}", other),
        }
    }).await;
}
```

- [ ] **Step 6: Run tests**

Run: `nix develop --command cargo test -p sunset-sync --features test-helpers`
Expected: all engine + digest tests pass.

- [ ] **Step 7: Commit**

```bash
git add crates/sunset-sync/src/digest.rs crates/sunset-sync/src/engine.rs
git commit -m "Add DigestExchange handling and bootstrap on PeerHello"
```

---

### Task 15: Anti-entropy timer

**Files:**
- Modify: `crates/sunset-sync/src/engine.rs`

Add a periodic tick (driven by `tokio::time::interval`) that issues a `DigestExchange` to every connected peer over the bootstrap filter. v1 covers only the bootstrap filter; later plans extend to per-peer-application filters.

- [ ] **Step 1: Add the timer arm to `run()`**

In the `select!`:

```rust
let mut anti_entropy = tokio::time::interval(self.config.anti_entropy_interval);
// First tick fires immediately; skip it so the bootstrap exchange isn't
// duplicated immediately after PeerHello.
anti_entropy.tick().await;

loop {
    tokio::select! {
        // ... existing arms ...
        _ = anti_entropy.tick() => {
            self.tick_anti_entropy().await;
        }
    }
}
```

And the new method:

```rust
async fn tick_anti_entropy(&self) {
    let bloom = match crate::digest::build_digest(
        &*self.store,
        &self.config.bootstrap_filter,
        &DigestRange::All,
        self.config.bloom_size_bits,
        self.config.bloom_hash_fns,
    )
    .await
    {
        Ok(b) => b,
        Err(_) => return,
    };
    let msg = SyncMessage::DigestExchange {
        filter: self.config.bootstrap_filter.clone(),
        range: DigestRange::All,
        bloom: bloom.to_bytes(),
    };
    let state = self.state.lock().await;
    for tx in state.peer_outbound.values() {
        let _ = tx.send(msg.clone());
    }
}
```

- [ ] **Step 2: Test (only verify the method runs without panicking — full end-to-end is Task 16)**

```rust
#[tokio::test(flavor = "current_thread")]
async fn tick_anti_entropy_with_no_peers_is_noop() {
    let local = tokio::task::LocalSet::new();
    local.run_until(async {
        let engine = Arc::new(make_engine("alice", b"alice"));
        engine.tick_anti_entropy().await; // shouldn't panic
    }).await;
}
```

- [ ] **Step 3: Run tests**

Run: `nix develop --command cargo test -p sunset-sync --features test-helpers`
Expected: all tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/sunset-sync/src/engine.rs
git commit -m "Add anti-entropy timer issuing periodic DigestExchange to all peers"
```

---

### Task 16: Two-peer integration test

**Files:**
- Create: `crates/sunset-sync/tests/two_peer_sync.rs`

Drive two `SyncEngine`s through `TestNetwork`. Alice writes an entry; Bob receives it via push. Assert Bob's store has the entry within a short timeout.

- [ ] **Step 1: Write the test**

```rust
//! Two-peer end-to-end integration test for sunset-sync.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use sunset_store::{ContentBlock, Filter, SignedKvEntry, Store as _, VerifyingKey};
use sunset_store_memory::MemoryStore;
use sunset_sync::test_transport::{TestNetwork, TestTransport};
use sunset_sync::{PeerAddr, PeerId, SyncConfig, SyncEngine};

fn vk(b: &[u8]) -> VerifyingKey {
    VerifyingKey::new(Bytes::copy_from_slice(b))
}

#[tokio::test(flavor = "current_thread")]
async fn alice_writes_bob_receives() {
    let local = tokio::task::LocalSet::new();
    local.run_until(async {
        let net = TestNetwork::new();
        let alice_addr = PeerAddr::new("alice");
        let bob_addr = PeerAddr::new("bob");
        let alice_id = PeerId(vk(b"alice"));
        let bob_id = PeerId(vk(b"bob"));

        let alice_transport = net.transport(alice_id.clone(), alice_addr.clone());
        let bob_transport = net.transport(bob_id.clone(), bob_addr.clone());

        let alice_store = Arc::new(MemoryStore::new());
        let bob_store = Arc::new(MemoryStore::new());

        let alice_engine = Arc::new(SyncEngine::new(
            alice_store.clone(),
            alice_transport,
            SyncConfig::default(),
            alice_id.clone(),
        ));
        let bob_engine = Arc::new(SyncEngine::new(
            bob_store.clone(),
            bob_transport,
            SyncConfig::default(),
            bob_id.clone(),
        ));

        // Run both engines.
        let alice_run = tokio::task::spawn_local({
            let e = alice_engine.clone();
            async move { e.run().await }
        });
        let bob_run = tokio::task::spawn_local({
            let e = bob_engine.clone();
            async move { e.run().await }
        });

        // Bob declares interest in keyspace `chat`.
        bob_engine
            .publish_subscription(Filter::Keyspace(vk(b"chat")), Duration::from_secs(60))
            .await
            .unwrap();

        // Alice connects to Bob.
        alice_engine.add_peer(bob_addr).await.unwrap();

        // Wait for the subscription to propagate from Bob -> Alice via
        // bootstrap digest exchange.
        // (In v1 this is short; we poll the registry with a short timeout.)
        let registered = wait_for(
            Duration::from_secs(2),
            Duration::from_millis(20),
            || async {
                let state = alice_engine.state.lock().await;
                state.registry.iter().any(|(vk_, _)| vk_ == &vk(b"bob"))
            },
        )
        .await;
        assert!(registered, "alice did not learn bob's subscription");

        // Alice writes an entry under (chat, k).
        let block = ContentBlock {
            data: Bytes::from_static(b"hello-bob"),
            references: vec![],
        };
        let entry = SignedKvEntry {
            verifying_key: vk(b"chat"),
            name: Bytes::from_static(b"k"),
            value_hash: block.hash(),
            priority: 1,
            expires_at: None,
            signature: Bytes::new(),
        };
        alice_store.insert(entry.clone(), Some(block.clone())).await.unwrap();

        // Bob should receive it via push.
        let received = wait_for(
            Duration::from_secs(2),
            Duration::from_millis(20),
            || async {
                bob_store.get_entry(&vk(b"chat"), b"k").await.unwrap().is_some()
            },
        )
        .await;
        assert!(received, "bob did not receive alice's entry");

        let bob_view = bob_store.get_entry(&vk(b"chat"), b"k").await.unwrap().unwrap();
        assert_eq!(bob_view, entry);

        alice_run.abort();
        bob_run.abort();
    }).await;
}

/// Poll `condition` until it returns `true` or the deadline elapses.
async fn wait_for<F, Fut>(deadline: Duration, interval: Duration, mut condition: F) -> bool
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let start = tokio::time::Instant::now();
    while start.elapsed() < deadline {
        if condition().await {
            return true;
        }
        tokio::time::sleep(interval).await;
    }
    false
}
```

(The test exposes `engine.state` and `engine.subscription_registry()` access via the `pub(crate)` field. If those aren't visible from the integration test, add a `#[cfg(any(test, feature = "test-helpers"))]` accessor on `SyncEngine`:

```rust
#[cfg(feature = "test-helpers")]
impl<S: Store + 'static, T: Transport + 'static> SyncEngine<S, T> {
    pub fn debug_state(&self) -> Arc<Mutex<EngineState>> {
        self.state.clone()
    }
}
```

Use whichever path is cleaner.)

- [ ] **Step 2: Run the integration test**

Run: `nix develop --command cargo test -p sunset-sync --features test-helpers --test two_peer_sync`
Expected: PASS within ~3 seconds.

If any assertion fails, **stop and diagnose**. Common failure shapes:
- `alice did not learn bob's subscription` → bootstrap digest exchange isn't delivering subscription entries. Check Task 14's `handle_local_store_event` and the `_sunset-sync/subscribe` filter routing.
- `bob did not receive alice's entry` → push flow isn't routing. Check `handle_local_store_event`'s registry lookup. Make sure Bob's subscription has been processed before Alice's write.
- Hangs forever → there's a deadlock between the engine's mutex and the per-peer task's mutex acquisition. Audit lock-ordering.

- [ ] **Step 3: Commit**

```bash
git add crates/sunset-sync/tests/two_peer_sync.rs
git commit -m "Add two-peer integration test (alice writes, bob receives via push)"
```

---

### Task 17: Final clippy + fmt + workspace test pass

**Files:**
- (No new files; cleanup pass)

- [ ] **Step 1: Run clippy across the workspace**

Run: `nix develop --command cargo clippy --workspace --all-features --all-targets -- -D warnings`
Expected: no warnings.

If clippy flags issues, fix them. Don't silence with `#[allow(...)]` unless the lint is genuinely wrong.

- [ ] **Step 2: Run fmt**

Run: `nix develop --command cargo fmt --all --check`
Expected: no diff. If diff, run `cargo fmt --all`.

- [ ] **Step 3: Run the full workspace test suite**

Run: `nix develop --command cargo test --workspace --all-features`
Expected: all tests across `sunset-store`, `sunset-store-memory`, `sunset-store-fs`, and `sunset-sync` pass — including the two-peer integration test.

- [ ] **Step 4: Commit any cleanup**

```bash
git add -A
git commit -m "Pass clippy + fmt across the workspace"
```

(Skip if no changes.)

---

## Verification (end-state acceptance)

After all 17 tasks land:

- `cargo test --workspace --all-features` passes — at least 50+ existing tests + ~25 new sunset-sync unit tests + the two-peer integration test.
- `cargo clippy --workspace --all-features --all-targets -- -D warnings` is clean.
- `cargo fmt --all --check` is clean.
- Two SyncEngine instances connected via `TestNetwork` exchange events end-to-end (`tests/two_peer_sync.rs`).
- `git log --oneline master..HEAD` shows seventeen task-by-task commits in order.

## Out of scope (deferred to follow-up plans)

- **WebRTC transport** — Plan 5 implements the browser (web-sys / wasm-bindgen) and native (`webrtc-rs`) transports against the trait surface defined here.
- **IndexedDB store backend** — Plan 3.
- **Reconnect / retry logic** — when a Transport connection drops, sunset-sync v1 just removes the peer from the registry. The host re-establishes via `add_peer`.
- **Backpressure** — per-connection rate-limiting, store-write throttling.
- **Catch-up pagination** — large stores need to partition `DigestRange` into multiple buckets.
- **Reconnect-aware anti-entropy** — currently anti-entropy hits all connected peers; could become smarter.
- **Concrete trust-set KV schema** — defined jointly with the identity subsystem.
- **Protocol versioning policy** — when does `protocol_version` bump, how is incompatibility surfaced.
- **`PEER_HEALTH_NAME` content** — the constant is reserved but no code reads/writes it in v1.
