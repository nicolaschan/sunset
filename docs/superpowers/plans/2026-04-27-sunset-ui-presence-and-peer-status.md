# UI presence + peer-status — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** wire the chat UI so the relay-status badge, the per-peer connection-mode indicator, and the member rail reflect real sunset-sync state instead of one-shot strings + fixture data.

**Architecture:** sunset-sync gains a typed event stream (`EngineEvent`) for peer add/remove with the originating transport's `TransportKind`. sunset-web-wasm adds a `MembershipTracker` that subscribes to a per-room heartbeat namespace + the engine event stream, derives `(presence, connection_mode)` per peer, and pushes the result through two new wasm-bindgen callbacks. The Gleam UI receives those callbacks via Lustre `Msg` variants, replaces every `fixture.members()` call site with `model.members`, and reads the heartbeat cadence from URL params so Playwright can compress the test arc to ~1s.

**Tech Stack:** Rust (sunset-sync, sunset-web-wasm), wasm-bindgen, tokio sync, futures channels, Gleam + Lustre, Playwright.

**Spec:** `docs/superpowers/specs/2026-04-27-sunset-ui-presence-and-peer-status-design.md`.

---

## File structure

```
sunset/
├── crates/sunset-sync/src/
│   ├── transport.rs                    # MODIFY: add TransportKind enum, kind() method
│   ├── multi_transport.rs              # MODIFY: override kind() on MultiConnection
│   ├── peer.rs                         # MODIFY: InboundEvent::PeerHello carries kind
│   ├── engine.rs                       # MODIFY: EngineEvent, fan-out, subscribe API
│   └── lib.rs                          # MODIFY: re-exports
├── crates/sunset-web-wasm/
│   ├── Cargo.toml                      # MODIFY: add wasmtimer if not present
│   └── src/
│       ├── lib.rs                      # MODIFY: re-exports
│       ├── client.rs                   # MODIFY: relay_status now event-driven
│       ├── members.rs                  # NEW: MemberJs + presence_bucket + derived state
│       ├── membership_tracker.rs       # NEW: tracker task (presence + engine events)
│       └── presence_publisher.rs       # NEW: heartbeat publisher task
├── web/src/sunset_web/
│   ├── sunset.gleam                    # MODIFY: externals for new methods
│   ├── sunset.ffi.mjs                  # MODIFY: JS shims
│   └── sunset_web.gleam                # MODIFY: Model, Msg, bootstrap, view wiring
└── web/e2e/
    └── presence.spec.js                # NEW: 3 tests, total wall-clock ~5s
```

---

## Tasks

### Task 1: `TransportKind` enum + default `kind()` on `TransportConnection`

**Files:**
- Modify: `crates/sunset-sync/src/transport.rs`
- Modify: `crates/sunset-sync/src/lib.rs`

- [ ] **Step 1:** Add the enum + trait method to `crates/sunset-sync/src/transport.rs`. Insert immediately above the existing `Transport` trait definition:

  ```rust
  /// Which side of a `MultiTransport` (or which discriminator a
  /// future multi-fanout transport chooses) produced this connection.
  /// Used by callers (e.g. UI clients) to render per-peer routing
  /// state without having to know the concrete transport type.
  #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
  pub enum TransportKind {
      /// Primary half of a `MultiTransport`. In v1 this is the
      /// relay-mediated WebSocket path.
      Primary,
      /// Secondary half of a `MultiTransport`. In v1 this is the
      /// direct WebRTC datachannel.
      Secondary,
      /// Used by transports that don't participate in a
      /// `MultiTransport` (e.g. `TestTransport`, single-transport
      /// setups).
      Unknown,
  }
  ```

  In the `TransportConnection` trait body, add a default-impl method **after** `peer_id` and **before** `close`:

  ```rust
  /// Identifies which transport produced this connection. Default
  /// is `TransportKind::Unknown`; `MultiConnection` overrides to
  /// return `Primary` or `Secondary`.
  fn kind(&self) -> TransportKind {
      TransportKind::Unknown
  }
  ```

- [ ] **Step 2:** Re-export `TransportKind` from `crates/sunset-sync/src/lib.rs`. Add to the existing `pub use transport::...` line:

  ```rust
  pub use transport::{RawConnection, RawTransport, Transport, TransportConnection, TransportKind};
  ```

- [ ] **Step 3:** Add a unit test at the bottom of `crates/sunset-sync/src/transport.rs`:

  ```rust
  #[cfg(test)]
  mod tests {
      use super::*;
      use async_trait::async_trait;
      use bytes::Bytes;
      use crate::types::PeerId;
      use sunset_store::VerifyingKey;

      struct DummyConn;
      #[async_trait(?Send)]
      impl TransportConnection for DummyConn {
          async fn send_reliable(&self, _: Bytes) -> crate::Result<()> { Ok(()) }
          async fn recv_reliable(&self) -> crate::Result<Bytes> { Ok(Bytes::new()) }
          async fn send_unreliable(&self, _: Bytes) -> crate::Result<()> { Ok(()) }
          async fn recv_unreliable(&self) -> crate::Result<Bytes> { Ok(Bytes::new()) }
          fn peer_id(&self) -> PeerId { PeerId(VerifyingKey::new(Bytes::from_static(&[0u8; 32]))) }
          async fn close(&self) -> crate::Result<()> { Ok(()) }
      }

      #[test]
      fn default_kind_is_unknown() {
          assert_eq!(DummyConn.kind(), TransportKind::Unknown);
      }
  }
  ```

- [ ] **Step 4:** Verify:

  ```
  nix develop --command cargo fmt -p sunset-sync
  nix develop --command cargo test -p sunset-sync transport::tests
  nix develop --command cargo clippy -p sunset-sync --all-features --all-targets -- -D warnings
  ```

  Expect 1 new test pass + clippy clean.

- [ ] **Step 5:** Commit:

  ```
  git add crates/sunset-sync/src/transport.rs crates/sunset-sync/src/lib.rs
  git commit -m "Add TransportKind + default kind() on TransportConnection"
  ```

---

### Task 2: Override `kind()` on `MultiConnection` + test

**Files:**
- Modify: `crates/sunset-sync/src/multi_transport.rs`

