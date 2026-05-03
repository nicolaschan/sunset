# Durable Relay Connect Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Push the relay reconnect/recovery loop from the gleam frontend (PR #18 band-aid) into `sunset-sync::PeerSupervisor`, and retire the special `relay_status` string in favor of intent snapshots flowing through one peer-status surface.

**Architecture:** The supervisor's `add()` becomes durable (no first-dial-failure cleanup), takes a `Connectable` enum (`Direct(PeerAddr)` or `Resolving { input, fetch }`), and runs the resolver inside the dial loop for `Resolving` intents — so a relay rotating identity is handled per attempt. `IntentSnapshot` gains `id`, `label`, `kind` and is the single status surface; `subscribe_intents()` replays the current state on subscribe and streams subsequent changes. `Client::add_relay` returns `IntentId` after a one cmd-channel round-trip; `Client::relay_status`, `Client::on_relay_status_changed`, and the membership tracker's relay-status helpers are removed. The gleam frontend trades `RelayConnectResult` + `RelayStatusUpdated` for `IntentChanged(snapshot)` and derives the room-status pill from intents.

**Tech Stack:** Rust (workspace, `cargo test --workspace --all-features`), wasm-bindgen (web client), gleam + Lustre (frontend), Playwright (e2e).

**Spec:** `docs/superpowers/specs/2026-05-03-durable-relay-connect-design.md`

**Pre-flight:**
- Work in the `.worktrees/spec-durable-relay-connect` worktree (already created off latest master).
- `nix develop --command cargo build --workspace --all-features` should succeed before starting Task 1.

---

## Task 1: Add `Connectable` + `resolve_addr` (sunset-sync)

Add the new type that the supervisor will consume in Task 2. The supervisor itself isn't changed yet — this task just lands the type plus its tests so it compiles and is exercised in isolation.

**Files:**
- Create: `crates/sunset-sync/src/connectable.rs`
- Modify: `crates/sunset-sync/src/lib.rs` (re-export)
- Modify: `crates/sunset-sync/Cargo.toml` (add `sunset-relay-resolver` dep)
- Modify: `Cargo.toml` (no change — `sunset-relay-resolver` is already in `[workspace.dependencies]`)

- [ ] **Step 1: Add the workspace dep to `sunset-sync`**

Edit `crates/sunset-sync/Cargo.toml`. Inside `[dependencies]`, add:

```toml
sunset-relay-resolver = { path = "../sunset-relay-resolver" }
```

Verify `[workspace.dependencies]` in the top-level `Cargo.toml` already has `sunset-relay-resolver = { path = "crates/sunset-relay-resolver" }`. (It does.)

- [ ] **Step 2: Write the failing tests**

Create `crates/sunset-sync/src/connectable.rs`:

```rust
//! What a supervisor-managed intent dials.
//!
//! `Direct(addr)` is a canonical `PeerAddr` (already carries
//! `#x25519=<hex>`) — no pre-dial work; `resolve_addr` returns a clone.
//!
//! `Resolving { input, fetch }` carries a user-typed string
//! (`relay.sunset.chat`, `wss://host:port`, …) plus an `HttpFetch`
//! impl. Each dial attempt runs the resolver to learn the relay's
//! x25519 key — re-resolving every attempt covers a relay that
//! rotates identity between deploys.

use std::rc::Rc;

use bytes::Bytes;
use sunset_relay_resolver::{HttpFetch, Resolver};

use crate::types::PeerAddr;

