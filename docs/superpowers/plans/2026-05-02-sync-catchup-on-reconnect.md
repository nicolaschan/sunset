# Sync catch-up on reconnect — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make a sunset client catch up on chat events that arrived at the relay while it was disconnected, by firing per-published-filter Bloom digests on `PeerHello` and on every anti-entropy tick.

**Architecture:** Engine-side change only. The generalized `send_filter_digest(to, filter)` helper already exists on master (PR #10). On `PeerHello` (post-Hello bootstrap) and on each `tick_anti_entropy`, walk our own self-authored `_sunset-sync/subscribe` entries and fire one digest per filter. Reuses existing wire format (`SyncMessage::DigestExchange`), no protocol-version bump, no frontend changes.

**Tech Stack:** Rust 2021, `sunset-sync`, `sunset-store-memory`, `tokio` single-thread runtime + `LocalSet` (engine is `?Send`), `postcard` wire format.

---

## Spec reference

Design doc: `docs/superpowers/specs/2026-05-02-sync-catchup-on-reconnect-design.md`

## Files

- Modify: `crates/sunset-sync/src/engine.rs`
  - Add `own_published_filters()` private helper.
  - Wire per-filter digests into `handle_inbound_event::PeerHello` (Change 1).
  - Route `tick_anti_entropy` through `send_filter_digest` and add per-published-filter fires (Change 2).
  - Add unit tests in the existing `#[cfg(all(test, feature = "test-helpers"))] mod tests` block.

No new files. No public API changes.

## Test commands

Per-test (during the loop):

```
nix develop --command cargo test -p sunset-sync --features test-helpers <test_name> -- --nocapture
```

Whole sync crate (after each task):

```
nix develop --command cargo test -p sunset-sync --features test-helpers
```

Workspace gate (before opening the PR):

```
nix develop --command cargo test --workspace --all-features
nix develop --command cargo clippy --workspace --all-features --all-targets -- -D warnings
nix develop --command cargo fmt --all --check
```

---

### Task 1: Add `own_published_filters()` helper

Walks our own `_sunset-sync/subscribe` entries and returns the parsed filters. Both Change 1 (PeerHello) and Change 2 (anti-entropy) call this.

**Files:**
- Modify: `crates/sunset-sync/src/engine.rs` — append helper alongside `replay_existing_subscriptions` (around line 650).
- Test: `crates/sunset-sync/src/engine.rs` — append unit test in the `mod tests` block.

- [ ] **Step 1: Write the failing test.**

Append at the bottom of `mod tests` (after the last existing `#[tokio::test(...)]` in the module, before the closing `}`):

```rust
    #[tokio::test(flavor = "current_thread")]
    async fn own_published_filters_returns_self_authored_subscribe_entries_only() {
        use sunset_store::{ContentBlock, SignedKvEntry, Store as _};

        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let engine = Rc::new(make_engine("alice", b"alice"));

                // Self-authored subscribe entry — should be returned.
                let mine = Filter::NamePrefix(Bytes::from_static(b"room/"));
                let mine_bytes = postcard::to_stdvec(&mine).unwrap();
                let mine_block = ContentBlock {
                    data: Bytes::from(mine_bytes),
                    references: vec![],
                };
                let mine_entry = SignedKvEntry {
                    verifying_key: vk(b"alice"),
                    name: Bytes::from_static(reserved::SUBSCRIBE_NAME),
                    value_hash: mine_block.hash(),
                    priority: 1,
                    expires_at: None,
                    signature: Bytes::new(),
                };
                engine
                    .store
                    .insert(mine_entry, Some(mine_block))
                    .await
                    .unwrap();

                // Someone else's subscribe entry — must NOT be returned.
                let theirs = Filter::NamePrefix(Bytes::from_static(b"other/"));
                let theirs_bytes = postcard::to_stdvec(&theirs).unwrap();
                let theirs_block = ContentBlock {
                    data: Bytes::from(theirs_bytes),
                    references: vec![],
                };
                let theirs_entry = SignedKvEntry {
                    verifying_key: vk(b"bob"),
                    name: Bytes::from_static(reserved::SUBSCRIBE_NAME),
                    value_hash: theirs_block.hash(),
                    priority: 1,
                    expires_at: None,
                    signature: Bytes::new(),
                };
                engine
                    .store
                    .insert(theirs_entry, Some(theirs_block))
                    .await
                    .unwrap();

                // Self-authored entry under a non-subscribe name — must NOT be returned.
                let chat_block = ContentBlock {
                    data: Bytes::from_static(b"hi"),
                    references: vec![],
                };
                let chat_entry = SignedKvEntry {
                    verifying_key: vk(b"alice"),
                    name: Bytes::from_static(b"room/msg/1"),
                    value_hash: chat_block.hash(),
                    priority: 1,
                    expires_at: None,
                    signature: Bytes::new(),
                };
                engine
                    .store
                    .insert(chat_entry, Some(chat_block))
                    .await
                    .unwrap();

                let filters = engine.own_published_filters().await;
                assert_eq!(filters, vec![mine]);
            })
            .await;
    }
```

- [ ] **Step 2: Run the test — expect compile failure.**

```
nix develop --command cargo test -p sunset-sync --features test-helpers own_published_filters_returns_self_authored_subscribe_entries_only
```

Expected: `no method named own_published_filters found`.

- [ ] **Step 3: Add the helper.**

In `crates/sunset-sync/src/engine.rs`, immediately after `replay_existing_subscriptions` (which ends at line 650 in the current file), insert:

```rust
    /// Walk the local store for `_sunset-sync/subscribe` entries authored
    /// by `self.local_peer` and return their parsed filters. Used by
    /// `PeerHello` and `tick_anti_entropy` to fire per-filter digests so
    /// a (re)connected client catches up on whatever it missed under
    /// each of its own published interests.
    ///
    /// Other peers' subscribe entries are intentionally skipped — they
    /// own their own catch-up, and this engine has no signing key for
    /// them. Iteration errors and parse errors are logged-and-skipped,
    /// matching `replay_existing_subscriptions`.
    async fn own_published_filters(&self) -> Vec<Filter> {
        use futures::StreamExt as _;
        let mut out = Vec::new();
        let filter = Filter::Namespace(Bytes::from_static(reserved::SUBSCRIBE_NAME));
        let mut entries = match self.store.iter(filter).await {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(error = %e, "own_published_filters: store iteration");
                return out;
            }
        };
        while let Some(item) = entries.next().await {
            let entry = match item {
                Ok(e) => e,
                Err(e) => {
                    tracing::warn!(error = %e, "own_published_filters: entry");
                    continue;
                }
            };
            if entry.verifying_key != self.local_peer.0 {
                continue;
            }
            let block = match self.store.get_content(&entry.value_hash).await {
                Ok(Some(b)) => b,
                Ok(None) => continue,
                Err(e) => {
                    tracing::warn!(error = %e, "own_published_filters: get_content");
                    continue;
                }
            };
            if let Ok(parsed) = parse_subscription_entry(&entry, &block) {
                out.push(parsed);
            }
        }
        out
    }
```

Note: `futures::StreamExt` is already imported elsewhere in this file (used by `replay_existing_subscriptions`). The local `use` keeps the helper self-contained; if rustfmt/clippy complains about a redundant import, drop the `use` line. Logging matches the `tracing::warn!(error = %e, "<function>: <what>")` style used by `replay_existing_subscriptions` (PR #11 migrated this crate from `eprintln!` to `tracing`).

- [ ] **Step 4: Run the test — expect pass.**

```
nix develop --command cargo test -p sunset-sync --features test-helpers own_published_filters_returns_self_authored_subscribe_entries_only
```

Expected: PASS.

- [ ] **Step 5: Run the full sync crate test suite.**

```
nix develop --command cargo test -p sunset-sync --features test-helpers
```

Expected: all green.

- [ ] **Step 6: Commit.**

```bash
git add crates/sunset-sync/src/engine.rs
git commit -m "$(cat <<'EOF'
sunset-sync: add own_published_filters helper

Walks the store for self-authored _sunset-sync/subscribe entries
and returns their parsed filters. Next two commits wire it into
PeerHello and anti-entropy to drive per-filter catch-up.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 2: Fire per-filter digests on `PeerHello` (Change 1)

This is the targeted reconnect fix. Every redial — supervisor backoff, relay restart, network blip recovery — runs `PeerHello` in the engine, so the catch-up is automatic.

**Files:**
- Modify: `crates/sunset-sync/src/engine.rs:585-587` (the body of the `PeerHello` arm in `handle_inbound_event`).
- Test: `crates/sunset-sync/src/engine.rs` — append unit test in the `mod tests` block.

- [ ] **Step 1: Write the failing test.**

The test drives `handle_inbound_event` directly with a `PeerHello`, with a captured outbound channel. Append after the test from Task 1:

```rust
    #[tokio::test(flavor = "current_thread")]
    async fn peer_hello_fires_filter_digest_for_own_published_subscriptions() {
        use crate::peer::InboundEvent;
        use crate::transport::TransportKind;
        use sunset_store::{ContentBlock, SignedKvEntry, Store as _};

        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let engine = Rc::new(make_engine("alice", b"alice"));

                // Pre-publish one self-authored subscribe entry over a chat-like filter.
                let chat_filter = Filter::NamePrefix(Bytes::from_static(b"room/"));
                let filter_bytes = postcard::to_stdvec(&chat_filter).unwrap();
                let block = ContentBlock {
                    data: Bytes::from(filter_bytes),
                    references: vec![],
                };
                let entry = SignedKvEntry {
                    verifying_key: vk(b"alice"),
                    name: Bytes::from_static(reserved::SUBSCRIBE_NAME),
                    value_hash: block.hash(),
                    priority: 1,
                    expires_at: None,
                    signature: Bytes::new(),
                };
                engine.store.insert(entry, Some(block)).await.unwrap();

                // Drive PeerHello with a captured outbound channel.
                let (tx, mut rx) = mpsc::unbounded_channel::<SyncMessage>();
                engine
                    .handle_inbound_event(InboundEvent::PeerHello {
                        peer_id: PeerId(vk(b"relay")),
                        conn_id: ConnectionId::for_test(1),
                        kind: TransportKind::Primary,
                        out_tx: tx,
                        registered: None,
                    })
                    .await;

                // The engine sends both digests synchronously before
                // `handle_inbound_event` returns (the per-peer fire path is
                // async-but-in-memory: `send_filter_digest` awaits the
                // state lock and then non-blocking-sends to an unbounded
                // mpsc). By the time we get here, every digest the engine
                // intends to fire is already queued — drain with try_recv
                // so a missing fire produces an assertion failure, not a
                // hang.
                let mut filters = std::collections::HashSet::new();
                loop {
                    match rx.try_recv() {
                        Ok(SyncMessage::DigestExchange { filter, .. }) => {
                            filters.insert(filter);
                        }
                        Ok(other) => panic!("expected DigestExchange, got {other:?}"),
                        Err(_) => break,
                    }
                }

                let bootstrap = Filter::Namespace(Bytes::from_static(reserved::SUBSCRIBE_NAME));
                assert!(
                    filters.contains(&bootstrap),
                    "bootstrap digest must still fire (got {filters:?})"
                );
                assert!(
                    filters.contains(&chat_filter),
                    "per-filter digest must fire for own published subscription (got {filters:?})"
                );
            })
            .await;
    }
