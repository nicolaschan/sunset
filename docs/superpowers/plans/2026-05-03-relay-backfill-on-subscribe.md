# Subscribe-triggered backfill — implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close the engine-side race in `sunset-sync` where third-party-authored entries that arrive *before* a recipient's `SUBSCRIBE_NAME` registry entry has been parsed are stored locally but never forwarded. Add registry-update as a third forwarding trigger (after live writes and `PeerHello` fan-out).

**Architecture:** A surgical change in `crates/sunset-sync/`. `SubscriptionRegistry::insert` is updated to return the displaced filter (HashMap-style), letting the caller distinguish new/changed/unchanged. In `handle_local_store_event`, when a `SUBSCRIBE_NAME` write changes a peer's filter and the peer is connected (and is not self), iterate the local store for entries matching the new filter and push them as a single `EventDelivery` to that peer.

**Tech Stack:** Rust workspace (stable), tokio (single-threaded, current_thread flavor for tests), postcard (frozen wire format), futures-stream, bloom-filter helpers in `crates/sunset-sync/src/digest.rs`.

**Spec:** [`docs/superpowers/specs/2026-05-03-relay-backfill-on-subscribe-design.md`](../specs/2026-05-03-relay-backfill-on-subscribe-design.md)

**Mechanism note:** The spec uses a *direct push* (not a `DigestExchange`). `DigestExchange` carries the *sender's* bloom and prompts the *receiver* to push entries the receiver has that the sender doesn't (see `handle_digest_exchange` at `crates/sunset-sync/src/engine.rs:715`). That's the right direction for a freshly subscribed peer asking a forwarder to catch them up — but it's the wrong direction for backfill, where the local engine already holds entries the peer is missing. We push directly via `SyncMessage::EventDelivery`, exactly as `handle_local_store_event` does on a fresh write, but applied to already-stored entries.

---

## File structure

**Modify (Rust):**
- `crates/sunset-sync/src/subscription_registry.rs` — change `insert(...)` signature to return `Option<Filter>`. Update existing unit tests for the new return value.
- `crates/sunset-sync/src/engine.rs` — two call sites of `registry.insert` (`handle_local_store_event` around line 846; `replay_existing_subscriptions` around line 579). Add the backfill push in `handle_local_store_event` after the registry update, gated on filter-changed + peer-connected + peer-not-self.