#[derive(Clone)]
pub enum Connectable {
    Direct(PeerAddr),
    Resolving {
        input: String,
        fetch: Rc<dyn HttpFetch>,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum ResolveErr {
    /// Permanent — the input string can't be parsed at all. The
    /// supervisor cancels the intent on this.
    #[error("parse error: {0}")]
    Parse(String),
    /// Transient — HTTP fetch / JSON / hex / I/O failed. The
    /// supervisor backs off and retries.
    #[error("transient resolve failure: {0}")]
    Transient(String),
}

impl Connectable {
    /// A short string that identifies this intent for UI display
    /// before a `peer_id` is known. For `Direct`, the canonical URL
    /// (which the user pasted themselves); for `Resolving`, the input.
    pub fn label(&self) -> String {
        match self {
            Connectable::Direct(addr) => {
                String::from_utf8_lossy(addr.as_bytes()).into_owned()
            }
            Connectable::Resolving { input, .. } => input.clone(),
        }
    }

    /// Produce the canonical `PeerAddr` to dial. For `Direct`, returns
    /// a clone immediately. For `Resolving`, runs the resolver via the
    /// supplied `HttpFetch`. `ResolveErr::Parse` is permanent;
    /// `ResolveErr::Transient` is retried.
    pub async fn resolve_addr(&self) -> Result<PeerAddr, ResolveErr> {
        match self {
            Connectable::Direct(addr) => Ok(addr.clone()),
            Connectable::Resolving { input, fetch } => {
                let resolver = Resolver::new(fetch.clone());
                match resolver.resolve(input).await {
                    Ok(canonical) => Ok(PeerAddr::new(Bytes::from(canonical))),
                    Err(sunset_relay_resolver::Error::MalformedInput(e)) => {
                        Err(ResolveErr::Parse(e))
                    }
                    Err(e) => Err(ResolveErr::Transient(format!("{e}"))),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::cell::RefCell;
    use sunset_relay_resolver::Result as ResolverResult;

    /// Fake fetcher that returns a pre-canned (url -> body) mapping
    /// per call, so tests can assert how many attempts have happened.
    struct FakeFetch {
        responses:
            RefCell<Vec<std::result::Result<String, sunset_relay_resolver::Error>>>,
        seen_count: RefCell<usize>,
    }

    impl FakeFetch {
        fn new(
            responses: Vec<std::result::Result<String, sunset_relay_resolver::Error>>,
        ) -> Rc<Self> {
            Rc::new(Self {
                responses: RefCell::new(responses),
                seen_count: RefCell::new(0),
            })
        }
    }

    #[async_trait(?Send)]
    impl HttpFetch for FakeFetch {
        async fn get(&self, _url: &str) -> ResolverResult<String> {
            *self.seen_count.borrow_mut() += 1;
            self.responses
                .borrow_mut()
                .remove(0)
                .map_err(|e| e)
        }
    }

    fn good_body(hex: &str) -> String {
        format!(
            "{{\"ed25519\":\"{}\",\"x25519\":\"{}\",\"address\":\"ws://x:1\"}}",
            "11".repeat(32),
            hex,
        )
    }

    #[tokio::test]
    async fn direct_returns_addr_clone() {
        let addr = PeerAddr::new(Bytes::from_static(b"wss://example#x25519=00"));
        let c = Connectable::Direct(addr.clone());
        let resolved = c.resolve_addr().await.unwrap();
        assert_eq!(resolved, addr);
    }

    #[tokio::test]
    async fn resolving_calls_fetcher_and_returns_canonical() {
        let hex = "ab".repeat(32);
        let body = good_body(&hex);
        let fetch = FakeFetch::new(vec![Ok(body)]);
        let c = Connectable::Resolving {
            input: "relay.example.com".into(),
            fetch: fetch.clone(),
        };
        let resolved = c.resolve_addr().await.unwrap();
        assert_eq!(*fetch.seen_count.borrow(), 1);
        let s = String::from_utf8(resolved.as_bytes().to_vec()).unwrap();
        assert!(s.starts_with("wss://relay.example.com#x25519="));
        assert!(s.ends_with(&hex));
    }

    #[tokio::test]
    async fn resolving_parse_error_is_permanent() {
        // Empty string is unparseable per `parse_input`.
        let fetch = FakeFetch::new(vec![]);
        let c = Connectable::Resolving {
            input: "".into(),
            fetch,
        };
        let err = c.resolve_addr().await.unwrap_err();
        assert!(matches!(err, ResolveErr::Parse(_)), "got {err:?}");
    }

    #[tokio::test]
    async fn resolving_http_error_is_transient() {
        let fetch =
            FakeFetch::new(vec![Err(sunset_relay_resolver::Error::Http(
                "status 503".into(),
            ))]);
        let c = Connectable::Resolving {
            input: "relay.example.com".into(),
            fetch,
        };
        let err = c.resolve_addr().await.unwrap_err();
        assert!(matches!(err, ResolveErr::Transient(_)), "got {err:?}");
    }

    #[test]
    fn label_for_direct_is_the_addr_string() {
        let addr = PeerAddr::new(Bytes::from_static(b"wss://h:1#x25519=00"));
        let c = Connectable::Direct(addr);
        assert_eq!(c.label(), "wss://h:1#x25519=00");
    }

    #[test]
    fn label_for_resolving_is_the_input() {
        let fetch = FakeFetch::new(vec![]);
        let c = Connectable::Resolving {
            input: "relay.sunset.chat".into(),
            fetch,
        };
        assert_eq!(c.label(), "relay.sunset.chat");
    }
}
```

Add the module to `crates/sunset-sync/src/lib.rs`. Find the existing `mod` lines (in alpha order) and insert:

```rust
mod connectable;
```

Plus the public re-exports near the bottom of the file (where things like `PeerSupervisor` are re-exported):

```rust
pub use connectable::{Connectable, ResolveErr};
```

- [ ] **Step 3: Run the tests; they should fail to compile**

```bash
nix develop --command cargo test -p sunset-sync connectable 2>&1 | tail -20
```

Expected: compile errors referencing `connectable` (the module exists but the crate won't have re-exports threaded everywhere yet — this step is just sanity that the new test module is being discovered).

- [ ] **Step 4: Verify the tests pass**

The implementation in Step 2 already makes the tests pass.

```bash
nix develop --command cargo test -p sunset-sync connectable 2>&1 | tail -20
```

Expected: 6 tests pass (`direct_returns_addr_clone`, `resolving_calls_fetcher_and_returns_canonical`, `resolving_parse_error_is_permanent`, `resolving_http_error_is_transient`, `label_for_direct_is_the_addr_string`, `label_for_resolving_is_the_input`).

- [ ] **Step 5: Lint**

```bash
nix develop --command cargo clippy --workspace --all-features --all-targets -- -D warnings 2>&1 | tail -10
```

Expected: clean.

- [ ] **Step 6: Commit**

```bash
git add crates/sunset-sync/Cargo.toml crates/sunset-sync/src/connectable.rs crates/sunset-sync/src/lib.rs
git commit -m "$(cat <<'EOF'
sunset-sync: add Connectable enum + resolve_addr method

Direct(PeerAddr) returns a clone; Resolving { input, fetch } runs the
sunset-relay-resolver per call. ResolveErr distinguishes Parse
(permanent — supervisor will cancel the intent) from Transient
(supervisor will back off and retry on next attempt). No supervisor
integration yet — just the type + unit tests.

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>
EOF
)"
```

---

## Task 2: Refactor supervisor — Connectable + IntentId + durable + replay-on-subscribe

This is the largest task. It atomically:
1. Replaces `add(addr) -> Result<()>` with `add(Connectable) -> Result<IntentId, ResolveErr>` — only `ResolveErr::Parse` aborts; transient is internal.
2. Re-keys `SupervisorState.intents` from `HashMap<PeerAddr, IntentEntry>` to `HashMap<IntentId, IntentEntry>`.
3. Re-keys the reverse `peer_to_addr` map to `peer_to_intent: HashMap<PeerId, IntentId>`.
4. Adds `id`, `label`, `kind` fields to `IntentSnapshot`.
5. Removes the first-dial-failure cleanup branch (intent now stays and goes to Backoff).
6. Adds dedup: same `Connectable::Direct(addr)` or `Connectable::Resolving { input }` reuses existing `IntentId`.
7. Replaces `subscribe()` with `subscribe_intents()` that emits a snapshot of every existing intent on subscribe, then live changes.
8. Renames `remove(addr)` → `remove(IntentId)`.
9. Wires `Connectable::resolve_addr` into the dial loop.
10. Rewrites the four existing supervisor unit tests to express the new contract.

**Files:**
- Modify: `crates/sunset-sync/src/supervisor.rs` (most of the file)
- Modify: `crates/sunset-sync/src/peer.rs` (only if `peer_to_addr` is referenced — likely not)

- [ ] **Step 1: Replace `IntentSnapshot`, `IntentEntry`, `SupervisorState`, `SupervisorCommand`**

Open `crates/sunset-sync/src/supervisor.rs`. Replace the block starting at `pub enum IntentState` (≈ line 60) through the end of `pub(crate) enum SupervisorCommand` with:

```rust
/// Per-intent state observed via `snapshot()`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IntentState {
    Connecting,
    Connected,
    Backoff,
    Cancelled,
}

/// Opaque, monotonically-allocated identifier for a registered intent.
/// Stable across reconnect cycles (peer_id may not be known yet on
/// first attempt; the IntentId always is).
pub type IntentId = u64;

#[derive(Clone, Debug)]
pub struct IntentSnapshot {
    pub id: IntentId,
    pub state: IntentState,
    pub peer_id: Option<PeerId>,
    pub kind: Option<crate::transport::TransportKind>,
    pub attempt: u32,
    /// Display label — `Connectable::label()` of the intent that
    /// created this snapshot. Stable across reconnect cycles.
    pub label: String,
}

pub(crate) struct IntentEntry {
    pub id: IntentId,
    pub state: IntentState,
    pub attempt: u32,
    pub peer_id: Option<PeerId>,
    pub kind: Option<crate::transport::TransportKind>,
    /// Earliest moment the next dial attempt may run. None when not in Backoff.
    pub next_attempt_at: Option<web_time::SystemTime>,
    /// What to dial. Used by every retry attempt; for `Resolving`,
    /// the resolver runs on every attempt so a rotated identity is
    /// picked up automatically.
    pub connectable: crate::connectable::Connectable,
    /// Cached `Connectable::label()` for snapshot construction.
    pub label: String,
}

pub(crate) struct SupervisorState {
    pub next_id: IntentId,
    pub intents: HashMap<IntentId, IntentEntry>,
    /// Reverse map: connected peer_id → intent that owns the connection.
    pub peer_to_intent: HashMap<PeerId, IntentId>,
    /// Dedup: existing `Direct(addr)` intent for this address, if any.
    pub direct_dedup: HashMap<PeerAddr, IntentId>,
    /// Dedup: existing `Resolving { input }` intent for this input, if any.
    pub resolving_dedup: HashMap<String, IntentId>,
    /// Live-state subscribers. Each receives an `IntentSnapshot` every
    /// time an intent transitions, plus an initial replay on subscribe.
    /// Senders whose receiver is dropped are pruned at broadcast time.
    pub subscribers: Vec<mpsc::UnboundedSender<IntentSnapshot>>,
}

pub(crate) enum SupervisorCommand {
    Add {
        connectable: crate::connectable::Connectable,
        ack: oneshot::Sender<Result<IntentId, crate::connectable::ResolveErr>>,
    },
    Remove {
        id: IntentId,
        ack: oneshot::Sender<()>,
    },
    Snapshot {
        ack: oneshot::Sender<Vec<IntentSnapshot>>,
    },
    Subscribe {
        ack: oneshot::Sender<mpsc::UnboundedReceiver<IntentSnapshot>>,
    },
}
```

- [ ] **Step 2: Update `PeerSupervisor::new`**

Replace the body of `PeerSupervisor::new` so the initial `SupervisorState` has the new fields:

```rust
pub fn new(engine: Rc<SyncEngine<S, T>>, policy: BackoffPolicy) -> Rc<Self> {
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    Rc::new(Self {
        engine,
        cmd_tx,
        cmd_rx: RefCell::new(Some(cmd_rx)),
        state: Rc::new(RefCell::new(SupervisorState {
            next_id: 0,
            intents: HashMap::new(),
            peer_to_intent: HashMap::new(),
            direct_dedup: HashMap::new(),
            resolving_dedup: HashMap::new(),
            subscribers: Vec::new(),
        })),
        policy,
    })
}
```

- [ ] **Step 3: Replace public API — `add`, `remove`, `subscribe`**

Replace the `add`, `remove`, and `subscribe` impls with:

```rust
/// Register a durable intent. Returns once the supervisor has
/// allocated an `IntentId` and recorded the intent (one cmd-channel
/// round-trip; does NOT wait for the first connection). The only
/// `Err` is `ResolveErr::Parse` — typed garbage that the resolver
/// can never make sense of. Transient failures (resolver fetch,
/// dial, Hello) never bubble out: the supervisor retries with the
/// existing exponential backoff.
///
/// If `connectable` matches an existing intent (same `Direct` addr,
/// or same `Resolving { input }`), the existing `IntentId` is
/// returned and no new intent is created.
pub async fn add(
    &self,
    connectable: crate::connectable::Connectable,
) -> Result<IntentId, crate::connectable::ResolveErr> {
    let (ack, rx) = oneshot::channel();
    self.cmd_tx
        .send(SupervisorCommand::Add { connectable, ack })
        .map_err(|_| crate::connectable::ResolveErr::Transient("supervisor closed".into()))?;
    rx.await
        .map_err(|_| crate::connectable::ResolveErr::Transient("supervisor closed".into()))?
}

/// Cancel a durable intent. Tears down the connection if connected.
pub async fn remove(&self, id: IntentId) {
    let (ack, rx) = oneshot::channel();
    if self
        .cmd_tx
        .send(SupervisorCommand::Remove { id, ack })
        .is_ok()
    {
        let _ = rx.await;
    }
}

/// Subscribe to intent state changes. The returned receiver is fed
/// the current snapshot of every existing intent on subscribe (so
/// late subscribers don't miss state), then every change after that.
pub async fn subscribe_intents(
    &self,
) -> mpsc::UnboundedReceiver<IntentSnapshot> {
    let (ack, rx) = oneshot::channel();
    if self
        .cmd_tx
        .send(SupervisorCommand::Subscribe { ack })
        .is_err()
    {
        let (_tx, rx) = mpsc::unbounded_channel();
        return rx;
    }
    rx.await.unwrap_or_else(|_| {
        let (_tx, rx) = mpsc::unbounded_channel();
        rx
    })
}
```

The old `subscribe()` method (returning a `LocalBoxStream`) is replaced — delete it. If anything in this crate still calls `subscribe()`, switch it to `subscribe_intents()`.

- [ ] **Step 4: Update `broadcast` to take `IntentId`**

Replace the existing `broadcast` helper (≈ line 184–197):

```rust
/// Broadcast the current snapshot of `id` to all subscribers.
/// Drops senders whose receiver has been dropped. Caller must hold
/// the inner state lock.
fn broadcast(state: &mut SupervisorState, id: IntentId) {
    let Some(entry) = state.intents.get(&id) else {
        return;
    };
    let snap = IntentSnapshot {
        id,
        state: entry.state,
        peer_id: entry.peer_id.clone(),
        kind: entry.kind,
        attempt: entry.attempt,
        label: entry.label.clone(),
    };
    state.subscribers.retain(|tx| tx.send(snap.clone()).is_ok());
}
```

- [ ] **Step 5: Replace `handle_command` body**

Find `async fn handle_command` and replace its body with:

```rust
async fn handle_command(
    self: Rc<Self>,
    cmd: SupervisorCommand,
    rng: &mut rand_chacha::ChaCha20Rng,
) {
    match cmd {
        SupervisorCommand::Add { connectable, ack } => {
            // Dedup by Connectable identity.
            let dedup_id: Option<IntentId> = {
                let state = self.state.borrow();
                match &connectable {
                    crate::connectable::Connectable::Direct(addr) => {
                        state.direct_dedup.get(addr).copied()
                    }
                    crate::connectable::Connectable::Resolving { input, .. } => {
                        state.resolving_dedup.get(input).copied()
                    }
                }
            };
            if let Some(existing) = dedup_id {
                let _ = ack.send(Ok(existing));
                return;
            }

            // Eager parse-check for Resolving inputs. The resolver
            // does this internally on every attempt, but we run it
            // once here so unparseable inputs never become a zombie
            // intent that retries forever.
            if let crate::connectable::Connectable::Resolving { input, .. } =
                &connectable
            {
                if let Err(
                    sunset_relay_resolver::Error::MalformedInput(e),
                ) = sunset_relay_resolver::parse_input(input)
                {
                    let _ = ack.send(Err(crate::connectable::ResolveErr::Parse(e)));
                    return;
                }
            }

            let id = {
                let mut state = self.state.borrow_mut();
                let id = state.next_id;
                state.next_id += 1;

                match &connectable {
                    crate::connectable::Connectable::Direct(addr) => {
                        state.direct_dedup.insert(addr.clone(), id);
                    }
                    crate::connectable::Connectable::Resolving { input, .. } => {
                        state.resolving_dedup.insert(input.clone(), id);
                    }
                }

                let label = connectable.label();
                state.intents.insert(
                    id,
                    IntentEntry {
                        id,
                        state: IntentState::Connecting,
                        attempt: 0,
                        peer_id: None,
                        kind: None,
                        next_attempt_at: None,
                        connectable: connectable.clone(),
                        label,
                    },
                );
                Self::broadcast(&mut state, id);
                id
            };

            // Reply to the caller now that the intent is registered.
            // The first dial happens in the background; failures
            // transition the intent to Backoff (no longer surfaced).
            let _ = ack.send(Ok(id));

            self.clone().spawn_dial(id);
        }

        SupervisorCommand::Remove { id, ack } => {
            let (peer_id_to_remove, addr_to_unmap, input_to_unmap) = {
                let mut state = self.state.borrow_mut();
                let entry = state.intents.get_mut(&id);
                let (pid, addr, input) = if let Some(entry) = entry {
                    entry.state = IntentState::Cancelled;
                    let pid = entry.peer_id.clone();
                    let addr = match &entry.connectable {
                        crate::connectable::Connectable::Direct(a) => Some(a.clone()),
                        _ => None,
                    };
                    let input = match &entry.connectable {
                        crate::connectable::Connectable::Resolving { input, .. } => {
                            Some(input.clone())
                        }
                        _ => None,
                    };
                    (pid, addr, input)
                } else {
                    (None, None, None)
                };
                if let Some(p) = &pid {
                    state.peer_to_intent.remove(p);
                }
                Self::broadcast(&mut state, id);
                (pid, addr, input)
            };
            if let Some(pid) = peer_id_to_remove {
                let _ = self.engine.remove_peer(pid).await;
            }
            {
                let mut state = self.state.borrow_mut();
                if let Some(addr) = addr_to_unmap {
                    state.direct_dedup.remove(&addr);
                }
                if let Some(input) = input_to_unmap {
                    state.resolving_dedup.remove(&input);
                }
                state.intents.remove(&id);
            }
            let _ = ack.send(());
        }

        SupervisorCommand::Snapshot { ack } => {
            let state = self.state.borrow();
            let snap: Vec<IntentSnapshot> = state
                .intents
                .values()
                .map(|e| IntentSnapshot {
                    id: e.id,
                    state: e.state,
                    peer_id: e.peer_id.clone(),
                    kind: e.kind,
                    attempt: e.attempt,
                    label: e.label.clone(),
                })
                .collect();
            let _ = ack.send(snap);
        }

        SupervisorCommand::Subscribe { ack } => {
            let (tx, rx) = mpsc::unbounded_channel();
            // Replay current state, then register for live updates.
            // Replay happens BEFORE register so callers can't see a
            // change for an intent without first having received its
            // baseline snapshot.
            {
                let mut state = self.state.borrow_mut();
                let snaps: Vec<IntentSnapshot> = state
                    .intents
                    .values()
                    .map(|e| IntentSnapshot {
                        id: e.id,
                        state: e.state,
                        peer_id: e.peer_id.clone(),
                        kind: e.kind,
                        attempt: e.attempt,
                        label: e.label.clone(),
                    })
                    .collect();
                for snap in snaps {
                    if tx.send(snap).is_err() {
                        let _ = ack.send(rx);
                        return;
                    }
                }
                state.subscribers.push(tx);
            }
            let _ = ack.send(rx);
        }
    }
    let _ = rng; // unchanged: rng is used by fire_due_backoffs / spawn_dial
}
```

- [ ] **Step 6: Add `spawn_dial` helper (replaces the inline `crate::spawn::spawn_local` block in old `Add` / `fire_due_backoffs`)**

Add a new private method on `PeerSupervisor`:

```rust
/// Spawn a single dial attempt for the given intent. On success,
/// transitions the intent to Connected and populates peer_id +
/// kind. On failure, transitions to Backoff and schedules the next
/// attempt with the existing `BackoffPolicy`.
fn spawn_dial(self: Rc<Self>, id: IntentId) {
    let engine = self.engine.clone();
    let state = self.state.clone();
    let policy = self.policy.clone();
    crate::spawn::spawn_local(async move {
        // Snapshot the connectable for this attempt.
        let connectable = {
            let s = state.borrow();
            match s.intents.get(&id) {
                Some(entry) if entry.state != IntentState::Cancelled => {
                    entry.connectable.clone()
                }
                _ => return,
            }
        };

        let attempt_result = match connectable.resolve_addr().await {
            Ok(addr) => engine
                .add_peer(addr)
                .await
                .map_err(crate::connectable::ResolveErr::from),
            Err(e) => Err(e),
        };

        // Compute next state under lock.
        let mut s = state.borrow_mut();
        let Some(entry) = s.intents.get_mut(&id) else {
            return;
        };
        if entry.state == IntentState::Cancelled {
            // Intent was cancelled while the dial was in flight.
            // If we got a peer, tear it down outside the lock below.
            if let Ok(_peer_id) = &attempt_result {
                // Best-effort; we don't have engine reference inside
                // the lock here. Leave the disconnect to the
                // PeerRemoved handler.
            }
            return;
        }
        match attempt_result {
            Ok(peer_id) => {
                entry.state = IntentState::Connected;
                entry.peer_id = Some(peer_id.clone());
                entry.attempt = 0;
                entry.next_attempt_at = None;
                // Kind is filled by the PeerAdded engine event.
                s.peer_to_intent.insert(peer_id, id);
                Self::broadcast(&mut s, id);
            }
            Err(crate::connectable::ResolveErr::Parse(_)) => {
                // Permanent. Cancel the intent.
                entry.state = IntentState::Cancelled;
                Self::broadcast(&mut s, id);
            }
            Err(_transient) => {
                entry.attempt = entry.attempt.saturating_add(1);
                entry.state = IntentState::Backoff;
                // Use a deterministic-ish RNG seeded from the entry
                // id + attempt so retries don't pile up at the same
                // wall time across crates that share a global RNG.
                let mut rng = rand_chacha::ChaCha20Rng::seed_from_u64(
                    id.wrapping_mul(0x9E37_79B9_7F4A_7C15)
                        .wrapping_add(entry.attempt as u64),
                );
                let delay = policy.delay(entry.attempt, &mut rng);
                entry.next_attempt_at = Some(web_time::SystemTime::now() + delay);
                Self::broadcast(&mut s, id);
            }
        }
    });
}
```

Add a `From<crate::error::Error>` impl for `ResolveErr` in `crates/sunset-sync/src/connectable.rs`:

```rust
impl From<crate::error::Error> for ResolveErr {
    fn from(e: crate::error::Error) -> Self {
        ResolveErr::Transient(format!("{e}"))
    }
}
```

- [ ] **Step 7: Replace `fire_due_backoffs` to use `IntentId` and `spawn_dial`**

Find `async fn fire_due_backoffs` and replace its body:

```rust
async fn fire_due_backoffs(self: Rc<Self>, _rng: &mut rand_chacha::ChaCha20Rng) {
    let now = web_time::SystemTime::now();
    let due: Vec<IntentId> = {
        let state = self.state.borrow();
        state
            .intents
            .iter()
            .filter(|(_, e)| {
                e.state == IntentState::Backoff
                    && e.next_attempt_at.map(|at| at <= now).unwrap_or(false)
            })
            .map(|(id, _)| *id)
            .collect()
    };

    for id in due {
        {
            let mut state = self.state.borrow_mut();
            if let Some(entry) = state.intents.get_mut(&id) {
                if entry.state != IntentState::Backoff {
                    continue;
                }
                entry.state = IntentState::Connecting;
                entry.next_attempt_at = None;
                Self::broadcast(&mut state, id);
            }
        }
        self.clone().spawn_dial(id);
    }
}
```

- [ ] **Step 8: Replace `handle_engine_event` to use `peer_to_intent` and populate `kind`**

Find `async fn handle_engine_event` and replace:

```rust
async fn handle_engine_event(
    self: Rc<Self>,
    ev: EngineEvent,
    _rng: &mut rand_chacha::ChaCha20Rng,
) {
    match ev {
        EngineEvent::PeerAdded { peer_id, kind } => {
            let mut state = self.state.borrow_mut();
            if let Some(id) = state.peer_to_intent.get(&peer_id).copied() {
                if let Some(entry) = state.intents.get_mut(&id) {
                    entry.state = IntentState::Connected;
                    entry.kind = Some(kind);
                    entry.attempt = 0;
                    entry.next_attempt_at = None;
                    Self::broadcast(&mut state, id);
                }
            }
        }
        EngineEvent::PeerRemoved { peer_id } => {
            let mut state = self.state.borrow_mut();
            if let Some(id) = state.peer_to_intent.remove(&peer_id) {
                if let Some(entry) = state.intents.get_mut(&id) {
                    if entry.state != IntentState::Cancelled {
                        entry.state = IntentState::Backoff;
                        entry.peer_id = None;
                        entry.kind = None;
                        let mut rng = rand_chacha::ChaCha20Rng::seed_from_u64(
                            id.wrapping_mul(0x9E37_79B9_7F4A_7C15)
                                .wrapping_add(entry.attempt as u64),
                        );
                        let delay = self.policy.delay(entry.attempt, &mut rng);
                        entry.next_attempt_at =
                            Some(web_time::SystemTime::now() + delay);
                        Self::broadcast(&mut state, id);
                    }
                }
            }
        }
    }
}
```

- [ ] **Step 9: Rewrite the four existing supervisor unit tests**

Open `crates/sunset-sync/src/supervisor.rs`, find the `mod tests` block (≈ line 464). Replace the four tests as follows. Leave the helpers (`vk`, `StubSigner`, `engine_with_addr`) alone except for the small adjustments noted.

Replace `first_dial_success`:

```rust
#[tokio::test(flavor = "current_thread")]
async fn first_dial_success_returns_id_and_connects() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let net = TestNetwork::new();
            let alice = engine_with_addr(&net, b"alice", "alice");
            let _bob = engine_with_addr(&net, b"bob", "bob");

            crate::spawn::spawn_local({
                let a = alice.clone();
                async move { a.run().await }
            });
            crate::spawn::spawn_local({
                let b = _bob.clone();
                async move { b.run().await }
            });

            let sup = PeerSupervisor::new(alice.clone(), BackoffPolicy::default());
            crate::spawn::spawn_local({
                let s = sup.clone();
                async move { s.run().await }
            });

            let bob_addr = PeerAddr::new(Bytes::from_static(b"bob"));
            let id = sup
                .add(crate::connectable::Connectable::Direct(bob_addr))
                .await
                .unwrap();

            // Wait until the snapshot reports Connected (with a
            // bounded retry — the dial happens asynchronously now).
            let mut connected = false;
            for _ in 0..50 {
                let snap = sup.snapshot().await;
                if snap.iter().any(|s| s.id == id && s.state == IntentState::Connected) {
                    connected = true;
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            assert!(connected, "intent did not reach Connected");
        })
        .await;
}
```

Replace `first_dial_failure_returns_err_and_clears_intent` (rename + rewrite) — this is the core durability test:

```rust
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn first_dial_failure_enters_backoff_and_retries() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let net = TestNetwork::new();
            let alice = engine_with_addr(&net, b"alice", "alice");

            crate::spawn::spawn_local({
                let a = alice.clone();
                async move { a.run().await }
            });

            let sup = PeerSupervisor::new(alice.clone(), BackoffPolicy::default());
            crate::spawn::spawn_local({
                let s = sup.clone();
                async move { s.run().await }
            });

            // No engine is listening at "ghost" yet.
            let ghost = PeerAddr::new(Bytes::from_static(b"ghost"));
            let id = sup
                .add(crate::connectable::Connectable::Direct(ghost.clone()))
                .await
                .expect("add must return Ok even if first dial will fail");

            // Wait for the intent to enter Backoff (first dial failed).
            let mut backoff_seen = false;
            for _ in 0..200 {
                let snap = sup.snapshot().await;
                if snap.iter().any(|s| s.id == id && s.state == IntentState::Backoff) {
                    backoff_seen = true;
                    break;
                }
                tokio::time::advance(std::time::Duration::from_millis(50)).await;
            }
            assert!(backoff_seen, "intent did not enter Backoff after first dial failure");

            // Bring "ghost" online; intent should reconnect.
            let _ghost_engine = engine_with_addr(&net, b"ghost", "ghost");
            crate::spawn::spawn_local({
                let g = _ghost_engine.clone();
                async move { g.run().await }
            });

            // Advance time past the longest plausible backoff (max 30 s
            // by default policy) and tick the supervisor's backoff timer.
            let mut connected = false;
            for _ in 0..200 {
                tokio::time::advance(std::time::Duration::from_secs(1)).await;
                let snap = sup.snapshot().await;
                if snap.iter().any(|s| s.id == id && s.state == IntentState::Connected) {
                    connected = true;
                    break;
                }
            }
            assert!(connected, "intent did not reconnect after engine came up");

            // Intent is still in the table (no first-dial cleanup).
            assert!(sup.snapshot().await.iter().any(|s| s.id == id));
        })
        .await;
}
```

Replace `idempotent_add`:

```rust
#[tokio::test(flavor = "current_thread")]
async fn idempotent_add_returns_same_id() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let net = TestNetwork::new();
            let alice = engine_with_addr(&net, b"alice", "alice");
            let _bob = engine_with_addr(&net, b"bob", "bob");

            crate::spawn::spawn_local({
                let a = alice.clone();
                async move { a.run().await }
            });
            crate::spawn::spawn_local({
                let b = _bob.clone();
                async move { b.run().await }
            });

            let sup = PeerSupervisor::new(alice.clone(), BackoffPolicy::default());
            crate::spawn::spawn_local({
                let s = sup.clone();
                async move { s.run().await }
            });

            let bob_addr = PeerAddr::new(Bytes::from_static(b"bob"));
            let id1 = sup
                .add(crate::connectable::Connectable::Direct(bob_addr.clone()))
                .await
                .unwrap();
            let id2 = sup
                .add(crate::connectable::Connectable::Direct(bob_addr.clone()))
                .await
                .unwrap();
            let id3 = sup
                .add(crate::connectable::Connectable::Direct(bob_addr))
                .await
                .unwrap();

            assert_eq!(id1, id2);
            assert_eq!(id2, id3);
            assert_eq!(sup.snapshot().await.len(), 1);
        })
        .await;
}
```

Replace `remove_cancels_intent`:

```rust
#[tokio::test(flavor = "current_thread")]
async fn remove_cancels_intent() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let net = TestNetwork::new();
            let alice = engine_with_addr(&net, b"alice", "alice");
            let bob = engine_with_addr(&net, b"bob", "bob");

            crate::spawn::spawn_local({
                let a = alice.clone();
                async move { a.run().await }
            });
            crate::spawn::spawn_local({
                let b = bob.clone();
                async move { b.run().await }
            });

            let sup = PeerSupervisor::new(alice.clone(), BackoffPolicy::default());
            crate::spawn::spawn_local({
                let s = sup.clone();
                async move { s.run().await }
            });

            let bob_addr = PeerAddr::new(Bytes::from_static(b"bob"));
            let id = sup
                .add(crate::connectable::Connectable::Direct(bob_addr))
                .await
                .unwrap();

            // Wait for connect.
            for _ in 0..50 {
                let snap = sup.snapshot().await;
                if snap.iter().any(|s| s.id == id && s.state == IntentState::Connected) {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }

            sup.remove(id).await;

            assert!(sup.snapshot().await.iter().all(|s| s.id != id));
            let connected = alice.connected_peers().await;
            assert!(
                connected
                    .iter()
                    .find(|p| p.0.as_bytes() == b"bob")
                    .is_none()
            );
        })
        .await;
}
```

- [ ] **Step 10: Run the supervisor tests**

```bash
nix develop --command cargo test -p sunset-sync supervisor 2>&1 | tail -40
```

Expected: 4 tests pass (`first_dial_success_returns_id_and_connects`, `first_dial_failure_enters_backoff_and_retries`, `idempotent_add_returns_same_id`, `remove_cancels_intent`).

If `first_dial_failure_enters_backoff_and_retries` flakes due to timer-virtualization issues, it's likely a real bug — the supervisor's backoff logic depends on `web_time::SystemTime::now()`, not `tokio::time`. Resolve by either: (a) extending the test's tick budget; (b) parameterizing the supervisor's clock for tests; (c) explicit short-circuit when `tokio::time::pause` is detected. Pick the smallest fix; don't paper over the symptom by removing the assertion.

- [ ] **Step 11: Run the full workspace build**

```bash
nix develop --command cargo build --workspace --all-features 2>&1 | tail -10
```

Expected: clean build. If `peer_to_addr` was referenced elsewhere (it shouldn't be), update the references to `peer_to_intent`.

- [ ] **Step 12: Run clippy**

```bash
nix develop --command cargo clippy --workspace --all-features --all-targets -- -D warnings 2>&1 | tail -10
```

Expected: clean.

- [ ] **Step 13: Commit**

```bash
git add crates/sunset-sync
git commit -m "$(cat <<'EOF'
sunset-sync: durable supervisor on Connectable + IntentId