```

- [ ] **Step 2: Run the test — expect failure (assertion miss, not a hang).**

```
nix develop --command cargo test -p sunset-sync --features test-helpers peer_hello_fires_filter_digest_for_own_published_subscriptions
```

Expected: assertion failure on the `chat_filter` line — the bootstrap digest is in the drained set, but the per-filter digest is missing because `PeerHello` doesn't fire it yet.

- [ ] **Step 3: Wire per-filter digests into the `PeerHello` handler.**

In `crates/sunset-sync/src/engine.rs`, in `handle_inbound_event`'s `InboundEvent::PeerHello` arm (around line 585), replace the trailing line:

```rust
                // Fire bootstrap digest exchange on the subscribe namespace.
                self.send_bootstrap_digest(&peer_id).await;
```

with:

```rust
                // Fire bootstrap digest exchange on the subscribe namespace,
                // then a per-filter digest for each of our own published
                // subscriptions. The latter is what makes a (re)connected
                // client catch up on chat (and any other room-namespace)
                // entries that landed at the relay while we were offline.
                self.send_bootstrap_digest(&peer_id).await;
                for filter in self.own_published_filters().await {
                    self.send_filter_digest(&peer_id, &filter).await;
                }
```

- [ ] **Step 4: Run the test — expect pass.**

```
nix develop --command cargo test -p sunset-sync --features test-helpers peer_hello_fires_filter_digest_for_own_published_subscriptions
```

Expected: PASS, both digests received.

- [ ] **Step 5: Run the full sync crate test suite.**

```
nix develop --command cargo test -p sunset-sync --features test-helpers
```

Expected: all green.

- [ ] **Step 6: Commit.**

```bash
git add crates/sunset-sync/src/engine.rs
git commit -m "$(cat <<'EOF'
sunset-sync: fire per-filter digest on PeerHello