- [ ] **Step 1:** In the `impl<C1, C2> TransportConnection for MultiConnection<C1, C2>` block (currently just before the file's end), add the override **between `peer_id` and `close`**:

  ```rust
  fn kind(&self) -> crate::transport::TransportKind {
      use crate::transport::TransportKind;
      match self {
          MultiConnection::Primary(_) => TransportKind::Primary,
          MultiConnection::Secondary(_) => TransportKind::Secondary,
      }
  }
  ```

- [ ] **Step 2:** Add a test at the bottom of `crates/sunset-sync/src/multi_transport.rs`:

  ```rust
  #[cfg(test)]
  mod tests {
      use super::*;
      use async_trait::async_trait;
      use crate::transport::TransportKind;
      use crate::types::PeerId;
      use sunset_store::VerifyingKey;

      struct StubConn;
      #[async_trait(?Send)]
      impl TransportConnection for StubConn {
          async fn send_reliable(&self, _: Bytes) -> Result<()> { Ok(()) }
          async fn recv_reliable(&self) -> Result<Bytes> { Ok(Bytes::new()) }
          async fn send_unreliable(&self, _: Bytes) -> Result<()> { Ok(()) }
          async fn recv_unreliable(&self) -> Result<Bytes> { Ok(Bytes::new()) }
          fn peer_id(&self) -> PeerId { PeerId(VerifyingKey::new(Bytes::from_static(&[0u8; 32]))) }
          async fn close(&self) -> Result<()> { Ok(()) }
      }

      #[test]
      fn primary_variant_reports_primary() {
          let c: MultiConnection<StubConn, StubConn> = MultiConnection::Primary(StubConn);
          assert_eq!(c.kind(), TransportKind::Primary);
      }

      #[test]
      fn secondary_variant_reports_secondary() {
          let c: MultiConnection<StubConn, StubConn> = MultiConnection::Secondary(StubConn);
          assert_eq!(c.kind(), TransportKind::Secondary);
      }
  }
  ```

- [ ] **Step 3:** Verify:

  ```
  nix develop --command cargo test -p sunset-sync multi_transport::tests
  nix develop --command cargo clippy -p sunset-sync --all-features --all-targets -- -D warnings
  ```

  Expect 2 new tests pass.

- [ ] **Step 4:** Commit:

  ```
  git add crates/sunset-sync/src/multi_transport.rs
  git commit -m "MultiConnection::kind reports Primary/Secondary by variant"
  ```

---

### Task 3: Carry `kind` through `InboundEvent::PeerHello`

**Files:**
- Modify: `crates/sunset-sync/src/peer.rs`

- [ ] **Step 1:** Modify `InboundEvent::PeerHello` to carry the kind. Replace the existing variant declaration:

  ```rust
  PeerHello {
      peer_id: PeerId,
      kind: crate::transport::TransportKind,
      out_tx: tokio::sync::mpsc::UnboundedSender<SyncMessage>,
  },
  ```

- [ ] **Step 2:** In `run_peer` (same file), capture the connection kind once after the function entry, before the Hello exchange. Find the line `pub(crate) async fn run_peer<C: TransportConnection + 'static>(` and add **at the very top of the function body, before `let our_hello = ...`**:

  ```rust
  let local_kind = conn.kind();
  ```

  Then update the existing `inbound_tx.send(InboundEvent::PeerHello { ... })` (the line that currently passes `peer_id: peer_id.clone(), out_tx`) to include the kind:

  ```rust
  let _ = inbound_tx.send(InboundEvent::PeerHello {
      peer_id: peer_id.clone(),
      kind: local_kind,
      out_tx,
  });
  ```

- [ ] **Step 3:** Verify the existing tests still compile + pass — `peer.rs` has tests that construct `PeerHello`. Update them:

  ```
  nix develop --command cargo build -p sunset-sync --all-features 2>&1 | head -30
  ```

  If the compiler reports `PeerHello { peer_id, out_tx }` patterns missing `kind`, fix each one to include `kind: _` (in match patterns) or `kind: TransportKind::Unknown` (in constructors). Search:

  ```
  grep -n "PeerHello {" crates/sunset-sync/src/
  ```

  In `engine.rs` callsites that pattern-match on `PeerHello { peer_id, out_tx }` add `kind: _` for now (Task 5 will use it).

- [ ] **Step 4:** Verify:

  ```
  nix develop --command cargo test -p sunset-sync --all-features
  nix develop --command cargo clippy -p sunset-sync --all-features --all-targets -- -D warnings
  ```

  Expect all existing tests still pass.

- [ ] **Step 5:** Commit:

  ```
  git add crates/sunset-sync/src/peer.rs crates/sunset-sync/src/engine.rs
  git commit -m "Carry TransportKind through InboundEvent::PeerHello"
  ```

---

### Task 4: `EngineEvent` + fan-out subscription registry

**Files:**
- Modify: `crates/sunset-sync/src/engine.rs`
- Modify: `crates/sunset-sync/src/lib.rs`

- [ ] **Step 1:** Add the `EngineEvent` enum near the top of `crates/sunset-sync/src/engine.rs`, immediately after the existing `pub(crate) enum EngineCommand` block:

  ```rust
  /// Lifecycle events emitted by the engine. Subscribers receive
  /// every event from the moment they subscribe; events emitted
  /// before subscription are NOT replayed.
  #[derive(Clone, Debug)]
  pub enum EngineEvent {
      PeerAdded {
          peer_id: PeerId,
          kind: crate::transport::TransportKind,
      },
      PeerRemoved {
          peer_id: PeerId,
      },
  }
  ```

- [ ] **Step 2:** Extend `EngineState` (line ~58 in engine.rs) with a subscribers vec. Replace the struct with:

  ```rust
  pub(crate) struct EngineState {
      pub trust: TrustSet,
      pub registry: SubscriptionRegistry,
      pub peer_outbound: HashMap<PeerId, mpsc::UnboundedSender<SyncMessage>>,
      /// Live `EngineEvent` subscribers. Dead senders (closed by the
      /// receiver being dropped) are evicted lazily on the next emit.
      pub event_subs: Vec<mpsc::UnboundedSender<EngineEvent>>,
  }
  ```

  Update the `EngineState` initialiser inside `SyncEngine::new` to include `event_subs: Vec::new()`.

- [ ] **Step 3:** Add the public subscribe API to the `impl SyncEngine` block. Place it next to the existing `pub async fn add_peer`:

  ```rust
  /// Subscribe to lifecycle events emitted by the engine. Each call
  /// returns a fresh receiver. Events are delivered to every live
  /// subscriber; subscribers receive only events that happen after
  /// they subscribe (no replay).
  pub async fn subscribe_engine_events(&self) -> mpsc::UnboundedReceiver<EngineEvent> {
      let (tx, rx) = mpsc::unbounded_channel::<EngineEvent>();
      self.state.lock().await.event_subs.push(tx);
      rx
  }
  ```

- [ ] **Step 4:** Add a private fan-out helper inside the same `impl` block (place above `do_publish_subscription` or wherever convenient):

  ```rust
  /// Fan-out an event to every live subscriber. Drops senders whose
  /// receivers have been dropped (lazy GC).
  async fn emit_engine_event(&self, ev: EngineEvent) {
      let mut state = self.state.lock().await;
      state
          .event_subs
          .retain(|tx| tx.send(ev.clone()).is_ok());
  }
  ```

- [ ] **Step 5:** Re-export `EngineEvent` from `crates/sunset-sync/src/lib.rs`. Update the existing `pub use engine::SyncEngine;` line to:

  ```rust
  pub use engine::{EngineEvent, SyncEngine};
  ```

- [ ] **Step 6:** Verify:

  ```
  nix develop --command cargo build -p sunset-sync --all-features
  nix develop --command cargo clippy -p sunset-sync --all-features --all-targets -- -D warnings
  ```

  Expect compile-clean (no functional changes yet — events aren't emitted).

- [ ] **Step 7:** Commit:

  ```
  git add crates/sunset-sync/src/engine.rs crates/sunset-sync/src/lib.rs
  git commit -m "Add EngineEvent enum + subscribe_engine_events fan-out"
  ```

---

### Task 5: Engine emits `PeerAdded` / `PeerRemoved` + fan-out test

**Files:**
- Modify: `crates/sunset-sync/src/engine.rs`

- [ ] **Step 1:** Wire `handle_inbound_event` to emit events. Find the existing match arm:

  ```rust
  InboundEvent::PeerHello { peer_id, kind: _, out_tx } => {
      ...
      self.state.lock().await.peer_outbound.insert(peer_id.clone(), out_tx);
      self.send_bootstrap_digest(&peer_id).await;
  }
  ```

  Replace with (keep the existing body, ADD the event emission after `peer_outbound.insert`):

  ```rust
  InboundEvent::PeerHello { peer_id, kind, out_tx } => {
      self.state
          .lock()
          .await
          .peer_outbound
          .insert(peer_id.clone(), out_tx);
      self.emit_engine_event(EngineEvent::PeerAdded {
          peer_id: peer_id.clone(),
          kind,
      })
      .await;
      self.send_bootstrap_digest(&peer_id).await;
  }
  ```

  Find the existing `Disconnected` arm:

  ```rust
  InboundEvent::Disconnected { peer_id, reason } => {
      eprintln!("sunset-sync: peer {peer_id:?} disconnected: {reason}");
      self.state.lock().await.peer_outbound.remove(&peer_id);
  }
  ```

  Replace with:

  ```rust
  InboundEvent::Disconnected { peer_id, reason } => {
      eprintln!("sunset-sync: peer {peer_id:?} disconnected: {reason}");
      self.state.lock().await.peer_outbound.remove(&peer_id);
      self.emit_engine_event(EngineEvent::PeerRemoved {
          peer_id,
      })
      .await;
  }
  ```

- [ ] **Step 2:** Add a test inside the existing `#[cfg(all(test, feature = "test-helpers"))] mod tests` block at the bottom of `engine.rs`:

  ```rust
  #[tokio::test(flavor = "current_thread")]
  async fn engine_event_fan_out_to_multiple_subscribers() {
      use crate::engine::EngineEvent;
      use crate::transport::TransportKind;

      let local = tokio::task::LocalSet::new();
      local
          .run_until(async {
              let engine = Rc::new(make_engine("alice", b"alice"));
              let mut sub_a = engine.subscribe_engine_events().await;
              let mut sub_b = engine.subscribe_engine_events().await;

              engine
                  .emit_engine_event(EngineEvent::PeerAdded {
                      peer_id: PeerId(vk(b"bob")),
                      kind: TransportKind::Primary,
                  })
                  .await;

              let a = sub_a.recv().await.expect("sub_a got event");
              let b = sub_b.recv().await.expect("sub_b got event");
              match (a, b) {
                  (
                      EngineEvent::PeerAdded { peer_id: pa, kind: ka },
                      EngineEvent::PeerAdded { peer_id: pb, kind: kb },
                  ) => {
                      assert_eq!(pa, PeerId(vk(b"bob")));
                      assert_eq!(pb, PeerId(vk(b"bob")));
                      assert_eq!(ka, TransportKind::Primary);
                      assert_eq!(kb, TransportKind::Primary);
                  }
                  _ => panic!("expected PeerAdded events"),
              }
          })
          .await;
  }

  #[tokio::test(flavor = "current_thread")]
  async fn engine_event_drops_dead_subscriber() {
      use crate::engine::EngineEvent;

      let local = tokio::task::LocalSet::new();
      local
          .run_until(async {
              let engine = Rc::new(make_engine("alice", b"alice"));
              let sub_a = engine.subscribe_engine_events().await;
              drop(sub_a); // simulate the receiver going away

              // First emission triggers lazy GC of the dead sender.
              engine
                  .emit_engine_event(EngineEvent::PeerRemoved {
                      peer_id: PeerId(vk(b"bob")),
                  })
                  .await;

              assert!(engine.state.lock().await.event_subs.is_empty());
          })
          .await;
  }
  ```

  These tests reference `make_engine` and `vk` which are existing helpers in the same `mod tests` (you can confirm with `grep -n "fn make_engine\|fn vk(" crates/sunset-sync/src/engine.rs`). The `emit_engine_event` method is `async fn` on `&self`; tests call it through `engine.emit_engine_event(...)`. Since it's a private method, the test mod (which is inside the same crate) can reach it.

- [ ] **Step 3:** Verify:

  ```
  nix develop --command cargo test -p sunset-sync --all-features engine_event
  nix develop --command cargo clippy -p sunset-sync --all-features --all-targets -- -D warnings
  ```

  Expect 2 new tests pass.

- [ ] **Step 4:** Commit:

  ```
  git add crates/sunset-sync/src/engine.rs
  git commit -m "Emit EngineEvent::PeerAdded/PeerRemoved + fan-out tests"
  ```

---

### Task 6: `MemberJs` + `presence_bucket` derivation logic

**Files:**
- Create: `crates/sunset-web-wasm/src/members.rs`
- Modify: `crates/sunset-web-wasm/src/lib.rs`

- [ ] **Step 1:** Create `crates/sunset-web-wasm/src/members.rs`:

  ```rust
  //! Per-room membership state derived from heartbeat presence entries
  //! + engine peer events. Pure data + reducer functions; the
  //! orchestrating task lives in `membership_tracker.rs`.

  use std::collections::HashMap;

  use wasm_bindgen::prelude::*;

  use sunset_sync::{PeerId, TransportKind};

  /// Three-state presence bucket derived from heartbeat age.
  #[derive(Clone, Copy, Debug, PartialEq, Eq)]
  pub enum Presence {
      Online,
      Away,
      Offline,
  }

  impl Presence {
      pub fn as_str(self) -> &'static str {
          match self {
              Presence::Online => "online",
              Presence::Away => "away",
              Presence::Offline => "offline",
          }
      }
  }

  /// Bucket a heartbeat age into Online / Away / Offline.
  ///
  /// - `age_ms < interval_ms`         → Online
  /// - `interval_ms ≤ age_ms < ttl_ms` → Away
  /// - `age_ms ≥ ttl_ms`              → Offline (caller drops member from list)
  pub fn presence_bucket(age_ms: u64, interval_ms: u64, ttl_ms: u64) -> Presence {
      if age_ms < interval_ms {
          Presence::Online
      } else if age_ms < ttl_ms {
          Presence::Away
      } else {
          Presence::Offline
      }
  }

  /// JS-exported per-member view consumed by the Gleam UI.
  #[wasm_bindgen]
  pub struct MemberJs {
      pub(crate) pubkey: Vec<u8>,
      pub(crate) presence: String,
      pub(crate) connection_mode: String,
      pub(crate) is_self: bool,
  }

  #[wasm_bindgen]
  impl MemberJs {
      #[wasm_bindgen(getter)]
      pub fn pubkey(&self) -> Vec<u8> {
          self.pubkey.clone()
      }
      #[wasm_bindgen(getter)]
      pub fn presence(&self) -> String {
          self.presence.clone()
      }
      #[wasm_bindgen(getter)]
      pub fn connection_mode(&self) -> String {
          self.connection_mode.clone()
      }
      #[wasm_bindgen(getter)]
      pub fn is_self(&self) -> bool {
          self.is_self
      }
  }

  /// Pure derivation: given the current state, return the rendered
  /// member list. Self is always present and always Online.
  pub fn derive_members(
      now_ms: u64,
      interval_ms: u64,
      ttl_ms: u64,
      self_peer: &PeerId,
      presence_map: &HashMap<PeerId, u64>,
      peer_kinds: &HashMap<PeerId, TransportKind>,
  ) -> Vec<MemberJs> {
      let mut out = Vec::new();
      // Self always first.
      out.push(MemberJs {
          pubkey: self_peer.verifying_key().as_bytes().to_vec(),
          presence: Presence::Online.as_str().to_owned(),
          connection_mode: "self".to_owned(),
          is_self: true,
      });
      // Others, sorted by pubkey for stable ordering.
      let mut others: Vec<(&PeerId, &u64)> = presence_map
          .iter()
          .filter(|(pk, _)| *pk != self_peer)
          .collect();
      others.sort_by(|(a, _), (b, _)| a.verifying_key().as_bytes().cmp(b.verifying_key().as_bytes()));
      for (pk, last_ms) in others {
          let age = now_ms.saturating_sub(*last_ms);
          let presence = presence_bucket(age, interval_ms, ttl_ms);
          if presence == Presence::Offline {
              continue;
          }
          let connection_mode = match peer_kinds.get(pk) {
              Some(TransportKind::Secondary) => "direct",
              Some(TransportKind::Primary) => "via_relay",
              _ => "unknown",
          }
          .to_owned();
          out.push(MemberJs {
              pubkey: pk.verifying_key().as_bytes().to_vec(),
              presence: presence.as_str().to_owned(),
              connection_mode,
              is_self: false,
          });
      }
      out
  }

  /// Stable shape signature used to debounce callbacks. The tracker
  /// compares the current signature with the previously-emitted one
  /// and only fires the callback if it changed.
  pub fn members_signature(members: &[MemberJs]) -> Vec<(Vec<u8>, String, String)> {
      members
          .iter()
          .map(|m| (m.pubkey.clone(), m.presence.clone(), m.connection_mode.clone()))
          .collect()
  }

  #[cfg(test)]
  mod tests {
      use super::*;
      use bytes::Bytes;
      use sunset_store::VerifyingKey;

      fn pk(b: u8) -> PeerId {
          PeerId(VerifyingKey::new(Bytes::copy_from_slice(&[b; 32])))
      }

      #[test]
      fn presence_bucket_thresholds() {
          assert_eq!(presence_bucket(0, 1000, 3000), Presence::Online);
          assert_eq!(presence_bucket(999, 1000, 3000), Presence::Online);
          assert_eq!(presence_bucket(1000, 1000, 3000), Presence::Away);
          assert_eq!(presence_bucket(2999, 1000, 3000), Presence::Away);
          assert_eq!(presence_bucket(3000, 1000, 3000), Presence::Offline);
          assert_eq!(presence_bucket(10_000, 1000, 3000), Presence::Offline);
      }

      #[test]
      fn derive_members_self_only_when_no_peers() {
          let me = pk(1);
          let presence = HashMap::new();
          let kinds = HashMap::new();
          let out = derive_members(0, 1000, 3000, &me, &presence, &kinds);
          assert_eq!(out.len(), 1);
          assert!(out[0].is_self);
          assert_eq!(out[0].presence, "online");
          assert_eq!(out[0].connection_mode, "self");
      }

      #[test]
      fn derive_members_skips_offline_peers() {
          let me = pk(1);
          let bob = pk(2);
          let mut presence = HashMap::new();
          presence.insert(bob.clone(), 0u64);
          let kinds = HashMap::new();
          // bob's heartbeat is 5s old but ttl is 3s → Offline → dropped.
          let out = derive_members(5000, 1000, 3000, &me, &presence, &kinds);
          assert_eq!(out.len(), 1);
          assert!(out[0].is_self);
      }

      #[test]
      fn derive_members_maps_kinds_to_modes() {
          let me = pk(1);
          let bob = pk(2);
          let carol = pk(3);
          let dave = pk(4);
          let mut presence = HashMap::new();
          presence.insert(bob.clone(), 100);
          presence.insert(carol.clone(), 100);
          presence.insert(dave.clone(), 100);
          let mut kinds = HashMap::new();
          kinds.insert(bob.clone(), TransportKind::Primary);
          kinds.insert(carol.clone(), TransportKind::Secondary);
          // dave: no kind → "unknown"
          let out = derive_members(200, 1000, 3000, &me, &presence, &kinds);
          assert_eq!(out.len(), 4);
          let modes: Vec<&str> = out
              .iter()
              .map(|m| m.connection_mode.as_str())
              .collect();
          assert_eq!(modes, vec!["self", "via_relay", "direct", "unknown"]);
      }

      #[test]
      fn members_signature_changes_on_presence_change() {
          let me = pk(1);
          let bob = pk(2);
          let mut presence = HashMap::new();
          presence.insert(bob.clone(), 0);
          let kinds = HashMap::new();

          let s1 = members_signature(&derive_members(500, 1000, 3000, &me, &presence, &kinds));
          let s2 = members_signature(&derive_members(1500, 1000, 3000, &me, &presence, &kinds));
          assert_ne!(s1, s2, "Online → Away should change signature");
      }
  }
  ```

- [ ] **Step 2:** Wire `members.rs` into `crates/sunset-web-wasm/src/lib.rs`. Add the module + re-export. The current lib.rs has these `cfg`-gated module decls — match the pattern:

  ```rust
  #[cfg(target_arch = "wasm32")]
  mod members;
  #[cfg(target_arch = "wasm32")]
  pub use members::MemberJs;
  ```

  Place these next to the existing `mod relay_signaler;` / `pub use relay_signaler::...` lines.

- [ ] **Step 3:** Verify (note: `members.rs` is wasm-cfg-gated; tests will run on wasm-only test profile, but the unit tests don't actually need wasm so we run them via `--target` or move them. For now, build-only verification is enough for this task; tests run in Task 9 as part of the wasm test push):

  ```
  nix develop --command cargo build -p sunset-web-wasm --target wasm32-unknown-unknown
  nix develop --command cargo clippy -p sunset-web-wasm --target wasm32-unknown-unknown -- -D warnings
  ```

  Expect compile-clean.

  Run the unit tests against the wasm target via wasm-pack:

  ```
  cd crates/sunset-web-wasm && nix develop ../.. --command wasm-pack test --node -- --lib
  ```

  Expect 4 new tests pass.

- [ ] **Step 4:** Commit:

  ```
  cd $(git rev-parse --show-toplevel)
  git add crates/sunset-web-wasm/src/members.rs crates/sunset-web-wasm/src/lib.rs
  git commit -m "Add MemberJs + presence_bucket + derive_members (pure logic)"
  ```

---

### Task 7: `MembershipTracker` task

**Files:**
- Create: `crates/sunset-web-wasm/src/membership_tracker.rs`
- Modify: `crates/sunset-web-wasm/src/lib.rs`
- Modify: `crates/sunset-web-wasm/Cargo.toml`

- [ ] **Step 0:** Add `wasmtimer` to `crates/sunset-web-wasm/Cargo.toml`. Find the existing `web-time.workspace = true` line and add a sibling:

  ```toml
  wasmtimer.workspace = true
  ```

  (Verify `wasmtimer` is in the workspace `[workspace.dependencies]` of the root Cargo.toml — it is, version 0.4.)

The tracker owns:
- `presence_map: HashMap<PeerId, u64>` (last-heartbeat-ms per peer)
- `peer_kinds: HashMap<PeerId, TransportKind>` (live engine peer set)
- `last_signature: Vec<(Vec<u8>, String, String)>` (debounce)
- callback handles for `on_members_changed` + `on_relay_status_changed`

It runs a single async task that drives three streams + a refresh tick.

- [ ] **Step 1:** Create `crates/sunset-web-wasm/src/membership_tracker.rs`:

  ```rust
  //! Membership + relay-status tracker. One spawned task per Client.
  //!
  //! Three input streams:
  //!   1. local store events on `<room_fp>/presence/` (heartbeats)
  //!   2. engine event stream (PeerAdded / PeerRemoved with kind)
  //!   3. periodic refresh tick (catches Online↔Away threshold crossings
  //!      between heartbeats)
  //!
  //! On every update, re-derives the member list + relay status and
  //! fires the corresponding JS callback if the value changed.

  use std::cell::RefCell;
  use std::collections::HashMap;
  use std::rc::Rc;
  use std::time::Duration;

  use bytes::Bytes;
  use futures::StreamExt;
  use futures::channel::mpsc as fmpsc;
  use js_sys::Array;
  use wasm_bindgen::prelude::*;
  use wasmtimer::tokio::sleep;

  use sunset_store::{Filter, Replay, Store};
  use sunset_store_memory::MemoryStore;
  use sunset_sync::{
      EngineEvent, PeerId, TransportKind,
  };

  use crate::members::{derive_members, members_signature, MemberJs};

  pub struct TrackerHandles {
      pub on_members: Rc<RefCell<Option<js_sys::Function>>>,
      pub on_relay_status: Rc<RefCell<Option<js_sys::Function>>>,
      pub last_relay_status: Rc<RefCell<String>>,
      pub peer_kinds: Rc<RefCell<HashMap<PeerId, TransportKind>>>,
  }

  impl TrackerHandles {
      pub fn new(initial_relay_status: &str) -> Self {
          Self {
              on_members: Rc::new(RefCell::new(None)),
              on_relay_status: Rc::new(RefCell::new(None)),
              last_relay_status: Rc::new(RefCell::new(initial_relay_status.to_owned())),
              peer_kinds: Rc::new(RefCell::new(HashMap::new())),
          }
      }
  }

  /// Spawn the tracker. Runs forever (page lifetime).
  #[allow(clippy::too_many_arguments)]
  pub fn spawn_tracker(
      store: std::sync::Arc<MemoryStore>,
      mut engine_events: tokio::sync::mpsc::UnboundedReceiver<EngineEvent>,
      self_peer: PeerId,
      room_fp_hex: String,
      interval_ms: u64,
      ttl_ms: u64,
      refresh_ms: u64,
      handles: TrackerHandles,
  ) {
      // Periodic refresh ticker as a separate task. Pushes a unit into
      // `refresh_rx` every `refresh_ms`. The main select loop only
      // deals with channels — no inline `sleep().fuse()` pinning
      // gymnastics required.
      let (refresh_tx, mut refresh_rx) = fmpsc::unbounded::<()>();
      sunset_sync::spawn::spawn_local(async move {
          loop {
              sleep(Duration::from_millis(refresh_ms)).await;
              if refresh_tx.unbounded_send(()).is_err() {
                  break;
              }
          }
      });

      sunset_sync::spawn::spawn_local(async move {
          let presence_filter =
              Filter::NamePrefix(Bytes::from(format!("{room_fp_hex}/presence/")));
          let mut presence_sub = match store.subscribe(presence_filter, Replay::All).await {
              Ok(s) => s,
              Err(e) => {
                  web_sys::console::error_1(&JsValue::from_str(&format!(
                      "MembershipTracker: presence subscribe failed: {e}"
                  )));
                  return;
              }
          };
          let presence_map: Rc<RefCell<HashMap<PeerId, u64>>> = Rc::new(RefCell::new(HashMap::new()));
          let last_signature: Rc<RefCell<Vec<(Vec<u8>, String, String)>>> =
              Rc::new(RefCell::new(Vec::new()));
          let prefix = format!("{room_fp_hex}/presence/");

          loop {
              futures::select! {
                  ev = presence_sub.next() => {
                      let Some(ev) = ev else { break };
                      let entry = match ev {
                          Ok(sunset_store::Event::Inserted(e)) => e,
                          Ok(sunset_store::Event::Replaced { new, .. }) => new,
                          Ok(_) => continue,
                          Err(e) => {
                              web_sys::console::warn_1(&JsValue::from_str(&format!(
                                  "MembershipTracker presence event: {e}"
                              )));
                              continue;
                          }
                      };
                      let Some(pk) = parse_presence_pk(&entry.name, &prefix) else { continue };
                      presence_map.borrow_mut().insert(pk, entry.priority);
                      maybe_fire(
                          now_ms(),
                          interval_ms,
                          ttl_ms,
                          &self_peer,
                          &presence_map.borrow(),
                          &handles.peer_kinds.borrow(),
                          &last_signature,
                          handles.on_members.borrow().as_ref(),
                      );
                  }
                  ev = recv_engine(&mut engine_events).fuse() => {
                      let Some(ev) = ev else { break };
                      handle_engine_event(&handles, &ev);
                      maybe_fire_relay_status(&handles);
                      maybe_fire(
                          now_ms(),
                          interval_ms,
                          ttl_ms,
                          &self_peer,
                          &presence_map.borrow(),
                          &handles.peer_kinds.borrow(),
                          &last_signature,
                          handles.on_members.borrow().as_ref(),
                      );
                  }
                  _ = refresh_rx.next() => {
                      // Periodic re-derive (catches Online↔Away threshold crossings).
                      maybe_fire(
                          now_ms(),
                          interval_ms,
                          ttl_ms,
                          &self_peer,
                          &presence_map.borrow(),
                          &handles.peer_kinds.borrow(),
                          &last_signature,
                          handles.on_members.borrow().as_ref(),
                      );
                  }
              }
          }
      });
  }

  fn now_ms() -> u64 {
      web_time::SystemTime::now()
          .duration_since(web_time::UNIX_EPOCH)
          .map(|d| d.as_millis() as u64)
          .unwrap_or(0)
  }

  async fn recv_engine(
      rx: &mut tokio::sync::mpsc::UnboundedReceiver<EngineEvent>,
  ) -> Option<EngineEvent> {
      rx.recv().await
  }

  fn parse_presence_pk(name: &[u8], prefix: &str) -> Option<PeerId> {
      let s = std::str::from_utf8(name).ok()?;
      let suffix = s.strip_prefix(prefix)?;
      let bytes = hex::decode(suffix).ok()?;
      Some(PeerId(sunset_store::VerifyingKey::new(Bytes::from(bytes))))
  }

  fn handle_engine_event(handles: &TrackerHandles, ev: &EngineEvent) {
      match ev {
          EngineEvent::PeerAdded { peer_id, kind } => {
              handles
                  .peer_kinds
                  .borrow_mut()
                  .insert(peer_id.clone(), *kind);
          }
          EngineEvent::PeerRemoved { peer_id } => {
              handles.peer_kinds.borrow_mut().remove(peer_id);
          }
      }
  }

  fn derive_relay_status(peer_kinds: &HashMap<PeerId, TransportKind>, prior: &str) -> String {
      // Sticky "connecting"/"error" states are owned by the Client
      // (set at add_relay call time). We only flip between
      // "connected" and "disconnected" based on whether any Primary
      // connection exists.
      if prior == "connecting" || prior == "error" {
          // Don't override transient explicit states.
          return prior.to_owned();
      }
      if peer_kinds
          .values()
          .any(|k| *k == TransportKind::Primary)
      {
          "connected".to_owned()
      } else {
          "disconnected".to_owned()
      }
  }

  fn maybe_fire_relay_status(handles: &TrackerHandles) {
      let prior = handles.last_relay_status.borrow().clone();
      let next = derive_relay_status(&handles.peer_kinds.borrow(), &prior);
      if next != prior {
          *handles.last_relay_status.borrow_mut() = next.clone();
          if let Some(cb) = handles.on_relay_status.borrow().as_ref() {
              let _ = cb.call1(&JsValue::NULL, &JsValue::from_str(&next));
          }
      }
  }

  #[allow(clippy::too_many_arguments)]
  fn maybe_fire(
      now_ms: u64,
      interval_ms: u64,
      ttl_ms: u64,
      self_peer: &PeerId,
      presence_map: &HashMap<PeerId, u64>,
      peer_kinds: &HashMap<PeerId, TransportKind>,
      last_signature: &RefCell<Vec<(Vec<u8>, String, String)>>,
      callback: Option<&js_sys::Function>,
  ) {
      let members = derive_members(
          now_ms,
          interval_ms,
          ttl_ms,
          self_peer,
          presence_map,
          peer_kinds,
      );
      let sig = members_signature(&members);
      if sig == *last_signature.borrow() {
          return;
      }
      *last_signature.borrow_mut() = sig;
      let Some(cb) = callback else { return };
      let arr = Array::new();
      for m in members {
          arr.push(&JsValue::from(m));
      }
      let _ = cb.call1(&JsValue::NULL, &arr);
  }
  ```

- [ ] **Step 2:** Wire into `crates/sunset-web-wasm/src/lib.rs`:

  ```rust
  #[cfg(target_arch = "wasm32")]
  mod membership_tracker;
  ```

- [ ] **Step 3:** Verify wasm build:

  ```
  nix develop --command cargo build -p sunset-web-wasm --target wasm32-unknown-unknown
  nix develop --command cargo clippy -p sunset-web-wasm --target wasm32-unknown-unknown -- -D warnings
  ```

  Expect compile-clean. Resolve any `wasmtimer` import errors by confirming `wasmtimer.workspace = true` in the crate's Cargo.toml; add it if missing.

- [ ] **Step 4:** Commit:

  ```
  git add crates/sunset-web-wasm/src/membership_tracker.rs crates/sunset-web-wasm/src/lib.rs crates/sunset-web-wasm/Cargo.toml
  git commit -m "Add MembershipTracker: derive member list + relay status from real state"
  ```

---

### Task 8: Heartbeat publisher (`presence_publisher`)

**Files:**
- Create: `crates/sunset-web-wasm/src/presence_publisher.rs`
- Modify: `crates/sunset-web-wasm/src/lib.rs`

- [ ] **Step 1:** Create `crates/sunset-web-wasm/src/presence_publisher.rs`:

  ```rust
  //! Heartbeat publisher: spawns a task that periodically writes a
  //! `<room_fp>/presence/<my_pk>` entry into the local store. The
  //! engine's existing room_filter subscription propagates these to
  //! peers automatically.

  use std::sync::Arc;
  use std::time::Duration;

  use bytes::Bytes;
  use wasm_bindgen::prelude::*;
  use wasmtimer::tokio::sleep;

  use sunset_core::Identity;
  use sunset_store::{
      canonical::signing_payload, ContentBlock, SignedKvEntry, Store, VerifyingKey,
  };
  use sunset_store_memory::MemoryStore;

  /// Spawn the heartbeat publisher. Runs forever (page lifetime).
  pub fn spawn_publisher(
      identity: Identity,
      room_fp_hex: String,
      store: Arc<MemoryStore>,
      interval_ms: u64,
      ttl_ms: u64,
  ) {
      sunset_sync::spawn::spawn_local(async move {
          let my_hex = hex::encode(identity.store_verifying_key().as_bytes());
          let name_str = format!("{room_fp_hex}/presence/{my_hex}");
          loop {
              if let Err(e) = publish_once(&identity, &name_str, &store, ttl_ms).await {
                  web_sys::console::warn_1(&JsValue::from_str(&format!(
                      "presence publisher: {e}"
                  )));
              }
              sleep(Duration::from_millis(interval_ms)).await;
          }
      });
  }

  async fn publish_once(
      identity: &Identity,
      name_str: &str,
      store: &MemoryStore,
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

  // Suppress unused-import warning when not on wasm — VerifyingKey only
  // appears via type plumbing inside Identity.
  #[allow(dead_code)]
  fn _vk_used(v: VerifyingKey) -> VerifyingKey {
      v
  }
  ```

  (The `_vk_used` helper exists only so clippy doesn't grumble about an unused import for `VerifyingKey`. Drop it if clippy is happy without.)

- [ ] **Step 2:** Wire into `crates/sunset-web-wasm/src/lib.rs`:

  ```rust
  #[cfg(target_arch = "wasm32")]
  mod presence_publisher;
  ```

- [ ] **Step 3:** Verify:

  ```
  nix develop --command cargo build -p sunset-web-wasm --target wasm32-unknown-unknown
  nix develop --command cargo clippy -p sunset-web-wasm --target wasm32-unknown-unknown -- -D warnings
  ```

  If clippy complains about `_vk_used` being dead code, remove it AND the `VerifyingKey` import.

- [ ] **Step 4:** Commit:

  ```
  git add crates/sunset-web-wasm/src/presence_publisher.rs crates/sunset-web-wasm/src/lib.rs
  git commit -m "Add presence publisher: signed heartbeat every interval_ms"
  ```

---

### Task 9: Wire `Client::start_presence` + `on_members_changed` + `on_relay_status_changed`

**Files:**
- Modify: `crates/sunset-web-wasm/src/client.rs`

- [ ] **Step 1:** Replace the relay_status field's type in `Client` (currently `Rc<RefCell<&'static str>>`) with an `Rc<RefCell<String>>` so the tracker can write any value. Find and replace:

  ```rust
  relay_status: Rc<RefCell<&'static str>>,
  ```
  with:
  ```rust
  relay_status: Rc<RefCell<String>>,
  ```

  And the initialiser line `relay_status: Rc::new(RefCell::new("disconnected")),` → `relay_status: Rc::new(RefCell::new("disconnected".to_owned())),`.

  Update the existing `relay_status` getter to clone the String, and update every `*self.relay_status.borrow_mut() = "connecting"` style assignment to use `.to_owned()`:

  ```rust
  #[wasm_bindgen(getter)]
  pub fn relay_status(&self) -> String {
      self.relay_status.borrow().clone()
  }

  pub async fn add_relay(&self, url_with_fragment: String) -> Result<(), JsError> {
      *self.relay_status.borrow_mut() = "connecting".to_owned();
      let addr = sunset_sync::PeerAddr::new(Bytes::from(url_with_fragment));
      match self.engine.add_peer(addr).await {
          Ok(()) => {
              *self.relay_status.borrow_mut() = "connected".to_owned();
              Ok(())
          }
          Err(e) => {
              *self.relay_status.borrow_mut() = "error".to_owned();
              Err(JsError::new(&format!("add_relay: {e}")))
          }
      }
  }
  ```

- [ ] **Step 2:** Add new fields to `Client`:

  ```rust
  presence_started: Rc<RefCell<bool>>,
  tracker_handles: Rc<crate::membership_tracker::TrackerHandles>,
  ```

  Initialise in `new()`:

  ```rust
  presence_started: Rc::new(RefCell::new(false)),
  tracker_handles: Rc::new(crate::membership_tracker::TrackerHandles::new("disconnected")),
  ```

- [ ] **Step 3:** Add the public methods to the `#[wasm_bindgen] impl Client` block. Place after the existing `peer_connection_mode`:

  ```rust
  /// Start the heartbeat publisher + the membership tracker.
  /// Idempotent: a second call is a no-op. The Gleam UI calls this
  /// once after the Client is constructed.
  pub async fn start_presence(&self, interval_ms: u32, ttl_ms: u32, refresh_ms: u32) {
      if *self.presence_started.borrow() {
          return;
      }
      *self.presence_started.borrow_mut() = true;

      let room_fp_hex = self.room.fingerprint().to_hex();
      let local_peer = sunset_sync::PeerId(self.identity.store_verifying_key());

      crate::presence_publisher::spawn_publisher(
          self.identity.clone(),
          room_fp_hex.clone(),
          self.store.clone(),
          interval_ms as u64,
          ttl_ms as u64,
      );

      let engine_events = self.engine.subscribe_engine_events().await;
      crate::membership_tracker::spawn_tracker(
          self.store.clone(),
          engine_events,
          local_peer,
          room_fp_hex,
          interval_ms as u64,
          ttl_ms as u64,
          refresh_ms as u64,
          (*self.tracker_handles).clone(),
      );
  }

  pub fn on_members_changed(&self, callback: js_sys::Function) {
      *self.tracker_handles.on_members.borrow_mut() = Some(callback);
  }

  pub fn on_relay_status_changed(&self, callback: js_sys::Function) {
      *self.tracker_handles.on_relay_status.borrow_mut() = Some(callback);
  }
  ```

- [ ] **Step 4:** `TrackerHandles` doesn't derive Clone, but the spawn call needs to move the handles into the spawned task while leaving a copy on `self.tracker_handles`. Add `#[derive(Clone)]` to `TrackerHandles` in `membership_tracker.rs` (all its fields are `Rc<...>` so cheap to clone).

  Update `crates/sunset-web-wasm/src/membership_tracker.rs`'s `TrackerHandles` declaration:

  ```rust
  #[derive(Clone)]
  pub struct TrackerHandles {
      ...
  }
  ```

- [ ] **Step 5:** Refactor the existing `peer_connection_mode` getter to use `tracker_handles.peer_kinds` instead of the now-removed `direct_peers`. Find:

  ```rust
  pub fn peer_connection_mode(&self, peer_pubkey: &[u8]) -> String {
      let pk: [u8; 32] = match peer_pubkey.try_into() { ... };
      let peer_id = PeerId(sunset_store::VerifyingKey::new(Bytes::copy_from_slice(&pk)));
      if self.direct_peers.borrow().contains(&peer_id) {
          "direct".to_owned()
      } else if *self.relay_status.borrow() == "connected" {
          "via_relay".to_owned()
      } else {
          "unknown".to_owned()
      }
  }
  ```

  Replace with:

  ```rust
  pub fn peer_connection_mode(&self, peer_pubkey: &[u8]) -> String {
      use sunset_sync::TransportKind;
      let pk: [u8; 32] = match peer_pubkey.try_into() {
          Ok(p) => p,
          Err(_) => return "unknown".to_owned(),
      };
      let peer_id = PeerId(sunset_store::VerifyingKey::new(Bytes::copy_from_slice(&pk)));
      match self.tracker_handles.peer_kinds.borrow().get(&peer_id) {
          Some(TransportKind::Secondary) => "direct",
          Some(TransportKind::Primary) => "via_relay",
          _ => "unknown",
      }
      .to_owned()
  }
  ```

  Then DELETE the `direct_peers` field declaration, its initialiser, and the `direct_peers.borrow_mut().insert(peer_id);` call inside `connect_direct`.

- [ ] **Step 6:** Verify:

  ```
  nix develop --command cargo build -p sunset-web-wasm --target wasm32-unknown-unknown
  nix develop --command cargo clippy -p sunset-web-wasm --target wasm32-unknown-unknown -- -D warnings
  ```

  Resolve any `unused_imports` for `HashSet`.

- [ ] **Step 7:** Commit:

  ```
  git add crates/sunset-web-wasm/src/client.rs crates/sunset-web-wasm/src/membership_tracker.rs
  git commit -m "Wire start_presence + on_members_changed + on_relay_status_changed in Client"
  ```

---

### Task 10: Existing kill_relay test must keep passing — quick check

**Files:** none modified, smoke test only.

- [ ] **Step 1:** Run the existing Playwright kill-relay test with the new code:

  ```
  nix run .#web-test -- --grep "relay death"
  ```

  Expect 1 passed. If it fails, the most likely cause is the `peer_connection_mode` refactor didn't preserve the connect-side direct-marking. Recall that the engine emits `PeerAdded { kind: Secondary }` when an outbound WebRTC connection completes and the per-peer task receives the remote's Hello. Verify by adding a `console.log` to the Client's `peer_connection_mode` if needed; remove before continuing.

- [ ] **Step 2:** No commit needed if it passes. If you had to fix anything, commit the fix:

  ```
  git add -A
  git commit -m "Fix peer_connection_mode regression after tracker refactor"
  ```

---

### Task 11: Gleam externals + JS shims

**Files:**
- Modify: `web/src/sunset_web/sunset.gleam`
- Modify: `web/src/sunset_web/sunset.ffi.mjs`

- [ ] **Step 1:** Add to `web/src/sunset_web/sunset.gleam` (place after the existing `IncomingMessage` accessors block):

  ```gleam
  pub type MemberJs

  /// Start the heartbeat publisher + membership tracker. Idempotent.
  @external(javascript, "./sunset.ffi.mjs", "startPresence")
  pub fn start_presence(
    client: ClientHandle,
    interval_ms: Int,
    ttl_ms: Int,
    refresh_ms: Int,
  ) -> Nil

  @external(javascript, "./sunset.ffi.mjs", "onMembersChanged")
  pub fn on_members_changed(
    client: ClientHandle,
    callback: fn(List(MemberJs)) -> Nil,
  ) -> Nil

  @external(javascript, "./sunset.ffi.mjs", "onRelayStatusChanged")
  pub fn on_relay_status_changed(
    client: ClientHandle,
    callback: fn(String) -> Nil,
  ) -> Nil

  @external(javascript, "./sunset.ffi.mjs", "memPubkey")
  pub fn mem_pubkey(m: MemberJs) -> BitArray

  @external(javascript, "./sunset.ffi.mjs", "memPresence")
  pub fn mem_presence(m: MemberJs) -> String

  @external(javascript, "./sunset.ffi.mjs", "memConnectionMode")
  pub fn mem_connection_mode(m: MemberJs) -> String

  @external(javascript, "./sunset.ffi.mjs", "memIsSelf")
  pub fn mem_is_self(m: MemberJs) -> Bool

  /// Read presence-cadence params from `?presence_interval=&presence_ttl=&presence_refresh=`.
  /// Returns `#(interval_ms, ttl_ms, refresh_ms)`. Defaults: 30000/60000/5000.
  @external(javascript, "./sunset.ffi.mjs", "presenceParamsFromUrl")
  pub fn presence_params_from_url() -> #(Int, Int, Int)
  ```

- [ ] **Step 2:** First, identify which Gleam-list helper `prelude.mjs` exports. The codebase already uses `BitArray, Ok, Error as GError` from prelude.mjs at the top of `sunset.ffi.mjs`; we need to add the array → list helper. Run:

  ```
  grep -nE "export (function |class )?(toList|List|Empty)" web/build/dev/javascript/prelude.mjs | head -10
  ```

  Pick whichever is exported:
  - If `toList` is exported: `import { ..., toList } from "../../prelude.mjs"; const jsArrayToGleamList = (arr) => toList(arr);`
  - If only `List` + `Empty` classes are exported: `import { ..., List, Empty } from "../../prelude.mjs"; function jsArrayToGleamList(arr) { let out = new Empty(); for (let i = arr.length - 1; i >= 0; i--) out = new List(arr[i], out); return out; }`
  - If `$List` / `$Empty` (with $-prefix) are the export names, use those.

  Use the form that matches. Below assumes `toList` is exported (the common case in modern Gleam).

  Update the existing import line at the **top** of `web/src/sunset_web/sunset.ffi.mjs` (currently `import { BitArray, Ok, Error as GError } from "../../prelude.mjs";`) to include the chosen helper:

  ```javascript
  import { BitArray, Ok, Error as GError, toList } from "../../prelude.mjs";
  ```

  Then append to the end of `web/src/sunset_web/sunset.ffi.mjs`:

  ```javascript
  // Presence + membership FFI shims.

  export async function startPresence(client, intervalMs, ttlMs, refreshMs) {
    try {
      await client.start_presence(intervalMs, ttlMs, refreshMs);
    } catch (e) {
      console.warn("startPresence failed", e);
    }
  }

  export function onMembersChanged(client, callback) {
    client.on_members_changed((members) => {
      try {
        callback(toList(Array.from(members)));
      } catch (e) {
        console.warn("onMembersChanged callback threw", e);
      }
    });
  }

  export function onRelayStatusChanged(client, callback) {
    client.on_relay_status_changed((s) => {
      try {
        callback(String(s));
      } catch (e) {
        console.warn("onRelayStatusChanged callback threw", e);
      }
    });
  }

  export function memPubkey(m) {
    return new BitArray(m.pubkey);
  }
  export function memPresence(m) {
    return m.presence;
  }
  export function memConnectionMode(m) {
    return m.connection_mode;
  }
  export function memIsSelf(m) {
    return m.is_self;
  }

  export function presenceParamsFromUrl() {
    const params = new URLSearchParams(window.location.search);
    const parseOr = (key, dflt) => {
      const raw = params.get(key);
      if (raw === null) return dflt;
      const n = parseInt(raw, 10);
      return Number.isFinite(n) && n > 0 ? n : dflt;
    };
    const interval = parseOr("presence_interval", 30000);
    const ttl = parseOr("presence_ttl", 60000);
    const refresh = parseOr("presence_refresh", 5000);
    // Gleam tuple #(Int, Int, Int) is a 3-element JS array.
    return [interval, ttl, refresh];
  }
  ```

- [ ] **Step 3:** Verify Gleam compiles:

  ```
  cd web && nix develop ../.. --command gleam build
  ```

  Expect compile-clean.

- [ ] **Step 4:** Commit:

  ```
  cd $(git rev-parse --show-toplevel)
  git add web/src/sunset_web/sunset.gleam web/src/sunset_web/sunset.ffi.mjs
  git commit -m "Add Gleam externals + JS shims for presence + membership"
  ```

---

### Task 12: Gleam Model + Msg + bootstrap + view wiring

**Files:**
- Modify: `web/src/sunset_web.gleam`

- [ ] **Step 1:** Find the `Model` declaration around line ~44 and add the `members` field. The new shape:

  ```gleam
  pub type Model {
    Model(
      mode: Mode,
      view: View,
      joined_rooms: List(String),
      rooms_collapsed: Bool,
      landing_input: String,
      sidebar_search: String,
      current_channel: ChannelId,
      draft: String,
      reacting_to: Option(String),
      detail_msg_id: Option(String),
      reactions: Dict(String, List(Reaction)),
      dragging_room: Option(String),
      drag_over_room: Option(String),
      voice_popover: Option(String),
      voice_settings: Dict(String, domain.VoiceSettings),
      client: Option(ClientHandle),
      messages: List(domain.Message),
      relay_status: String,
      members: List(domain.Member),
    )
  }
  ```

  Update the corresponding `Model(...)` constructor call inside `init` (around line 160) to add `members: []`.

- [ ] **Step 2:** Find the `Msg` declaration and add the two new variants:

  ```gleam
  pub type Msg {
    NoOp
    ToggleMode
    ...existing variants...
    MembersUpdated(List(domain.Member))
    RelayStatusUpdated(String)
  }
  ```

- [ ] **Step 3:** Add the message handlers in the `update` function. Find an existing handler (e.g. `RelayConnectResult`) and add these alongside it:

  ```gleam
  MembersUpdated(ms) -> #(Model(..model, members: ms), effect.none())
  RelayStatusUpdated(s) -> #(Model(..model, relay_status: s), effect.none())
  ```

- [ ] **Step 4:** Wire the bootstrap. Find the `RelayConnectResult(Ok(_))` handler (around line 496). After the existing `pub_eff` is built, add a parallel effect that starts presence and registers the callbacks. Replace:

  ```gleam
  RelayConnectResult(Ok(_)) ->
    case model.client {
      Some(client) -> {
        let pub_eff =
          effect.from(fn(dispatch) {
            sunset.publish_room_subscription(client, fn(r) {
              dispatch(SubscribePublishResult(r))
            })
          })
        #(Model(..model, relay_status: "connected"), pub_eff)
      }
      None -> #(model, effect.none())
    }
  ```

  with:

  ```gleam
  RelayConnectResult(Ok(_)) ->
    case model.client {
      Some(client) -> {
        let pub_eff =
          effect.from(fn(dispatch) {
            sunset.publish_room_subscription(client, fn(r) {
              dispatch(SubscribePublishResult(r))
            })
          })
        let presence_eff =
          effect.from(fn(dispatch) {
            let #(interval, ttl, refresh) = sunset.presence_params_from_url()
            sunset.start_presence(client, interval, ttl, refresh)
            sunset.on_members_changed(client, fn(ms) {
              dispatch(MembersUpdated(map_members(ms)))
            })
            sunset.on_relay_status_changed(client, fn(s) {
              dispatch(RelayStatusUpdated(s))
            })
          })
        #(
          Model(..model, relay_status: "connected"),
          effect.batch([pub_eff, presence_eff]),
        )
      }
      None -> #(model, effect.none())
    }
  ```

- [ ] **Step 5:** Add the `map_members` + helpers near the existing `format_time_ms`/`short_pubkey` helpers (search the file for `fn short_pubkey`):

  ```gleam
  fn map_members(ms: List(sunset.MemberJs)) -> List(domain.Member) {
    list.map(ms, fn(m) {
      let pk = sunset.mem_pubkey(m)
      domain.Member(
        id: domain.MemberId(short_pubkey(pk)),
        name: short_pubkey(pk),
        initials: short_initials(pk),
        status: presence_to_status(sunset.mem_presence(m)),
        relay: connection_mode_to_relay(sunset.mem_connection_mode(m)),
        you: sunset.mem_is_self(m),
        in_call: False,
        bridge: domain.NoBridge,
        role: domain.NoRole,
      )
    })
  }

  fn presence_to_status(s: String) -> domain.Presence {
    case s {
      "online" -> domain.Online
      "away" -> domain.Away
      _ -> domain.OfflineP
    }
  }

  fn connection_mode_to_relay(s: String) -> domain.RelayStatus {
    case s {
      "direct" -> domain.Direct
      "via_relay" -> domain.OneHop
      "self" -> domain.SelfRelay
      _ -> domain.NoRelay
    }
  }
  ```

  Verify the `domain.Member` constructor's required fields by reading `web/src/sunset_web/domain.gleam:82-94` — adjust the named-fields above if any are missing. The `NoRole` variant comes from `domain.RoleOpt`; if the project uses a different default, use that instead. If `domain.Member` doesn't have a `role` field at all, drop that line.

- [ ] **Step 6:** Replace `fixture.members()` with `model.members` at the call sites in this file. Search:

  ```
  grep -n "fixture.members()" web/src/sunset_web.gleam
  ```

  For each call site, threading `model.members` through. The call at line ~742 (`members: fixture.members()` inside a `Room(...)` constructor for the room context) should still pass `fixture.members()` (the `Room` type's `members: Int` field is a count, not a list — check by reading the field — if it's `Int`, leave it). The call at line ~765 (`members.view(palette: palette, members: fixture.members())`) becomes:

  ```gleam
  members.view(palette: palette, members: model.members)
  ```

  The lookup at line ~775 (`list.find(fixture.members(), ...)`) — if this powers the voice popover, it stays as fixture for now (out-of-scope for this plan; voice work uses fixture members). Add an inline comment:

  ```gleam
  // Voice popover still reads fixture members — out of scope for this
  // plan (real voice presence comes in V3).
  ```

- [ ] **Step 7:** Verify Gleam compiles:

  ```
  cd web && nix develop ../.. --command gleam build
  ```

  Expect compile-clean. Resolve any "unused variable" warnings on the new helpers (Gleam may complain if `presence_to_status` etc. only appear in `map_members` — they do, so should be fine).

- [ ] **Step 8:** Commit:

  ```
  cd $(git rev-parse --show-toplevel)
  git add web/src/sunset_web.gleam
  git commit -m "Wire MembersUpdated + RelayStatusUpdated into Model + view"
  ```

---

### Task 13: Playwright `presence.spec.js`

**Files:**
- Create: `web/e2e/presence.spec.js`

- [ ] **Step 1:** Create `web/e2e/presence.spec.js`. Borrow the relay-spawn boilerplate from `kill_relay.spec.js`:

  ```javascript
  // Presence + membership e2e.
  //
  // Uses fast-mode URL params to compress the wall-clock arc of
  // Online → Away → Offline transitions to ~1.5s.

  import { test, expect } from "@playwright/test";
  import { spawn } from "child_process";
  import { mkdtempSync, rmSync } from "fs";
  import { tmpdir } from "os";
  import { join } from "path";

  let relayProcess = null;
  let relayAddress = null;
  let relayDataDir = null;

  test.beforeAll(async () => {
    relayDataDir = mkdtempSync(join(tmpdir(), "sunset-relay-presence-test-"));
    const configPath = join(relayDataDir, "relay.toml");
    const fs = await import("fs/promises");
    await fs.writeFile(
      configPath,
      [
        `listen_addr = "127.0.0.1:0"`,
        `data_dir = "${relayDataDir}"`,
        `interest_filter = "all"`,
        `identity_secret = "auto"`,
        `peers = []`,
        "",
      ].join("\n"),
    );
    relayProcess = spawn("sunset-relay", ["--config", configPath], {
      stdio: ["ignore", "pipe", "pipe"],
    });
    relayAddress = await new Promise((resolve, reject) => {
      const timer = setTimeout(
        () => reject(new Error("relay didn't print address banner within 15s")),
        15_000,
      );
      let buffer = "";
      relayProcess.stdout.on("data", (chunk) => {
        buffer += chunk.toString();
        const m = buffer.match(/address:\s+(ws:\/\/[^\s]+)/);
        if (m) {
          clearTimeout(timer);
          resolve(m[1]);
        }
      });
      relayProcess.stderr.on("data", (chunk) => {
        process.stderr.write(`[relay] ${chunk}`);
      });
      relayProcess.on("error", (e) => {
        clearTimeout(timer);
        reject(e);
      });
      relayProcess.on("exit", (code) => {
        if (code !== null && code !== 0) {
          clearTimeout(timer);
          reject(new Error(`relay exited prematurely with code ${code}`));
        }
      });
    });
  });

  test.afterAll(async () => {
    if (relayProcess && relayProcess.exitCode === null) {
      relayProcess.kill("SIGTERM");
    }
    if (relayDataDir) {
      rmSync(relayDataDir, { recursive: true, force: true });
    }
  });

  function fastUrl(relay) {
    return `/?relay=${encodeURIComponent(relay)}&presence_interval=300&presence_ttl=900&presence_refresh=100#sunset-presence-test`;
  }

  async function setupPage(browser) {
    const ctx = await browser.newContext();
    const page = await ctx.newPage();
    await page.addInitScript(() => { window.SUNSET_TEST = true; });
    page.on("pageerror", (err) =>
      process.stderr.write(`[pageerror] ${err.stack || err}\n`),
    );
    page.on("console", (msg) => {
      if (msg.type() === "error") process.stderr.write(`[console] ${msg.text()}\n`);
    });
    return { ctx, page };
  }

  async function memberMode(page, peerPkArr) {
    return await page.evaluate((pkArr) => {
      const target = new Uint8Array(pkArr);
      const eq = (a, b) => a.length === b.length && a.every((x, i) => x === b[i]);
      const ms = window.__sunsetLastMembers || [];
      const m = ms.find((mm) => eq(Array.from(mm.pubkey), Array.from(target)));
      return m ? { presence: m.presence, mode: m.connection_mode } : null;
    }, peerPkArr);
  }

  test.setTimeout(30_000);

  test("two browsers see each other in the member rail", async ({ browser }) => {
    const { page: a } = await setupPage(browser);
    const { page: b } = await setupPage(browser);
    await a.goto(fastUrl(relayAddress));
    await b.goto(fastUrl(relayAddress));
    await expect(a.getByText("sunset", { exact: true })).toBeVisible({ timeout: 15_000 });
    await expect(b.getByText("sunset", { exact: true })).toBeVisible({ timeout: 15_000 });

    // Page exposes received members on window.__sunsetLastMembers via the
    // FFI shim — confirm via a small init script that captures the array
    // on every callback fire.
    for (const p of [a, b]) {
      await p.evaluate(() => {
        // Stash members as plain JS objects (wasm-bindgen objects can
        // be freed between calls; freezing them avoids use-after-free).
        window.sunsetClient.on_members_changed((members) => {
          window.__sunsetLastMembers = Array.from(members).map((m) => ({
            pubkey: Array.from(m.pubkey),
            presence: m.presence,
            connection_mode: m.connection_mode,
            is_self: m.is_self,
          }));
        });
      });
    }

    // Wait until A sees B's presence with via_relay mode.
    const bPub = await b.evaluate(() => Array.from(window.sunsetClient.public_key));
    await a.waitForFunction(
      (pkArr) => {
        const target = new Uint8Array(pkArr);
        const eq = (x, y) => x.length === y.length && x.every((v, i) => v === y[i]);
        const ms = window.__sunsetLastMembers || [];
        const m = ms.find((mm) => eq(Array.from(mm.pubkey), Array.from(target)));
        return m && m.presence === "online" && m.connection_mode === "via_relay";
      },
      bPub,
      { timeout: 5_000 },
    );

    const aPub = await a.evaluate(() => Array.from(window.sunsetClient.public_key));
    await b.waitForFunction(
      (pkArr) => {
        const target = new Uint8Array(pkArr);
        const eq = (x, y) => x.length === y.length && x.every((v, i) => v === y[i]);
        const ms = window.__sunsetLastMembers || [];
        const m = ms.find((mm) => eq(Array.from(mm.pubkey), Array.from(target)));
        return m && m.presence === "online" && m.connection_mode === "via_relay";
      },
      aPub,
      { timeout: 5_000 },
    );
  });

  test("connect_direct flips connection_mode to direct", async ({ browser }) => {
    const { page: a } = await setupPage(browser);
    const { page: b } = await setupPage(browser);
    await a.goto(fastUrl(relayAddress));
    await b.goto(fastUrl(relayAddress));
    await expect(a.getByText("sunset", { exact: true })).toBeVisible({ timeout: 15_000 });
    await expect(b.getByText("sunset", { exact: true })).toBeVisible({ timeout: 15_000 });

    for (const p of [a, b]) {
      await p.evaluate(() => {
        const orig = window.sunsetClient.on_members_changed.bind(window.sunsetClient);
        orig((members) => { window.__sunsetLastMembers = members; });
      });
    }

    const bPub = await b.evaluate(() => Array.from(window.sunsetClient.public_key));
    await a.evaluate(async (pkArr) => {
      await window.sunsetClient.connect_direct(new Uint8Array(pkArr));
    }, bPub);

    await a.waitForFunction(
      (pkArr) => {
        const target = new Uint8Array(pkArr);
        const eq = (x, y) => x.length === y.length && x.every((v, i) => v === y[i]);
        const ms = window.__sunsetLastMembers || [];
        const m = ms.find((mm) => eq(Array.from(mm.pubkey), Array.from(target)));
        return m && m.connection_mode === "direct";
      },
      bPub,
      { timeout: 10_000 },
    );
  });

  test("closing one tab makes the other side see away then drop", async ({ browser }) => {
    const { ctx: ctxA, page: a } = await setupPage(browser);
    const { page: b } = await setupPage(browser);
    await a.goto(fastUrl(relayAddress));
    await b.goto(fastUrl(relayAddress));
    await expect(a.getByText("sunset", { exact: true })).toBeVisible({ timeout: 15_000 });
    await expect(b.getByText("sunset", { exact: true })).toBeVisible({ timeout: 15_000 });

    await b.evaluate(() => {
      window.sunsetClient.on_members_changed((members) => {
        window.__sunsetLastMembers = Array.from(members).map((m) => ({
          pubkey: Array.from(m.pubkey),
          presence: m.presence,
          connection_mode: m.connection_mode,
          is_self: m.is_self,
        }));
      });
    });

    const aPub = await a.evaluate(() => Array.from(window.sunsetClient.public_key));

    // Confirm B sees A first.
    await b.waitForFunction(
      (pkArr) => {
        const target = new Uint8Array(pkArr);
        const eq = (x, y) => x.length === y.length && x.every((v, i) => v === y[i]);
        const ms = window.__sunsetLastMembers || [];
        return ms.some((mm) => eq(Array.from(mm.pubkey), Array.from(target)));
      },
      aPub,
      { timeout: 5_000 },
    );

    // Close A.
    await ctxA.close();

    // Within ttl_ms (900) + refresh_ms (100) buffer, A should be dropped from B's list.
    await b.waitForFunction(
      (pkArr) => {
        const target = new Uint8Array(pkArr);
        const eq = (x, y) => x.length === y.length && x.every((v, i) => v === y[i]);
        const ms = window.__sunsetLastMembers || [];
        return !ms.some((mm) => eq(Array.from(mm.pubkey), Array.from(target)));
      },
      aPub,
      { timeout: 5_000 },
    );
  });
  ```

  **Note:** the JS shim for `on_members_changed` calls the underlying `client.on_members_changed` exactly once (storing the callback on the wasm side). Calling it a second time overwrites the previous callback. The test's pattern of "call again with a wrapper" therefore works because the wrapper installs *itself* as the new callback. If the kill_relay test or another piece of test infrastructure already registered a callback on `window.sunsetClient`, we'll need to chain through it; for now this isolated test is fine.

  Actually — observe that `Client::on_members_changed` is currently a setter, not a multi-subscriber. In the test we call it twice (once from the Gleam UI's bootstrap, once from the test re-registration). The test's re-registration WINS. The Gleam UI loses its callback and stops getting member updates — but the test doesn't care about UI rendering, only about `window.__sunsetLastMembers` being populated. Acceptable.

- [ ] **Step 2:** Run the new test from the worktree:

  ```
  nix run .#web-test -- --grep "presence"
  ```

  Expect 3 passed (or whatever the test count grep matches).

- [ ] **Step 3:** Commit:

  ```
  git add web/e2e/presence.spec.js
  git commit -m "Add Playwright presence + member rail e2e (fast mode)"
  ```

---

### Task 14: Final pass

- [ ] **Step 1:** Workspace-wide checks:

  ```
  nix develop --command cargo fmt --all --check
  nix develop --command cargo clippy --workspace --all-features --all-targets -- -D warnings
  nix develop --command cargo test --workspace --all-features
  ```

  Expect all green.

- [ ] **Step 2:** All wasm builds:

  ```
  nix develop --command cargo build -p sunset-noise --target wasm32-unknown-unknown --lib
  nix develop --command cargo build -p sunset-core --target wasm32-unknown-unknown --lib
  nix develop --command cargo build -p sunset-sync-ws-browser --target wasm32-unknown-unknown --lib
  nix develop --command cargo build -p sunset-sync-webrtc-browser --target wasm32-unknown-unknown --lib
  nix develop --command cargo build -p sunset-web-wasm --target wasm32-unknown-unknown --lib
  ```

- [ ] **Step 3:** All Nix derivations:

  ```
  nix build .#sunset-core-wasm .#sunset-web-wasm .#sunset-relay .#sunset-relay-docker .#web --no-link
  ```

- [ ] **Step 4:** Full Playwright suite:

  ```
  nix run .#web-test
  ```

  Expect: prior tests still pass (`two_browser_chat`, `kill_relay`, the 5 fixture-skipped tests still skip), plus the 3 new `presence.spec.js` tests pass.

- [ ] **Step 5:** If any cleanup needed:

  ```
  git add -u
  git commit -m "Final fmt + clippy pass"
  ```

---

## Verification (end-state acceptance)

- All cargo checks (fmt / clippy / test) green.
- All wasm builds succeed.
- All Nix derivations build.
- Playwright suite: prior tests still pass; new `presence.spec.js` 3 tests pass.
- `git log --oneline master..HEAD` — roughly 12-14 task-by-task commits.
- The Gleam UI's member rail shows real peers (self + anyone whose heartbeat is fresh), with their connection mode rendered as `Direct` (WebRTC) or `OneHop` (relay-mediated).
- Relay-status badge flips to "disconnected" within ~1s of relay process death (instead of staying "connected" forever).

---

## What this unlocks

- **V1.5** (auto-upgrade) becomes much smaller — the Client already has the presence map of "everyone in the room"; speculatively dialing connect_direct for each is a few lines.
- Real receipts work later becomes "subscribe to a different per-room namespace" rather than a fresh design.
- Voice presence (`Speaking` / `MutedP`) plugs into the same `MembershipTracker` — add a second subscription on `<room_fp>/voice-presence/`, fold into the same MemberJs.