PeerSupervisor now:
  * accepts Connectable (Direct or Resolving) instead of PeerAddr
  * returns IntentId from add() once the intent is registered (one
    cmd-channel round-trip; no longer awaits first connection)
  * keeps the intent across first-dial failure — transitions to
    Backoff like any other failure (existing exponential backoff)
  * keys SupervisorState by IntentId; PeerAddr/input dedup live in
    side maps so `add()` is idempotent for the same Connectable
  * exposes subscribe_intents() with replay-on-subscribe
  * extends IntentSnapshot with id, kind, label

Resolving intents run sunset-relay-resolver per dial attempt, so an
upstream identity rotation between deploys is picked up
automatically. ResolveErr::Parse aborts the intent permanently;
every other failure is transient and gets retried.

The four existing supervisor tests are rewritten for the new
contract (first_dial_failure now asserts durability + reconnect,
idempotent_add asserts shared IntentId, remove takes IntentId).

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>
EOF
)"
```

---

## Task 3: Add new supervisor unit tests for Resolving + replay

Cover the two behaviors the existing tests don't: (1) `Resolving` re-runs the resolver per attempt; (2) `subscribe_intents` replays current state on subscribe.

**Files:**
- Modify: `crates/sunset-sync/src/supervisor.rs` (tests block)

- [ ] **Step 1: Add a helper for a fake `HttpFetch` in the test module**

Inside the `#[cfg(all(test, feature = "test-helpers"))] mod tests` block, after the `engine_with_addr` helper, add:

```rust
struct CountingFakeFetch {
    body: String,
    fail_first: std::cell::Cell<usize>,
    seen: std::cell::Cell<usize>,
}

impl CountingFakeFetch {
    fn new(body: String, fail_first: usize) -> Rc<Self> {
        Rc::new(Self {
            body,
            fail_first: std::cell::Cell::new(fail_first),
            seen: std::cell::Cell::new(0),
        })
    }
}

#[async_trait::async_trait(?Send)]
impl sunset_relay_resolver::HttpFetch for CountingFakeFetch {
    async fn get(&self, _url: &str) -> sunset_relay_resolver::Result<String> {
        self.seen.set(self.seen.get() + 1);
        let n = self.fail_first.get();
        if n > 0 {
            self.fail_first.set(n - 1);
            return Err(sunset_relay_resolver::Error::Http("status 503".into()));
        }
        Ok(self.body.clone())
    }
}
```

- [ ] **Step 2: Write `resolving_intent_re_resolves_each_attempt`**

Add inside the same `mod tests`:

```rust
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn resolving_intent_re_resolves_each_attempt() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            // The resolved address must point at an in-network engine
            // so the dial that follows resolve actually connects.
            let net = TestNetwork::new();
            let alice = engine_with_addr(&net, b"alice", "alice");
            let _bob = engine_with_addr(&net, b"bob", "bob");
            crate::spawn::spawn_local({
                let a = alice.clone();
                async move { a.run().await }
            });
            crate::spawn::spawn_local({
                let b = _bob.clone();
                async move { b.run().await }
            });

            // Fake resolver body whose canonical URL points at "bob".
            // The hex value is irrelevant for the TestNetwork transport,
            // which routes by addr only.
            let body = format!(
                "{{\"ed25519\":\"{}\",\"x25519\":\"{}\",\"address\":\"ws://bob\"}}",
                "00".repeat(32),
                "ab".repeat(32),
            );
            let fetch = CountingFakeFetch::new(body, 2); // fail twice, then succeed

            let sup = PeerSupervisor::new(alice.clone(), BackoffPolicy::default());
            crate::spawn::spawn_local({
                let s = sup.clone();
                async move { s.run().await }
            });

            let id = sup
                .add(crate::connectable::Connectable::Resolving {
                    input: "bob".into(),
                    fetch: fetch.clone(),
                })
                .await
                .unwrap();

            let mut connected = false;
            for _ in 0..400 {
                tokio::time::advance(std::time::Duration::from_secs(1)).await;
                let snap = sup.snapshot().await;
                if snap.iter().any(|s| s.id == id && s.state == IntentState::Connected) {
                    connected = true;
                    break;
                }
            }
            assert!(connected, "intent never reached Connected");
            assert_eq!(
                fetch.seen.get(),
                3,
                "resolver should have been called once per attempt"
            );
        })
        .await;
}
```