After every (re)connection's PeerHello, walk our own published
subscribe entries and fire one DigestExchange per filter to the
freshly-connected peer. This is what makes a client catch up on
chat that arrived at the relay while it was disconnected.

Frontend hosts (Gleam, future TUI) get this for free — no API
change, no extra calls required.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 3: Fire per-filter digests on `tick_anti_entropy` (Change 2)

Steady-state belt-and-suspenders for cases where a connection stays alive but data was missed (transient relay state bug, future race, anything that punctures push routing).

**Files:**
- Modify: `crates/sunset-sync/src/engine.rs:437-459` — replace the master `tick_anti_entropy` body wholesale.
- Test: `crates/sunset-sync/src/engine.rs` — append unit test.

- [ ] **Step 1: Write the failing test.**

Append after the test from Task 2:

```rust
    #[tokio::test(flavor = "current_thread")]
    async fn anti_entropy_tick_fires_filter_digest_for_own_published_subscriptions() {
        use sunset_store::{ContentBlock, SignedKvEntry, Store as _};

        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let engine = Rc::new(make_engine("alice", b"alice"));

                // Pre-publish one self-authored subscribe entry.
                let chat_filter = Filter::NamePrefix(Bytes::from_static(b"room/"));
                let filter_bytes = postcard::to_stdvec(&chat_filter).unwrap();
                let block = ContentBlock {
                    data: Bytes::from(filter_bytes),
                    references: vec![],
                };
                let entry = SignedKvEntry {
                    verifying_key: vk(b"alice"),
                    name: Bytes::from_static(reserved::SUBSCRIBE_NAME),
                    value_hash: block.hash(),
                    priority: 1,
                    expires_at: None,
                    signature: Bytes::new(),
                };
                engine.store.insert(entry, Some(block)).await.unwrap();

                // Pre-register one connected peer with a captured outbound channel.
                let (tx, mut rx) = mpsc::unbounded_channel::<SyncMessage>();
                let peer = PeerId(vk(b"relay"));
                engine.state.lock().await.peer_outbound.insert(
                    peer.clone(),
                    PeerOutbound {
                        conn_id: ConnectionId::for_test(1),
                        tx,
                    },
                );

                engine.tick_anti_entropy().await;

                // Same drain pattern as the PeerHello test — both digests
                // are queued by the time `tick_anti_entropy` returns.
                let mut filters = std::collections::HashSet::new();
                loop {
                    match rx.try_recv() {
                        Ok(SyncMessage::DigestExchange { filter, .. }) => {
                            filters.insert(filter);
                        }
                        Ok(other) => panic!("expected DigestExchange, got {other:?}"),
                        Err(_) => break,
                    }
                }

                let bootstrap = Filter::Namespace(Bytes::from_static(reserved::SUBSCRIBE_NAME));
                assert!(
                    filters.contains(&bootstrap),
                    "bootstrap digest must fire (got {filters:?})"
                );
                assert!(
                    filters.contains(&chat_filter),
                    "per-filter digest must fire on anti-entropy tick (got {filters:?})"
                );
            })
            .await;
    }
```