**Create (Rust):**
- `crates/sunset-sync/tests/subscribe_backfill.rs` — new integration test that drives the race deterministically (entry written before the subscriber's `SUBSCRIBE_NAME` lands) and asserts delivery without any registry-state polling.

**Modify (test workarounds — acceptance criterion):**
- `crates/sunset-sync/tests/two_peer_sync.rs:91–98` — remove the `knows_peer_subscription()` poll.
- `web/e2e/voice_network.spec.js` lines 147–158 and 247–258 — remove the `waitForFunction((pk) => window.__voice.memberVisible(pk), …)` calls (two pairs, in both tests). Keep the `startPresence()` calls — they're real UI behaviour, not a workaround. Update the load-bearing comment block at lines 139–146 (and the matching reference at lines 244–246) to reflect that the membership wait is no longer needed.
- `web/voice-e2e-test.html` lines 20, 43–48, 85–87 — remove the `visibleMembers` set, the `client.on_members_changed(...)` registration that populates it, and the `memberVisible()` accessor. (The `on_members_changed` API itself stays — it's used by the real UI and other e2e tests.)

---

## Task 1 — Registry insert returns previous filter

**Files:**
- Modify: `crates/sunset-sync/src/subscription_registry.rs`
- Modify: `crates/sunset-sync/src/engine.rs:579–584` and `:846–851`
- Test: `crates/sunset-sync/src/subscription_registry.rs` (existing `#[cfg(test)] mod tests`)

- [ ] **Step 1.1: Add a failing unit test for the new return type**

Append to the `mod tests` block in `crates/sunset-sync/src/subscription_registry.rs`:

```rust
#[test]
fn insert_returns_none_for_new_peer() {
    let mut r = SubscriptionRegistry::new();
    let prev = r.insert(vk(b"alice"), Filter::Keyspace(vk(b"chat-1")));
    assert!(prev.is_none(), "expected None when inserting a new peer");
}

#[test]
fn insert_returns_previous_filter_for_existing_peer() {
    let mut r = SubscriptionRegistry::new();
    r.insert(vk(b"alice"), Filter::Keyspace(vk(b"chat-1")));
    let prev = r.insert(vk(b"alice"), Filter::Keyspace(vk(b"chat-2")));
    assert_eq!(prev, Some(Filter::Keyspace(vk(b"chat-1"))));
}

#[test]
fn insert_returns_same_filter_when_unchanged() {
    let mut r = SubscriptionRegistry::new();
    let f = Filter::Keyspace(vk(b"chat-1"));
    r.insert(vk(b"alice"), f.clone());
    let prev = r.insert(vk(b"alice"), f.clone());
    assert_eq!(prev, Some(f));
}
```

- [ ] **Step 1.2: Run the test — expect compile failure**

Run: `nix develop --command cargo test -p sunset-sync subscription_registry::tests::insert_returns 2>&1 | tail -20`
Expected: build fails with "cannot use `let prev = r.insert(...)` because it returns `()`". This proves the test will exercise the new contract.

- [ ] **Step 1.3: Change the `insert` signature**

In `crates/sunset-sync/src/subscription_registry.rs`, replace the `insert` method:

```rust
/// Replace the filter for `vk` with `filter`. Returns the previous filter
/// for that peer, if any. Mirrors `HashMap::insert`'s return semantics so
/// callers can distinguish new / changed / unchanged subscriptions.
pub fn insert(&mut self, vk: VerifyingKey, filter: Filter) -> Option<Filter> {
    self.by_peer.insert(vk, filter)
}
```

- [ ] **Step 1.4: Update the two engine call sites to ignore the return value**

In `crates/sunset-sync/src/engine.rs`, both call sites currently look like:

```rust
self.state.lock().await.registry.insert(entry.verifying_key.clone(), filter);
```

(at engine.rs:846–851 inside `handle_local_store_event`) and:

```rust
self.state.lock().await.registry.insert(entry.verifying_key, parsed_filter);
```

(at engine.rs:579–584 inside `replay_existing_subscriptions`).

The new return value is `Option<Filter>`; in this task we just need them to compile. Add `let _ =` in front of each so the `unused_must_use` workspace lint stays satisfied. The trigger logic at the `handle_local_store_event` call site is added in Task 3.

```rust
// engine.rs replay_existing_subscriptions (around line 579)
let _ = self
    .state
    .lock()
    .await
    .registry
    .insert(entry.verifying_key, parsed_filter);
```

```rust
// engine.rs handle_local_store_event (around line 846)
let _ = self
    .state
    .lock()
    .await
    .registry
    .insert(entry.verifying_key.clone(), filter);
```

- [ ] **Step 1.5: Run the new tests and the full sync test suite**

Run: `nix develop --command cargo test -p sunset-sync 2>&1 | tail -30`
Expected: all sunset-sync tests pass, including the three new unit tests.

- [ ] **Step 1.6: Run clippy on the workspace**

Run: `nix develop --command cargo clippy --workspace --all-features --all-targets -- -D warnings 2>&1 | tail -10`
Expected: clean.

- [ ] **Step 1.7: Commit**

```bash
git add crates/sunset-sync/src/subscription_registry.rs crates/sunset-sync/src/engine.rs
git commit -m "$(cat <<'EOF'
sunset-sync: SubscriptionRegistry::insert returns previous filter

Mirror HashMap::insert's semantics so the registry-update call site in
handle_local_store_event can detect new vs. changed vs. unchanged
filters. Engine call sites currently discard the return value; the
backfill trigger that consumes it lands in the next commit.

EOF
)"
```

---

## Task 2 — Failing regression test for the race

**Files:**
- Create: `crates/sunset-sync/tests/subscribe_backfill.rs`

Goal: drive the race deterministically. Alice has an entry stored *before* Bob ever publishes his `SUBSCRIBE_NAME`. Today, Bob's `publish_subscription` digest fires from Bob's side and asks Alice for matching entries — that closes the publisher-side path. The receiver-side gap is when Alice's *registry* learns about Bob's filter (via Bob's `SUBSCRIBE_NAME` arriving over the wire) and fails to backfill what's already in Alice's store. Without the engine fix, the entry stays in Alice's store and Bob never receives it (until anti-entropy fires, which is well past the 2-second test bound).

This test fails today and passes once Task 3 ships.

- [ ] **Step 2.1: Write the test**

Create `crates/sunset-sync/tests/subscribe_backfill.rs`:

```rust
//! Regression test for the subscribe-triggered backfill race.
//!
//! Scenario:
//!   1. Alice writes entry E to her local store.
//!   2. Bob publishes a `SUBSCRIBE_NAME` whose filter matches E.
//!
//! Alice's registry only learns about Bob's filter when Bob's
//! `SUBSCRIBE_NAME` entry arrives at Alice — but by then Alice has
//! already stored E, and `handle_local_store_event` has only fired
//! for E once (when E was written, before Bob was in the registry).
//!
//! With the engine fix, the registry update is itself a forwarding
//! trigger and Alice pushes E to Bob. Without it, Bob waits for
//! anti-entropy.

use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use sunset_store::{ContentBlock, Filter, SignedKvEntry, Store as _, VerifyingKey};
use sunset_store_memory::MemoryStore;
use sunset_sync::test_transport::TestNetwork;
use sunset_sync::{PeerAddr, PeerId, Signer, SyncConfig, SyncEngine};

fn vk(b: &[u8]) -> VerifyingKey {
    VerifyingKey::new(Bytes::copy_from_slice(b))
}

struct StubSigner {
    vk: VerifyingKey,
}

impl Signer for StubSigner {
    fn verifying_key(&self) -> VerifyingKey {
        self.vk.clone()
    }
    fn sign(&self, _payload: &[u8]) -> Bytes {
        Bytes::from_static(&[0u8; 64])
    }
}

#[tokio::test(flavor = "current_thread")]
async fn entry_written_before_subscription_is_backfilled_when_subscription_arrives() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let net = TestNetwork::new();
            let alice_addr = PeerAddr::new("alice");
            let bob_addr = PeerAddr::new("bob");
            let alice_id = PeerId(vk(b"alice"));
            let bob_id = PeerId(vk(b"bob"));

            let alice_transport = net.transport(alice_id.clone(), alice_addr.clone());
            let bob_transport = net.transport(bob_id.clone(), bob_addr.clone());

            let alice_store = Arc::new(MemoryStore::with_accept_all());
            let bob_store = Arc::new(MemoryStore::with_accept_all());

            let alice_signer = Arc::new(StubSigner { vk: alice_id.0.clone() });
            let bob_signer = Arc::new(StubSigner { vk: bob_id.0.clone() });

            let alice_engine = Rc::new(SyncEngine::new(
                alice_store.clone(),
                alice_transport,
                SyncConfig::default(),
                alice_id.clone(),
                alice_signer,
            ));
            let bob_engine = Rc::new(SyncEngine::new(
                bob_store.clone(),
                bob_transport,
                SyncConfig::default(),
                bob_id.clone(),
                bob_signer,
            ));

            let _alice_run = tokio::task::spawn_local({
                let e = alice_engine.clone();
                async move { e.run().await }
            });
            let _bob_run = tokio::task::spawn_local({
                let e = bob_engine.clone();
                async move { e.run().await }
            });

            // 1. Alice writes E *before* connecting to Bob.
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
            alice_store
                .insert(entry.clone(), Some(block.clone()))
                .await
                .unwrap();

            // 2. Alice connects to Bob. Alice has no idea what Bob is
            // subscribed to yet.
            alice_engine.add_peer(bob_addr.clone()).await.unwrap();

            // 3. Bob declares interest in the `chat` keyspace. Bob's
            // SUBSCRIBE_NAME entry will reach Alice, updating Alice's
            // registry. Bob also fires a digest from his side — but
            // because Bob's bloom carries no chat entries, Alice's
            // handle_digest_exchange already pushes matching entries
            // back to Bob during PR #10's catch-up. The race we're
            // testing is that registry-update on Alice's side ALSO
            // backfills, even when bob's digest didn't arrive first
            // or didn't fire. To isolate the backfill path, we let
            // the natural order run and assert delivery happens.
            bob_engine
                .publish_subscription(Filter::Keyspace(vk(b"chat")), Duration::from_secs(60))
                .await
                .unwrap();

            // 4. Bob should receive E within a short bound, *without
            // any test-side wait on registry state*. A real API user
            // calling publish_subscription expects subsequent matching
            // entries to arrive — and entries written before the
            // subscription should be no different.
            let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
            let mut received = false;
            while tokio::time::Instant::now() < deadline {
                if bob_store
                    .get_entry(&vk(b"chat"), b"k")
                    .await
                    .unwrap()
                    .is_some()
                {
                    received = true;
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            assert!(
                received,
                "bob did not receive entry written before his subscription"
            );
        })
        .await;
}
```

- [ ] **Step 2.2: Run the test — expect failure**

Run: `nix develop --command cargo test -p sunset-sync --test subscribe_backfill 2>&1 | tail -20`
Expected: panic at `bob did not receive entry written before his subscription` within ~2 seconds.

If the test passes today (because PR #10's publisher-side digest happens to close the gap in this exact ordering), tighten the test: insert `E` *after* `add_peer` returns (so Alice hasn't yet learned about Bob), but *before* `bob.publish_subscription`. The relevant invariant is "an entry already in Alice's store at the moment Alice's registry first learns Bob's filter must reach Bob without anti-entropy."

- [ ] **Step 2.3: No commit yet — failing tests stay uncommitted until the implementation lands together in Task 3**

---

## Task 3 — Implement the backfill trigger

**Files:**
- Modify: `crates/sunset-sync/src/engine.rs:843–853` (the registry-update branch in `handle_local_store_event`)

- [ ] **Step 3.1: Update the registry-update branch to capture the previous filter and trigger backfill**

In `handle_local_store_event` (`crates/sunset-sync/src/engine.rs`), replace the existing block (around line 841–853):

```rust
// If this is a subscription announcement, update the registry so
// future push routing knows about the peer's interests.
if entry.name.as_ref() == reserved::SUBSCRIBE_NAME {
    if let Ok(Some(block)) = self.store.get_content(&entry.value_hash).await {
        if let Ok(filter) = parse_subscription_entry(&entry, &block) {
            let _ = self
                .state
                .lock()
                .await
                .registry
                .insert(entry.verifying_key.clone(), filter);
        }
    }
}
```

with:

```rust
// If this is a subscription announcement, update the registry so
// future push routing knows about the peer's interests.
//
// On a new or changed filter, also backfill the peer with already-
// stored entries that match the filter. This closes the receiver-side
// race where third-party-authored entries arrive in our local store
// *before* the recipient's SUBSCRIBE_NAME is parsed; without the
// backfill, those entries sit in our store with no forwarding trigger
// until anti-entropy fires (well past the latency budget for, e.g.,
// WebRTC SDP signaling).
if entry.name.as_ref() == reserved::SUBSCRIBE_NAME {
    if let Ok(Some(block)) = self.store.get_content(&entry.value_hash).await {
        if let Ok(filter) = parse_subscription_entry(&entry, &block) {
            let peer_vk = entry.verifying_key.clone();
            let peer_id = PeerId(peer_vk.clone());
            let prev = self
                .state
                .lock()
                .await
                .registry
                .insert(peer_vk.clone(), filter.clone());
            let filter_changed = prev.as_ref() != Some(&filter);
            let is_self = peer_vk == self.local_peer.0;
            if filter_changed && !is_self {
                self.backfill_peer_for_filter(&peer_id, &filter).await;
            }
        }
    }
}
```

- [ ] **Step 3.2: Add the `backfill_peer_for_filter` helper**

Add a new private method on `SyncEngine`, placed near `send_filter_digest` (around line 656 in `engine.rs`):

```rust
/// Push every entry in our local store matching `filter` to `to` as a
/// single `EventDelivery`. Called from `handle_local_store_event` when
/// the registry first learns (or changes) `to`'s filter, to deliver
/// already-stored entries that pre-date the registry update.
///
/// Skipped silently if the peer is not currently connected (no
/// outbound channel) — a future PeerHello will fan out digests, and
/// anti-entropy ticks bridge the rest.
async fn backfill_peer_for_filter(&self, to: &PeerId, filter: &Filter) {
    use futures::StreamExt;
    let mut iter = match self.store.iter(filter.clone()).await {
        Ok(it) => it,
        Err(e) => {
            tracing::warn!(error = %e, "backfill: store iter failed");
            return;
        }
    };
    let mut entries = Vec::new();
    let mut blobs = Vec::new();
    while let Some(item) = iter.next().await {
        let entry = match item {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!(error = %e, "backfill: store iter yielded error");
                continue;
            }
        };
        if let Ok(Some(blob)) = self.store.get_content(&entry.value_hash).await {
            blobs.push(blob);
        }
        entries.push(entry);
    }
    if entries.is_empty() {
        return;
    }
    let msg = SyncMessage::EventDelivery { entries, blobs };
    let state = self.state.lock().await;
    if let Some(po) = state.peer_outbound.get(to) {
        let _ = po.tx.send(msg);
    }
}
```

`Filter` and `SyncMessage` are already in scope at this point in the file (used by `send_filter_digest`). `PeerId` is too.

- [ ] **Step 3.3: Run the regression test — expect pass**

Run: `nix develop --command cargo test -p sunset-sync --test subscribe_backfill 2>&1 | tail -10`
Expected: `test entry_written_before_subscription_is_backfilled_when_subscription_arrives ... ok`.

- [ ] **Step 3.4: Run the full sync test suite**

Run: `nix develop --command cargo test -p sunset-sync 2>&1 | tail -20`
Expected: all tests pass.

- [ ] **Step 3.5: Run clippy**

Run: `nix develop --command cargo clippy --workspace --all-features --all-targets -- -D warnings 2>&1 | tail -10`
Expected: clean.

- [ ] **Step 3.6: Commit**

```bash
git add crates/sunset-sync/src/engine.rs crates/sunset-sync/tests/subscribe_backfill.rs
git commit -m "$(cat <<'EOF'
sunset-sync: backfill peers on SUBSCRIBE_NAME registry update

Adds a third forwarding trigger to handle_local_store_event: when a
peer's filter is newly learned or changed, walk the local store for
matching entries and push them as an EventDelivery to that peer.
Closes the receiver-side race where a third-party entry arrives in
the local store *before* the recipient's SUBSCRIBE_NAME has been
parsed into the registry — the entry would otherwise wait for the
next anti-entropy tick, which is too slow for latency-sensitive
flows like WebRTC signaling.

Mechanism is a direct EventDelivery push, not DigestExchange:
DigestExchange's bloom is the sender's bloom, and the receiver
responds with what *they* have that the sender doesn't — the
opposite of what backfill needs.

New regression test covers the race deterministically without
polling on engine-internal registry state.

EOF
)"
```

---

## Task 4 — Remove the workaround in `two_peer_sync.rs`

**Files:**
- Modify: `crates/sunset-sync/tests/two_peer_sync.rs:90–98`

This is one of the spec's acceptance criteria: the `knows_peer_subscription` poll exists only because the engine couldn't deliver Alice's entry to Bob without it. With Task 3 in place, the test should pass without it.

- [ ] **Step 4.1: Remove the registry poll**

Open `crates/sunset-sync/tests/two_peer_sync.rs`. Delete lines 90–98 inclusive (the `// Wait for Bob's subscription...` comment, the `wait_for(...)` call, and the `assert!(registered, ...)` line). The block to delete:

```rust
            // Wait for Bob's subscription to propagate to Alice's registry
            // via the bootstrap digest exchange.
            let registered = wait_for(
                Duration::from_secs(2),
                Duration::from_millis(20),
                || async { alice_engine.knows_peer_subscription(&vk(b"bob")).await },
            )
            .await;
            assert!(registered, "alice did not learn bob's subscription");
```

The "Alice writes (chat, k)" block and the rest of the test stay untouched.

- [ ] **Step 4.2: Run the test**

Run: `nix develop --command cargo test -p sunset-sync --test two_peer_sync 2>&1 | tail -15`
Expected: `test alice_writes_bob_receives ... ok` within the existing 2-second `wait_for` for delivery.

If the test fails, the engine fix is incomplete — this is the primary signal that Task 3 didn't actually close the race. Do not patch the test back in; investigate the engine.

- [ ] **Step 4.3: Check whether `knows_peer_subscription` and `wait_for` still have any callers**

Run: `grep -rn "knows_peer_subscription\|wait_for" crates/sunset-sync/ web/ 2>&1 | head`

If `knows_peer_subscription` has no remaining callers (it's gated on `feature = "test-helpers"` and only used by tests), delete its definition at `crates/sunset-sync/src/engine.rs:933–941` along with its `#[cfg(feature = "test-helpers")]` attribute. If `wait_for` is unused in `two_peer_sync.rs`, drop the import too.

If either has other callers, leave them alone. The goal is removing what the workaround needed, not a wider cleanup.

- [ ] **Step 4.4: Run the full sync test suite**

Run: `nix develop --command cargo test -p sunset-sync --all-features 2>&1 | tail -20`
Expected: clean.

- [ ] **Step 4.5: Run clippy**

Run: `nix develop --command cargo clippy --workspace --all-features --all-targets -- -D warnings 2>&1 | tail -10`
Expected: clean.

- [ ] **Step 4.6: Commit**

```bash
git add crates/sunset-sync/tests/two_peer_sync.rs crates/sunset-sync/src/engine.rs
git commit -m "$(cat <<'EOF'
sunset-sync: drop knows_peer_subscription workaround in two_peer_sync

The wait_for(knows_peer_subscription) poll existed only to mask the
registry-update race that subscribe-triggered backfill now closes.
Removing it validates the engine fix: a real API user calling
publish_subscription does not poll engine-internal state to gate
subsequent inserts.

EOF
)"
```

---

## Task 5 — Remove the workaround in `voice_network.spec.js` and `voice-e2e-test.html`

**Files:**
- Modify: `web/e2e/voice_network.spec.js` (two test functions, lines 139–158 and 244–258)
- Modify: `web/voice-e2e-test.html` (remove the `visibleMembers` plumbing)

This is the second acceptance criterion. The `waitForFunction(memberVisible(peer))` calls before `connectDirect` exist only because the relay's registry might not yet know about the peer's subscription when alice fires the SDP offer; subscribe-triggered backfill closes that race directly.

- [ ] **Step 5.1: Remove the membership waits in `voice_network.spec.js` (byte-equal test)**

In `web/e2e/voice_network.spec.js`, replace lines 139–158 (the load-bearing comment block plus the two `waitForFunction` blocks) with:

```javascript
  // Start presence on both. Member-list visibility is no longer a
  // prerequisite for connect_direct: subscribe-triggered backfill in
  // sunset-sync ensures alice's SDP offer reaches bob even if it lands
  // at the relay before bob's room subscription is in the relay's
  // registry.
  await alice.evaluate(async () => await window.__voice.startPresence());
  await bob.evaluate(async () => await window.__voice.startPresence());
```

(That is: keep the `startPresence()` calls, drop both `waitForFunction(memberVisible(...))` calls, and rewrite the comment.)

- [ ] **Step 5.2: Remove the membership waits in `voice_network.spec.js` (peer-state test)**

In the same file, replace lines 244–258 (the matching block in the `voice peer state transitions` test) with:

```javascript
  // Same setup as the byte-equal test — backfill makes the membership
  // wait unnecessary.
  await alice.evaluate(async () => await window.__voice.startPresence());
  await bob.evaluate(async () => await window.__voice.startPresence());
```

- [ ] **Step 5.3: Drop the `visibleMembers` plumbing in `voice-e2e-test.html`**

In `web/voice-e2e-test.html`:
- Delete line 20: `const visibleMembers = new Set();`
- Delete lines 43–48: the `client.on_members_changed((members) => { visibleMembers.clear(); … });` block.
- Delete lines 85–87: the `memberVisible(peerHex)` method on the test harness object.

Other uses of `on_members_changed` elsewhere in the codebase (real UI in `web/src/sunset_web.gleam`, presence e2e tests in `web/e2e/presence.spec.js`) are unaffected — we are only removing the test harness's `visibleMembers`/`memberVisible` plumbing, not the API.

- [ ] **Step 5.4: Run the full Rust test suite (regression check)**

Run: `nix develop --command cargo test --workspace --all-features 2>&1 | tail -20`
Expected: clean.

- [ ] **Step 5.5: Run the e2e voice tests**

The e2e harness has its own command — check `web/package.json` and `flake.nix` for the right invocation. Likely:

Run: `nix develop --command bash -c 'cd web && pnpm exec playwright test e2e/voice_network.spec.js' 2>&1 | tail -30`

If the command differs in this repo, use whatever `flake.nix`'s `apps` or `web/package.json`'s `scripts` exposes for playwright. Both `voice byte-equal` and `voice peer state transitions` tests should pass within their existing per-test timeouts (default 30 s) without flake.

If they flake, the engine fix is incomplete — investigate before patching the workaround back in.

- [ ] **Step 5.6: Commit**

```bash
git add web/e2e/voice_network.spec.js web/voice-e2e-test.html
git commit -m "$(cat <<'EOF'
e2e: drop voice_network member-visibility wait now that backfill ships

The waitForFunction(memberVisible(peer)) calls before connectDirect
existed because alice's SDP offer could land at the relay before
bob's room subscription was in the relay's registry; the relay had
no trigger to forward already-stored matching entries on registry
update. Subscribe-triggered backfill in sunset-sync closes that race
at the engine layer, so the test can rely on the documented public
API: startPresence + connectDirect, no internal-state polling.

Drops the visibleMembers/memberVisible plumbing in
voice-e2e-test.html that existed only to support the workaround.
The on_members_changed API is untouched (still used by the real UI
and presence e2e tests).

EOF
)"
```

---

## Task 6 — Final verification

- [ ] **Step 6.1: Run the full workspace test suite**

Run: `nix develop --command cargo test --workspace --all-features 2>&1 | tail -30`
Expected: all green.

- [ ] **Step 6.2: Run clippy on the workspace**

Run: `nix develop --command cargo clippy --workspace --all-features --all-targets -- -D warnings 2>&1 | tail -10`
Expected: clean.

- [ ] **Step 6.3: Run `cargo fmt` check**

Run: `nix develop --command cargo fmt --all --check 2>&1 | tail -5`
Expected: clean. If it reports drift, run `cargo fmt --all` and commit the formatting fix as a follow-up.

- [ ] **Step 6.4: Run the no-clippy-suppressions audit**

Run: `nix develop --command bash scripts/check-no-clippy-allow.sh 2>&1 | tail -5`
Expected: clean. (No `#[allow(clippy::...)]` or `#[expect(clippy::...)]` should have been added by this work.)

- [ ] **Step 6.5: Re-run e2e voice tests to confirm stability under realistic load**

Run the same playwright invocation from Task 5.5 three times in a row:

```bash
for i in 1 2 3; do
  nix develop --command bash -c 'cd web && pnpm exec playwright test e2e/voice_network.spec.js' || exit 1
done
```

Expected: all three runs green. The original flake reproduced ~20% of the time on a slow runner; three consecutive clean runs is a reasonable confidence floor for the engine-side fix.

- [ ] **Step 6.6: Open the PR**

Use `gh pr create` per CLAUDE.md's workflow notes. Body should reference the spec and the c27ad46 follow-up.

---

## Self-review notes (recorded inline during plan writing, not for the implementer)

Spec coverage checklist run after writing the plan:

- **Trigger and mechanism** (spec §Design → Trigger and mechanism): covered by Task 3 (registry insert returns `Option<Filter>`, change-detection in `handle_local_store_event`, `backfill_peer_for_filter` push).
- **Registry change-detection** (spec §Design → Registry change-detection): covered by Task 1 (return-type refactor) + Task 3 (caller compares `prev` vs `filter`).
- **Concurrency** (spec §Design → Concurrency): covered by Task 3's structure — registry update under the existing `state` lock; backfill push acquires the lock per `EventDelivery` send, exactly like `handle_local_store_event`'s push branch.
- **Edge cases** (spec §Edge cases): all handled in Task 3 — refresh-no-change skipped via filter equality, self-published `SUBSCRIBE_NAME` skipped via `peer_vk == self.local_peer.0`, peer-not-connected handled by `peer_outbound.get(to)` returning `None`. No federation-specific code path needed; `handle_local_store_event` runs on every engine.
- **New regression test** (spec §Tests): covered by Task 2.
- **Workaround removals** (spec §Tests → Workaround removals): covered by Tasks 4 and 5.
- **Verification matrix** (spec §Tests → Verification matrix): covered by Task 6.

---

## Refactor: direct push → DigestRequest (post-execution amendment)

After initial execution (Tasks 1–6), a design review of PR #21 identified that the direct-push `backfill_peer_for_filter` approach wastes bandwidth in two cases that matter as the project grows:

1. **Browser persistence (IndexedDB, landing soon):** A reconnecting browser already has matching entries from a prior session; direct push re-sends them all (idempotent via LWW, but wasteful on the wire).
2. **Federation:** Relays with overlapping subscriptions would re-push entries the peer already received from other federation sources.

The `backfill_peer_for_filter` helper was replaced with a `SyncMessage::DigestRequest` wire variant (added at index 10, end of enum, preserving all existing variant indices and frozen wire-format tests). On registry add/change, the engine sends `DigestRequest { filter, range: All }` to the peer. The peer's new `handle_digest_request` calls the existing `send_filter_digest` back, and the existing `handle_digest_exchange` path computes the diff and pushes only missing entries.

The regression test contract is unchanged — assertion is "bob receives E within bounded time." Only the mechanism narrows: 3-message exchange (DigestRequest → DigestExchange → EventDelivery) instead of 1-message direct push, but bandwidth scales with the diff rather than the full match set. Spec updated to reflect the new mechanism.