Note: `TestNetwork`'s `connect()` looks up the `PeerAddr` directly; the `PeerAddr` produced by the resolver from `address: "ws://bob"` will canonicalize to something `TestNetwork` doesn't know. If this test fails because the canonical `PeerAddr` doesn't match `"bob"`, fall back to a fake-resolver body whose `address` field directly contains the canonical `bob` string the test transport routes on. The exact body shape is dictated by `sunset_relay_resolver::parse_input`'s canonicalizer; consult `crates/sunset-relay-resolver/src/parse.rs` and adjust the body so the resolved canonical equals `PeerAddr::new(Bytes::from_static(b"bob"))`.

- [ ] **Step 3: Write `subscribe_intents_replays_current_state`**

Add:

```rust
#[tokio::test(flavor = "current_thread")]
async fn subscribe_intents_replays_current_state() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let net = TestNetwork::new();
            let alice = engine_with_addr(&net, b"alice", "alice");
            let _bob = engine_with_addr(&net, b"bob", "bob");
            crate::spawn::spawn_local({
                let a = alice.clone();
                async move { a.run().await }
            });
            crate::spawn::spawn_local({
                let b = _bob.clone();
                async move { b.run().await }
            });

            let sup = PeerSupervisor::new(alice.clone(), BackoffPolicy::default());
            crate::spawn::spawn_local({
                let s = sup.clone();
                async move { s.run().await }
            });

            // Register two intents: one that connects, one that won't.
            let bob_addr = PeerAddr::new(Bytes::from_static(b"bob"));
            let ghost = PeerAddr::new(Bytes::from_static(b"ghost"));
            let id_bob = sup
                .add(crate::connectable::Connectable::Direct(bob_addr))
                .await
                .unwrap();
            let id_ghost = sup
                .add(crate::connectable::Connectable::Direct(ghost))
                .await
                .unwrap();

            // Wait until both intents have a non-initial state.
            for _ in 0..50 {
                let snap = sup.snapshot().await;
                let ready = snap.iter().any(|s| s.id == id_bob && s.state == IntentState::Connected)
                    && snap.iter().any(|s| s.id == id_ghost && s.state == IntentState::Backoff);
                if ready {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }

            // Late subscribe — must receive a snapshot for both.
            let mut rx = sup.subscribe_intents().await;
            let mut seen_bob = false;
            let mut seen_ghost = false;
            for _ in 0..2 {
                let snap = tokio::time::timeout(
                    std::time::Duration::from_secs(1),
                    rx.recv(),
                )
                .await
                .expect("subscribe replay should fire promptly")
                .expect("channel should not have closed");
                if snap.id == id_bob {
                    seen_bob = true;
                } else if snap.id == id_ghost {
                    seen_ghost = true;
                }
            }
            assert!(seen_bob, "bob's snapshot was not replayed");
            assert!(seen_ghost, "ghost's snapshot was not replayed");
        })
        .await;
}
```

- [ ] **Step 4: Write `add_returns_immediately_on_resolving`**

Add:

```rust
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn add_returns_immediately_on_resolving() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let net = TestNetwork::new();
            let alice = engine_with_addr(&net, b"alice", "alice");
            crate::spawn::spawn_local({
                let a = alice.clone();
                async move { a.run().await }
            });

            // Always-failing fetcher — first dial would otherwise
            // hang for the resolver fetch indefinitely.
            let fetch = CountingFakeFetch::new(String::new(), usize::MAX);

            let sup = PeerSupervisor::new(alice.clone(), BackoffPolicy::default());
            crate::spawn::spawn_local({
                let s = sup.clone();
                async move { s.run().await }
            });

            let started = tokio::time::Instant::now();
            let _ = sup
                .add(crate::connectable::Connectable::Resolving {
                    input: "relay.example.com".into(),
                    fetch,
                })
                .await
                .unwrap();
            let elapsed = started.elapsed();
            assert!(
                elapsed < std::time::Duration::from_millis(50),
                "add() returned in {elapsed:?}; should be near-instant"
            );
        })
        .await;
}
```

- [ ] **Step 5: Run all supervisor tests**

```bash
nix develop --command cargo test -p sunset-sync supervisor 2>&1 | tail -20
```

Expected: 7 tests pass.

- [ ] **Step 6: Commit**

```bash
git add crates/sunset-sync/src/supervisor.rs
git commit -m "$(cat <<'EOF'
sunset-sync: add supervisor tests for Resolving + replay + add returns immediately

resolving_intent_re_resolves_each_attempt: a CountingFakeFetch fails
twice then succeeds; the supervisor must call the fetcher 3× and end
in Connected — proves we re-resolve per attempt rather than caching
the canonical addr forever.

subscribe_intents_replays_current_state: a late subscriber receives
a snapshot for every existing intent before any new transitions.

add_returns_immediately_on_resolving: with start_paused, add() must
return well under 50 ms even when the underlying resolver would
otherwise block forever — proves the API contract that add() does
not await the first dial.

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>
EOF
)"
```

---

## Task 4: Update `Client::add_relay` + add `Client::on_intent_changed` (Rust + JS bridge)

Surface the new supervisor API to JS. `add_relay` now returns `IntentId` (a JS `Number`); `on_intent_changed` registers a callback that fires per `IntentSnapshot` update.

**Files:**
- Modify: `crates/sunset-web-wasm/src/client.rs`
- Create: `crates/sunset-web-wasm/src/intent.rs` (JS-facing snapshot type)
- Modify: `crates/sunset-web-wasm/src/lib.rs` (module + re-export)
- Modify: `crates/sunset-web-wasm/src/resolver_adapter.rs` (no change in this task; verify the `WebSysFetch` is `Rc`-able)