- [ ] **Step 2: Run the test — expect failure (assertion miss).**

```
nix develop --command cargo test -p sunset-sync --features test-helpers anti_entropy_tick_fires_filter_digest_for_own_published_subscriptions
```

Expected: assertion failure on the `chat_filter` line.

- [ ] **Step 3: Extend `tick_anti_entropy` to fire per-published-filter digests.**

Replace the existing `tick_anti_entropy` body in `crates/sunset-sync/src/engine.rs` (lines 437–459 on master, which inlines its own `build_digest` over `bootstrap_filter` and broadcasts to every peer) with:

```rust
    async fn tick_anti_entropy(&self) {
        let peers: Vec<PeerId> = {
            let state = self.state.lock().await;
            state.peer_outbound.keys().cloned().collect()
        };
        if peers.is_empty() {
            return;
        }
        let bootstrap_filter = self.config.bootstrap_filter.clone();
        let own_filters = self.own_published_filters().await;
        for peer in &peers {
            self.send_filter_digest(peer, &bootstrap_filter).await;
            for filter in &own_filters {
                self.send_filter_digest(peer, filter).await;
            }
        }
    }
```

This routes the bootstrap-filter digest through the shared `send_filter_digest` helper (which already builds the bloom and dispatches to one peer), then fires one digest per own published filter to each peer in the same loop. Same wire bytes as before for the bootstrap path; new bytes only when there's an own published filter.

- [ ] **Step 4: Run the test — expect pass.**

```
nix develop --command cargo test -p sunset-sync --features test-helpers anti_entropy_tick_fires_filter_digest_for_own_published_subscriptions
```

Expected: PASS.

- [ ] **Step 5: Run the full sync crate test suite.**

```
nix develop --command cargo test -p sunset-sync --features test-helpers
```

Expected: all green. The existing `tick_anti_entropy_with_no_peers_is_noop` test still passes because of the early `peers.is_empty()` return.

- [ ] **Step 6: Commit.**

```bash
git add crates/sunset-sync/src/engine.rs
git commit -m "$(cat <<'EOF'
sunset-sync: fire per-filter digest on anti-entropy tick

In addition to the existing bootstrap-filter digest, every
anti-entropy tick now fires one DigestExchange per own-published
subscription to every connected peer. Steady-state catch-up for
data missed despite a healthy connection (transient relay bug,
future race, etc.).

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 4: Workspace gates + e2e regression spot-check + PR

- [ ] **Step 1: Workspace test sweep.**

```
nix develop --command cargo test --workspace --all-features
```

Expected: all green.

- [ ] **Step 2: Clippy.**

```
nix develop --command cargo clippy --workspace --all-features --all-targets -- -D warnings
```

Expected: no warnings.

- [ ] **Step 3: Format check.**

```
nix develop --command cargo fmt --all --check
```

Expected: clean. If diff is reported, run `nix develop --command cargo fmt --all` and re-run the check.

- [ ] **Step 4: e2e relay-restart spot-check.**

The web e2e suite already exercises the reconnect path. The flake exposes the runner via `nix run .#web-test`, which builds `node_modules` + the prod dist and invokes Playwright with Nix-store browsers. Run only the restart spec:

```
nix run .#web-test -- relay_restart.spec.js
```