- [ ] **Step 1: Create `intent.rs` with the JS-facing snapshot type**

```rust
//! JS-facing intent snapshot.

use wasm_bindgen::prelude::*;

use sunset_sync::{IntentSnapshot, IntentState, TransportKind};

/// Maps `sunset_sync::IntentSnapshot` into a JS-friendly object.
/// `IntentId` is `u64` in Rust → `BigInt` in JS via wasm-bindgen, so
/// we narrow to `f64` (safe up to 2^53; the supervisor's monotonic
/// counter never gets near that in any realistic session).
#[wasm_bindgen]
pub struct IntentSnapshotJs {
    pub id: f64,
    #[wasm_bindgen(getter_with_clone)]
    pub state: String,
    #[wasm_bindgen(getter_with_clone)]
    pub label: String,
    #[wasm_bindgen(getter_with_clone)]
    pub peer_pubkey: Option<Vec<u8>>,
    #[wasm_bindgen(getter_with_clone)]
    pub kind: Option<String>,
    pub attempt: u32,
}

impl From<&IntentSnapshot> for IntentSnapshotJs {
    fn from(s: &IntentSnapshot) -> Self {
        Self {
            id: s.id as f64,
            state: match s.state {
                IntentState::Connecting => "connecting",
                IntentState::Connected => "connected",
                IntentState::Backoff => "backoff",
                IntentState::Cancelled => "cancelled",
            }
            .into(),
            label: s.label.clone(),
            peer_pubkey: s
                .peer_id
                .as_ref()
                .map(|p| p.verifying_key().as_bytes().to_vec()),
            kind: s.kind.map(|k| match k {
                TransportKind::Primary => "primary".to_owned(),
                TransportKind::Secondary => "secondary".to_owned(),
            }),
            attempt: s.attempt,
        }
    }
}
```

- [ ] **Step 2: Wire `intent` into the crate**

Edit `crates/sunset-web-wasm/src/lib.rs`. Add:

```rust
mod intent;
```

(in alpha order with the other `mod` declarations).

- [ ] **Step 3: Replace `Client::add_relay` body**

In `crates/sunset-web-wasm/src/client.rs`, replace the existing `add_relay` impl with:

```rust
/// Register a durable intent to keep connected to `url`. Returns
/// the supervisor-assigned `IntentId` once the intent is recorded
/// (one cmd-channel round-trip; does NOT wait for the first
/// connection). The only `Err` is for malformed input.
pub async fn add_relay(&self, url: String) -> Result<f64, JsError> {
    let fetch: std::rc::Rc<dyn sunset_relay_resolver::HttpFetch> =
        std::rc::Rc::new(crate::resolver_adapter::WebSysFetch);
    let connectable = sunset_sync::Connectable::Resolving {
        input: url,
        fetch,
    };
    let id = self
        .supervisor
        .add(connectable)
        .await
        .map_err(|e| JsError::new(&format!("add_relay: {e}")))?;
    Ok(id as f64)
}
```

- [ ] **Step 4: Add `Client::on_intent_changed`**

Append to the same `impl Client` block:

```rust
/// Register a JS callback that fires:
///   * once per existing intent, immediately on register, and
///   * once per intent state transition thereafter.
/// The callback receives an `IntentSnapshotJs`.
pub fn on_intent_changed(&self, callback: js_sys::Function) {
    let supervisor = self.supervisor.clone();
    wasm_bindgen_futures::spawn_local(async move {
        let mut rx = supervisor.subscribe_intents().await;
        while let Some(snap) = rx.recv().await {
            let js_snap = crate::intent::IntentSnapshotJs::from(&snap);
            let _ = callback.call1(&JsValue::NULL, &JsValue::from(js_snap));
        }
    });
}
```

- [ ] **Step 5: Add `Client::intents` synchronous accessor**

Append:

```rust
/// Synchronous snapshot of every registered intent. JS array of
/// `IntentSnapshotJs`. Used by the frontend on first paint, before
/// the `on_intent_changed` callback's replay arrives.
pub async fn intents(&self) -> Vec<crate::intent::IntentSnapshotJs> {
    self.supervisor
        .snapshot()
        .await
        .iter()
        .map(crate::intent::IntentSnapshotJs::from)
        .collect()
}
```

- [ ] **Step 6: Build and test the wasm crate**

```bash
nix develop --command cargo test -p sunset-web-wasm 2>&1 | tail -10
nix develop --command cargo build -p sunset-web-wasm --target wasm32-unknown-unknown 2>&1 | tail -10
```

Expected: clean.

- [ ] **Step 7: Run clippy on the workspace + wasm target**

```bash
nix develop --command cargo clippy --workspace --all-features --all-targets -- -D warnings 2>&1 | tail -10
nix develop --command cargo clippy -p sunset-web-wasm --target wasm32-unknown-unknown -- -D warnings 2>&1 | tail -10
```

Expected: clean.

- [ ] **Step 8: Commit**

```bash
git add crates/sunset-web-wasm
git commit -m "$(cat <<'EOF'
sunset-web-wasm: add_relay returns IntentId; expose on_intent_changed

Client::add_relay no longer awaits the first connect — it returns
the supervisor's IntentId after the cmd-channel round-trip.
Client::on_intent_changed registers a JS callback fed by
PeerSupervisor::subscribe_intents() (replay-on-subscribe + live
updates). Client::intents() returns a sync snapshot for first paint.

The new IntentSnapshotJs wasm-bindgen struct narrows IntentId u64 →
f64 (JS Number; safe up to 2^53). state / kind become strings;
peer_pubkey is Option<Vec<u8>>.

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>
EOF
)"
```

---

## Task 5: Remove `Client::relay_status` field, `Client::on_relay_status_changed`, and the membership-tracker relay-status helpers

The supervisor's intent stream is now the source of truth. Retire the duplicate/sticky surface.

**Files:**
- Modify: `crates/sunset-web-wasm/src/client.rs`
- Modify: `crates/sunset-web-wasm/src/lib.rs`
- Modify: `crates/sunset-core/src/membership.rs`

- [ ] **Step 1: Remove from `Client` struct definition**

Open `crates/sunset-web-wasm/src/client.rs`. In the `pub struct Client { ... }` definition, remove the `relay_status: Rc<RefCell<String>>` field. Remove its initializer in `Client::new`.

- [ ] **Step 2: Remove `Client::relay_status()` getter and `on_relay_status_changed`**

In the same file, delete:

```rust
#[wasm_bindgen(getter)]
pub fn relay_status(&self) -> String { ... }

pub fn on_relay_status_changed(&self, callback: js_sys::Function) { ... }
```

- [ ] **Step 3: Remove the sticky-status writes in `add_relay`**