(Playwright accepts a positional spec-file argument, which is the simplest way to scope the run.) Expected: passes. Read the spec output for any new "messages missing after reconnect" assertion failures — there shouldn't be any, but this is the closest thing we have to a behavioral confirmation that the catch-up actually fires end-to-end. If the suite doesn't exercise post-restart traffic delivery, that's an accepted gap (the unit tests in Tasks 2 and 3 are the load-bearing coverage); note it in the PR description rather than expanding scope here.

- [ ] **Step 5: Open PR.**

Push the branch and open a PR. Use the design doc as the body's anchor:

```bash
git push -u origin HEAD
gh pr create --title "sunset-sync: catch up on reconnect via per-filter digests" --body "$(cat <<'EOF'
## Summary

Closes the reconnect catch-up gap. PR #10 made `publish_subscription`
fire a per-filter digest, which closed the *initial-connection*
late-subscriber gap. But the supervisor's reconnects don't re-fire
`publish_subscription` (the engine handles redials internally; Gleam
never sees a second `RelayConnectResult`), so a client that briefly
disconnected and reconnected never asked the relay for the chat
data it missed.

This PR fires per-filter digests at the engine boundaries that *do*
run on every reconnect:

1. Adds `own_published_filters()` — walks self-authored
   `_sunset-sync/subscribe` entries and returns their parsed filters.
2. On `PeerHello`, fires `send_filter_digest` for each own published
   filter, in addition to the existing bootstrap digest.
3. On each anti-entropy tick, also fires per-published-filter
   digests to every connected peer.

No protocol-version bump, no wire-format change, no frontend change.
Frontend hosts (Gleam, future TUI, future mod) keep their existing
`add_relay` / `publish_subscription` flow.

Design: `docs/superpowers/specs/2026-05-02-sync-catchup-on-reconnect-design.md`

## Test plan

- [x] Unit: `own_published_filters_returns_self_authored_subscribe_entries_only`
- [x] Unit: `peer_hello_fires_filter_digest_for_own_published_subscriptions`
- [x] Unit: `anti_entropy_tick_fires_filter_digest_for_own_published_subscriptions`
- [x] Workspace: `cargo test --workspace --all-features`
- [x] `cargo clippy --workspace --all-features --all-targets -- -D warnings`
- [x] `cargo fmt --all --check`
- [x] e2e: relay-restart spec passes (best-effort; suite does not yet
      assert post-restart message delivery — see follow-up note)

🤖 Generated with [Claude Code](https://claude.com/claude-code)
EOF
)"
```

Expected: PR opened. Print the URL.

---

## Self-review

**Spec coverage:**

| Spec section                          | Implementing task |
|---------------------------------------|-------------------|
| Generalized digest helper             | already on master (PR #10) |
| Change 1 — fire on `PeerAdded`        | Task 2            |
| Change 2 — extend `tick_anti_entropy` | Task 3            |
| Common helper `own_published_filters` | Task 1            |
| Unit tests `peer_hello_fires_...`     | Task 2            |
| Unit tests `anti_entropy_tick_...`    | Task 3            |
| Error handling — log & continue       | Task 1 helper body |
| e2e relay-restart spot-check          | Task 4 step 4     |

**Placeholder scan:** None. Every step ships actual code or an exact command.

**Type / name consistency:**

- `send_filter_digest(to: &PeerId, filter: &Filter)` — exists on master (PR #10) and is called in Tasks 2, 3.
- `own_published_filters() -> Vec<Filter>` — same signature in Tasks 1, 2, 3.
- `Filter::Namespace(Bytes::from_static(reserved::SUBSCRIBE_NAME))` for the bootstrap filter, consistent across the helper body and the test assertions.
- `PeerOutbound { conn_id, tx }` — matches the existing struct (line ~137 in current `engine.rs`).
- `ConnectionId::for_test(N)` — matches the existing test helpers.
- Logging style: `tracing::warn!(error = %e, "<function>: <what>")` — matches `replay_existing_subscriptions` after PR #11's logging migration. **No `eprintln!`.**