Already absent after Task 4 (the new `add_relay` doesn't touch `relay_status`).

- [ ] **Step 4: Remove `derive_relay_status` / `maybe_fire_relay_status` / `fire_relay_status_now` from `sunset-core::membership`**

Open `crates/sunset-core/src/membership.rs`. Delete:

- `fn derive_relay_status(...)` (≈ line 406)
- `fn maybe_fire_relay_status(...)` (≈ line 420)
- `pub fn fire_relay_status_now(...)` (≈ line 368)
- The `on_relay_status: RelayStatusCallbackSlot` field on `TrackerHandles` (≈ line 221)
- The `last_relay_status: Rc<RefCell<String>>` field on `TrackerHandles` (≈ line 222)
- Both type aliases at line 118 (`RelayStatusCallback`) and line 126 (`RelayStatusCallbackSlot`)

Update `TrackerHandles::new` to drop the removed fields. The new shape:

```rust
impl TrackerHandles {
    pub fn new() -> Self {
        Self {
            on_members: Rc::new(RefCell::new(None)),
            peer_kinds: Rc::new(RefCell::new(HashMap::new())),
            last_signature: Rc::new(RefCell::new(Vec::new())),
        }
    }
}
```

The `&str` `initial_relay_status` parameter on `TrackerHandles::new` goes away — callers passed `"disconnected"`, which had no real effect once the sticky branches retired.

- [ ] **Step 5: Update `spawn_tracker` and `handle_engine_event` callsites**

In `crates/sunset-core/src/membership.rs`, search for `maybe_fire_relay_status(` and remove every call. Search for `last_relay_status` and remove every reference. Search for `on_relay_status` and remove every reference.

- [ ] **Step 6: Remove cross-crate callsites**

In `crates/sunset-web-wasm/src/client.rs`:

- Change `Rc::new(TrackerHandles::new("disconnected"))` (≈ line 145) to `Rc::new(TrackerHandles::new())`.
- Delete the `sunset_core::membership::fire_relay_status_now(&self.tracker_handles);` call inside `start_presence` (≈ line 281).

- [ ] **Step 7: Build + test the workspace**

```bash
nix develop --command cargo build --workspace --all-features 2>&1 | tail -10
nix develop --command cargo test --workspace --all-features 2>&1 | grep -E "^test result|FAILED|error\[" | tail -30
```

Expected: clean build, all tests pass.

- [ ] **Step 8: Run clippy**

```bash
nix develop --command cargo clippy --workspace --all-features --all-targets -- -D warnings 2>&1 | tail -10
```

Expected: clean.

- [ ] **Step 9: Commit**

```bash
git add crates/sunset-web-wasm crates/sunset-core
git commit -m "$(cat <<'EOF'
core+web-wasm: retire relay_status string + membership relay-status helpers

The supervisor's intent stream is the single source of truth for
relay connection state. Remove:
  * Client::relay_status field and getter
  * Client::on_relay_status_changed
  * sunset-core::membership::derive_relay_status,
    maybe_fire_relay_status, fire_relay_status_now
  * TrackerHandles::on_relay_status, last_relay_status

Callers used the sticky `"connecting"`/`"error"` strings to bridge
the gap before add_relay's first connect landed. With the supervisor
durable and exposing IntentSnapshot directly, the bridge is gone.

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>
EOF
)"
```

---

## Task 6: Gleam frontend — replace `RelayConnectResult` + `RelayStatusUpdated` with `IntentChanged`

**Files:**
- Modify: `web/src/sunset_web/sunset.gleam`
- Modify: `web/src/sunset_web/sunset.ffi.mjs`
- Modify: `web/src/sunset_web.gleam`

- [ ] **Step 1: Update the FFI types in `sunset.gleam`**

Open `web/src/sunset_web/sunset.gleam`. Replace the existing `add_relay` external + the `delay_ms` external with:

```gleam
/// Register a durable intent to keep connected to `url`. The
/// callback is fired with `Ok(intent_id)` once the intent is
/// recorded; `Error(msg)` is reserved for malformed input.
@external(javascript, "./sunset.ffi.mjs", "addRelay")
pub fn add_relay(
  client: ClientHandle,
  url: String,
  callback: fn(Result(Float, String)) -> Nil,
) -> Nil

/// Snapshot of one supervisor intent, mirrored from
/// `IntentSnapshotJs`. `kind` is `"primary"` / `"secondary"` / not
/// present.
pub type IntentSnapshot {
  IntentSnapshot(
    id: Float,
    state: String,
    label: String,
    peer_pubkey: option.Option(BitArray),
    kind: option.Option(String),
    attempt: Int,
  )
}

/// Register a callback fired for every intent (once on register,
/// then once per state transition).
@external(javascript, "./sunset.ffi.mjs", "onIntentChanged")
pub fn on_intent_changed(
  client: ClientHandle,
  callback: fn(IntentSnapshot) -> Nil,
) -> Nil
```

Remove the `delay_ms` external entirely.

Remove the `relay_status` external (the sync getter) — `Client::relay_status` no longer exists.

Remove the `on_relay_status_changed` external if it's still in this file — search for it and delete.

- [ ] **Step 2: Update `sunset.ffi.mjs`**

Open `web/src/sunset_web/sunset.ffi.mjs`. Replace `addRelay`:

```js
export async function addRelay(client, url, callback) {
  try {
    const id = await client.add_relay(url);
    callback(new Ok(id));
  } catch (e) {
    callback(new GError(String(e)));
  }
}
```

Add `onIntentChanged`:

```js
export function onIntentChanged(client, callback) {
  client.on_intent_changed((snap) => {
    // Copy the wasm-bindgen object's fields into a plain JS object
    // so it survives the wasm side dropping the wrapper. `kind` and
    // `peer_pubkey` arrive as undefined when None on the Rust side;
    // map both to gleam `Option(None)` shape (a tagged tuple).
    const plain = {
      id: snap.id,
      state: snap.state,
      label: snap.label,
      peer_pubkey: snap.peer_pubkey,
      kind: snap.kind,
      attempt: snap.attempt,
    };
    callback(plain);
  });
}
```

Remove `delayMs` entirely.

Remove `relayStatus` and `onRelayStatusChanged` if they exist in this file.

- [ ] **Step 3: Update Lustre Msg type and Model**

Open `web/src/sunset_web.gleam`. In the `Msg` block:

- Delete `RelayConnectResult(url: String, result: Result(Nil, String))`.
- Delete `RelayStatusUpdated(String)` if present.
- Add `IntentChanged(snap: sunset.IntentSnapshot)`.

In the `Model` block:

- Delete the `relay_status: String` field.
- Add `intents: dict.Dict(Float, sunset.IntentSnapshot)`.
- Add `published: Bool` (latch for `publish_room_subscription`).

Update `init` (search for `relay_status: "disconnected"`) — replace that line with:

```gleam
intents: dict.new(),
published: False,
```

- [ ] **Step 4: Replace the `RelayConnectResult` / `RelayStatusUpdated` handlers**

In the `update` fn, find the two cases:

```gleam
RelayConnectResult(_url, Ok(_)) -> ...
RelayConnectResult(url, Error(_)) -> ...
RelayStatusUpdated(s) -> ...
```

Delete all three. Replace with:

```gleam
IntentChanged(snap) -> {
  let new_intents = dict.insert(model.intents, snap.id, snap)
  let any_connected =
    list.any(dict.values(new_intents), fn(s) { s.state == "connected" })
  case any_connected, model.published, model.client {
    True, False, Some(client) -> {
      let pub_eff =
        effect.from(fn(dispatch) {
          sunset.publish_room_subscription(client, fn(r) {
            dispatch(SubscribePublishResult(r))
          })
        })
      #(
        Model(..model, intents: new_intents, published: True),
        pub_eff,
      )
    }
    _, _, _ -> #(Model(..model, intents: new_intents), effect.none())
  }
}
```

- [ ] **Step 5: Wire `on_intent_changed` in `ClientReady`**

In the `ClientReady` handler, replace the existing connect_eff (which calls `sunset.add_relay(client, url, fn(r) { dispatch(RelayConnectResult(url, r)) })`) with:

```gleam
let intent_eff =
  effect.from(fn(dispatch) {
    sunset.on_intent_changed(client, fn(snap) {
      dispatch(IntentChanged(snap))
    })
  })
let connect_eff =
  effect.from(fn(_dispatch) {
    list.each(relays, fn(url) {
      // Errors here are JS-side malformed-URL issues; ignore (the
      // resolver+supervisor handle every transient case).
      sunset.add_relay(client, url, fn(_r) { Nil })
    })
  })
```

Add `intent_eff` to the `effect.batch([...])` list. Remove the `on_relay_status_changed` registration if it's still there.

The model line (currently `Model(..model, client: Some(client), relay_status: new_status)`) becomes:

```gleam
#(
  Model(..model, client: Some(client)),
  effect.batch([on_receipt_eff, on_msg_eff, presence_eff, intent_eff, connect_eff]),
)
```

- [ ] **Step 6: Replace `relay_status_to_conn` with `relay_status_pill` and rethread the callers**

Replace the existing helper near the bottom of `web/src/sunset_web.gleam`:

```gleam
fn relay_status_to_conn(relay_status: String) -> domain.ConnStatus {
  case relay_status {
    "connected" -> domain.Connected
    "connecting" -> domain.Reconnecting
    "reconnecting" -> domain.Reconnecting
    "error" -> domain.Offline
    "disconnected" -> domain.Offline
    _ -> domain.Connected
  }
}
```

with:

```gleam
pub fn relay_status_pill(
  intents: dict.Dict(Float, sunset.IntentSnapshot),
) -> domain.ConnStatus {
  let snaps = dict.values(intents)
  case list.any(snaps, fn(s) { s.state == "connected" }) {
    True -> domain.Connected
    False ->
      case
        list.any(snaps, fn(s) {
          s.state == "connecting" || s.state == "backoff"
        })
      {
        True -> domain.Reconnecting
        False -> domain.Offline
      }
  }
}
```

(Make it `pub fn` for the unit test in Step 7.)

Update the three helpers that ferried the old `relay_status: String`:

`resolve_rooms` (≈ line 1296) — change the parameter:

```gleam
fn resolve_rooms(
  names: List(String),
  intents: dict.Dict(Float, sunset.IntentSnapshot),
) -> List(Room) {
  let fixture_rooms = fixture.rooms()
  let conn = relay_status_pill(intents)
  list.map(names, fn(name) {
    case list.find(fixture_rooms, fn(r) { r.name == name }) {
      Ok(r) -> Room(..r, status: conn, id: RoomId(name))
      Error(_) -> synthetic_room(name, intents)
    }
  })
}
```

`lookup_room` (≈ line 1307):

```gleam
fn lookup_room(
  rs: List(Room),
  name: String,
  intents: dict.Dict(Float, sunset.IntentSnapshot),
) -> Room {
  case list.find(rs, fn(r) { r.name == name }) {
    Ok(r) -> r
    Error(_) -> synthetic_room(name, intents)
  }
}
```

`synthetic_room` (≈ line 1316):

```gleam
fn synthetic_room(
  name: String,
  intents: dict.Dict(Float, sunset.IntentSnapshot),
) -> Room {
  Room(
    id: RoomId(name),
    name: name,
    members: 1,
    online: 1,
    in_call: 0,
    status: relay_status_pill(intents),
    last_active: "now",
    unread: 0,
    bridge: NoBridge,
  )
}
```

Update the two callsites in `view`:

```gleam
// ≈ line 963
let displayed_rooms = resolve_rooms(model.joined_rooms, model.intents)
// ≈ line 966
lookup_room(displayed_rooms, current_name, model.intents)
```

- [ ] **Step 7: Make `relay_status_pill` `pub fn` and add unit tests**

The latch + `update` flow is hard to assert in gleam without
elaborate plumbing (Lustre's `Effect` doesn't expose its closure for
inspection). Cover the pure derivation function instead — the latch
behavior is exercised end-to-end by `relay_deploy.spec.js`.

In `web/src/sunset_web.gleam`, change `fn relay_status_pill(...)` to
`pub fn relay_status_pill(...)`.

Create `web/test/relay_status_pill_test.gleam`:

```gleam
import gleam/dict
import gleam/option.{None}
import gleeunit/should
import sunset_web
import sunset_web/sunset.{IntentSnapshot}
import sunset_web/views/domain

fn snap(id: Float, state: String) -> IntentSnapshot {
  IntentSnapshot(
    id: id,
    state: state,
    label: "test",
    peer_pubkey: None,
    kind: None,
    attempt: 0,
  )
}

pub fn empty_dict_is_offline_test() {
  sunset_web.relay_status_pill(dict.new())
  |> should.equal(domain.Offline)
}

pub fn any_connected_wins_test() {
  let intents =
    dict.new()
    |> dict.insert(1.0, snap(1.0, "backoff"))
    |> dict.insert(2.0, snap(2.0, "connected"))
  sunset_web.relay_status_pill(intents)
  |> should.equal(domain.Connected)
}

pub fn connecting_or_backoff_is_reconnecting_test() {
  let intents =
    dict.new()
    |> dict.insert(1.0, snap(1.0, "connecting"))
  sunset_web.relay_status_pill(intents)
  |> should.equal(domain.Reconnecting)

  let intents =
    dict.new()
    |> dict.insert(1.0, snap(1.0, "backoff"))
  sunset_web.relay_status_pill(intents)
  |> should.equal(domain.Reconnecting)
}

pub fn cancelled_only_is_offline_test() {
  let intents =
    dict.new()
    |> dict.insert(1.0, snap(1.0, "cancelled"))
  sunset_web.relay_status_pill(intents)
  |> should.equal(domain.Offline)
}
```

Adjust the `views/domain` import path if it differs in the actual
codebase (look at how other tests import `domain.ConnStatus`).

Add a second test file `web/test/publish_latch_test.gleam` that
exercises the publish-once latch via the public `update` function:

```gleam
import gleam/dict
import gleam/option.{None}
import gleeunit/should
import sunset_web
import sunset_web/sunset.{IntentSnapshot}

fn snap(id: Float, state: String) -> IntentSnapshot {
  IntentSnapshot(
    id: id,
    state: state,
    label: "test",
    peer_pubkey: None,
    kind: None,
    attempt: 0,
  )
}

pub fn publish_latch_flips_on_first_connected_test() {
  // Construct a minimal Model with the relevant fields. Reuse
  // sunset_web.init or whatever helper exists; otherwise build a
  // Model manually with `..test_model_default()` semantics. The
  // assertion is on `published: Bool` only.
  let m0 = sunset_web.test_model_with_no_client()
  let #(m1, _eff1) =
    sunset_web.update(m0, sunset_web.IntentChanged(snap(1.0, "connected")))
  m1.published |> should.equal(False) // no client → no publish

  let m0c = sunset_web.test_model_with_client_stub()
  let #(m1c, _eff1c) =
    sunset_web.update(m0c, sunset_web.IntentChanged(snap(1.0, "connected")))
  m1c.published |> should.equal(True)

  let #(m2c, _eff2c) =
    sunset_web.update(m1c, sunset_web.IntentChanged(snap(2.0, "connected")))
  m2c.published |> should.equal(True) // still True; not re-flipped
}
```

`test_model_with_no_client()` and `test_model_with_client_stub()`
are helpers you'll add to `sunset_web.gleam` as `pub fn` for tests.
Their bodies construct a `Model` with sensible defaults — copy the
shape from `init`'s return value, leave the fields you don't care
about at their fixture defaults.

- [ ] **Step 8: Run gleam tests**

```bash
nix develop --command bash -c 'cd web && gleam format --check src test'
nix develop --command bash -c 'cd web && gleam test'
```

Expected: format clean, all gleam unit tests pass (incl. the three
new ones).

- [ ] **Step 9: Commit**

```bash
git add web
git commit -m "$(cat <<'EOF'
web: replace RelayConnectResult/RelayStatusUpdated with IntentChanged

The frontend now subscribes to the supervisor's intent stream
through Client::on_intent_changed. Status flows in via
IntentChanged(snap) and the room-status pill is derived in one
helper (relay_status_pill) from `dict.values(intents)`.

Removed:
  * RelayConnectResult Msg variant (Ok + Err handlers)
  * RelayStatusUpdated Msg variant
  * relay_status: String model field, replaced by intents Dict
  * relay_status_to_conn string-pattern helper
  * delay_ms FFI binding + JS function (PR #18 retry effect)
  * on_relay_status_changed FFI binding

Added:
  * IntentChanged(IntentSnapshot) Msg variant
  * intents: Dict(Float, IntentSnapshot) model field
  * published: Bool latch (publish_room_subscription fires once on
    first transition to any-intent-Connected)
  * relay_status_pill(intents) -> domain.ConnStatus helper

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>
EOF
)"
```

---

## Task 7: Tighten the e2e test wait + run the full Playwright suite

The 15 s wait in `relay_deploy.spec.js` was a hack around the gleam-side 2 s retry cadence. With the supervisor's first dial firing as soon as the port comes back, the wait can shrink.

**Files:**
- Modify: `web/e2e/relay_deploy.spec.js`

- [ ] **Step 1: Replace the fixed 15 s wait with a status-driven wait**

Open `web/e2e/relay_deploy.spec.js`. Find:

```js
await new Promise((r) => setTimeout(r, 15_000));
```

Replace with a poll loop that watches `window.sunsetClient.intents()` for both clients to report any intent in `"connected"` state:

```js
async function waitForConnected(page) {
  await page.waitForFunction(
    () =>
      window.sunsetClient &&
      window.sunsetClient.intents &&
      // intents() is async; the polled function returns a Promise,
      // which page.waitForFunction awaits as truthy.
      window.sunsetClient
        .intents()
        .then((arr) => arr.some((s) => s.state === "connected")),
    { timeout: 30_000, polling: 250 },
  );
}

await waitForConnected(pageA);
await waitForConnected(pageB);
```

Note: `window.sunsetClient` is set by `sunset.ffi.mjs::createClient` when `window.SUNSET_TEST` is true. The test fixture must set `SUNSET_TEST` before navigation; if it isn't already, set it via `await page.addInitScript(() => { window.SUNSET_TEST = true; })` before `page.goto(url)`.

- [ ] **Step 2: Run only this spec to confirm**

```bash
SUNSET_WEB_PORT=4773 nix run .#web-test -- --grep "client recovers when initial add_relay" --project chromium 2>&1 | tail -10
```

Expected: pass, runtime under 25 s.

- [ ] **Step 3: Run the full Playwright suite to check for regressions**

```bash
SUNSET_WEB_PORT=4773 nix run .#web-test -- --project chromium 2>&1 | tail -15
```

Expected: all green (`relay_restart.spec.js`, `kill_relay.spec.js`, `two_browser_chat.spec.js`, etc.).

- [ ] **Step 4: Final fmt + clippy + workspace tests**

```bash
nix develop --command cargo fmt --all --check 2>&1 | tail -5
nix develop --command bash -c 'cd web && gleam format --check src test' 2>&1 | tail -5
./scripts/check-no-clippy-allow.sh
nix develop --command cargo clippy --workspace --all-features --all-targets -- -D warnings 2>&1 | tail -5
nix develop --command cargo test --workspace --all-features 2>&1 | grep -E "^test result|FAILED|error\[" | tail -10
```

Expected: every check clean, every test green.

- [ ] **Step 5: Commit**

```bash
git add web/e2e/relay_deploy.spec.js
git commit -m "$(cat <<'EOF'
e2e: tighten relay_deploy wait — poll intents() instead of fixed 15 s

The previous 15 s sleep was a hack around the gleam-side 2 s retry
cadence (PR #18). With the supervisor driving the dial as soon as
the port comes back, we can drop straight into a 250 ms poll loop on
window.sunsetClient.intents() looking for any intent in "connected"
state. Test runtime drops accordingly.

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>
EOF
)"
```

---

## Task 8: Open the PR

- [ ] **Step 1: Push the branch and open the PR**

```bash
git push -u origin spec/durable-relay-connect 2>&1 | tail -3
```

Then:

```bash
gh pr create --title "sunset-sync: durable supervisor on Connectable + IntentId; retire relay_status" --body "$(cat <<'EOF'
## Summary

- Push the relay reconnect/recovery loop from the gleam frontend (PR #18 band-aid) into `sunset-sync::PeerSupervisor`. All host clients (web today, future TUI/mod) get the same retry behavior for free.
- The supervisor's `add()` is now durable (no first-dial-failure cleanup), takes a `Connectable` enum (`Direct(PeerAddr)` or `Resolving { input, fetch }`), and runs the resolver inside the dial loop — so a relay rotating identity between deploys is picked up per attempt.
- Retire the special `relay_status` string. `IntentSnapshot` (extended with `id`, `kind`, `label`) flows through the same peer-status surface used elsewhere; the gleam frontend trades `RelayConnectResult` + `RelayStatusUpdated` for `IntentChanged(snap)` + a single `relay_status_pill` derivation.

## Spec

`docs/superpowers/specs/2026-05-03-durable-relay-connect-design.md`

## Test plan

- [x] `cargo fmt --all --check`
- [x] `gleam format --check`
- [x] `./scripts/check-no-clippy-allow.sh`
- [x] `cargo clippy --workspace --all-features --all-targets -- -D warnings`
- [x] Cargo workspace tests (incl. new supervisor tests for Connectable, durability, replay-on-subscribe, add-returns-immediately)
- [x] Full Playwright e2e suite, including `relay_deploy.spec.js` (with the 15 s hack replaced by an `intents()` poll) and `relay_restart.spec.js`

🤖 Generated with [Claude Code](https://claude.com/claude-code)
EOF
)"
```
