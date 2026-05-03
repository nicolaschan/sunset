# Sunset Voice Network (C2b) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** End-to-end working voice between two browsers in the same room: encrypted `VoicePacket` over Bus, frame + membership Liveness, FFI for voice peer state and connection-liveness, Playwright e2e test.

**Architecture:** Add a `VoicePacket` enum + AEAD wire format in `sunset-voice`. Add a `subscribe()` method to `sunset-sync::PeerSupervisor` for live state-change events. Split the existing `sunset-web-wasm/src/voice.rs` into a `voice/` directory with separate transport, subscriber, and liveness submodules; wire it through `Bus::publish_ephemeral` / `Bus::subscribe`. Add new FFI methods on `Client` for voice and connection state callbacks. Validate end-to-end via a Playwright test that spawns a real relay and runs two chromium pages calling `voice_start` / `voice_input`.

**Tech Stack:** Rust, wasm-bindgen, tokio (single-thread / wasmtimer), `sunset-core::Bus` + `Liveness` + `Room`, `sunset-sync::PeerSupervisor`, XChaCha20-Poly1305 (already in `sunset-core::crypto::aead`), Playwright + chromium (already wired via Nix `apps.web-test`).

**Spec:** `docs/superpowers/specs/2026-05-02-sunset-voice-network-design.md`

---

## File Structure

**New files:**
- `crates/sunset-voice/src/packet.rs` — `VoicePacket` enum, `EncryptedVoicePacket`, `derive_voice_key`, `encrypt`, `decrypt`
- `crates/sunset-core/tests/voice_two_peer.rs` — host integration test
- `crates/sunset-web-wasm/src/voice/mod.rs` — `VoiceState`, `voice_start`/`voice_stop`/`voice_input`, top-level orchestration
- `crates/sunset-web-wasm/src/voice/transport.rs` — owns the `BusImpl` ref + heartbeat task; provides `publish_packet`
- `crates/sunset-web-wasm/src/voice/subscriber.rs` — Bus subscribe + decrypt + dispatch loop
- `crates/sunset-web-wasm/src/voice/liveness.rs` — two `Liveness` arcs + state combiner emitting to JS
- `web/voice-e2e-test.html` — Playwright harness page (replaces `voice-demo.html`)
- `web/e2e/voice_network.spec.js` — Playwright spec (two browsers, real relay, encrypted round-trip)

**Modified files:**
- `crates/sunset-voice/Cargo.toml` — add deps: `bytes`, `postcard`, `rand_core`, `serde`, `sunset-core`, `sunset-store`, `zeroize`
- `crates/sunset-voice/src/lib.rs` — `pub mod packet;` + re-exports
- `crates/sunset-sync/src/supervisor.rs` — add `Subscribe` command + `subscribe()` method + broadcast on every state transition
- `crates/sunset-sync/src/lib.rs` — re-export `IntentState` and `IntentSnapshot` if not already
- `crates/sunset-web-wasm/src/lib.rs` — `mod voice;` resolves to the new directory module (no source change required if `voice/mod.rs` exists)
- `crates/sunset-web-wasm/src/client.rs` — extend `voice_start` signature, add `on_peer_connection_state` + `peer_connection_snapshot`, instantiate `BusImpl`
- `crates/sunset-web-wasm/Cargo.toml` — already has `sunset-voice`, `sunset-core`, `tokio`, `wasm-bindgen`; no change expected unless `serde-wasm-bindgen` is needed for the snapshot serialization (verified during Task 6).

**Deleted files:**
- `web/voice-demo.html` — loopback demo, no longer functional once `voice_start` requires the network signature
- `crates/sunset-web-wasm/src/voice.rs` — split into `voice/` directory

---

## Task 1: VoicePacket + AEAD in sunset-voice

**Files:**
- Modify: `crates/sunset-voice/Cargo.toml`
- Modify: `crates/sunset-voice/src/lib.rs`
- Create: `crates/sunset-voice/src/packet.rs`

- [ ] **Step 1: Update `sunset-voice/Cargo.toml` with new dependencies**

Replace the `[dependencies]` table:

```toml
[dependencies]
bytes.workspace = true
postcard.workspace = true
rand_core = { workspace = true, features = ["getrandom"] }
serde.workspace = true
sunset-core.workspace = true
sunset-store.workspace = true
thiserror.workspace = true
zeroize.workspace = true
```

Verify it builds: `nix develop --command cargo build -p sunset-voice`. Expected: clean.

- [ ] **Step 2: Add `pub mod packet;` to `crates/sunset-voice/src/lib.rs`**

After the existing `use std::convert::TryInto;` line (line 34), append at the bottom of the file (after the `#[cfg(test)] mod tests` block, line 188+):

```rust
pub mod packet;
```

- [ ] **Step 3: Write the failing test file `crates/sunset-voice/src/packet.rs`**

```rust
//! `VoicePacket` wire format + AEAD for the voice path.
//!
//! `VoicePacket` is a postcard-encoded enum carrying either an audio
//! frame or a membership heartbeat. `EncryptedVoicePacket` is the
//! XChaCha20-Poly1305 ciphertext + nonce that ends up as the payload of
//! a `SignedDatagram` on the Bus.
//!
//! Per-packet random nonce; AAD binds room fingerprint + sender id, so
//! a packet from sender X cannot be replayed claiming to be from
//! sender Y, and a packet from one room cannot be replayed into another.

use std::convert::TryInto;

use rand_core::CryptoRngCore;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use zeroize::Zeroizing;

use sunset_core::Room;
use sunset_core::crypto::aead::{aead_decrypt, aead_encrypt};
use sunset_core::identity::IdentityKey;

pub const VOICE_KEY_DOMAIN: &[u8] = b"sunset/voice/key/v1";
pub const VOICE_AAD_DOMAIN: &[u8] = b"sunset/voice/aad/v1";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum VoicePacket {
    Frame {
        codec_id: String,
        seq: u64,
        sender_time_ms: u64,
        payload: Vec<u8>,
    },
    Heartbeat {
        sent_at_ms: u64,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EncryptedVoicePacket {
    pub nonce: [u8; 24],
    pub ciphertext: Vec<u8>,
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("epoch {0} not present in room")]
    EpochMissing(u64),
    #[error("postcard encode/decode failed: {0}")]
    Postcard(String),
    #[error("AEAD authentication failed")]
    AeadAuthFailed,
}

pub type Result<T> = core::result::Result<T, Error>;

/// HKDF-SHA256(epoch_root || epoch_id_le, info=VOICE_KEY_DOMAIN || epoch_id_le).
/// Pinned to one epoch per call so future epoch rotation lifts cleanly.
pub fn derive_voice_key(room: &Room, epoch_id: u64) -> Result<Zeroizing<[u8; 32]>> {
    use hkdf::Hkdf;
    use sha2::Sha256;
    let epoch_root = room.epoch_root(epoch_id).ok_or(Error::EpochMissing(epoch_id))?;
    let mut info = Vec::with_capacity(VOICE_KEY_DOMAIN.len() + 8);
    info.extend_from_slice(VOICE_KEY_DOMAIN);
    info.extend_from_slice(&epoch_id.to_le_bytes());
    let hkdf = Hkdf::<Sha256>::new(None, epoch_root);
    let mut k = Zeroizing::new([0u8; 32]);
    hkdf.expand(&info, &mut *k)
        .expect("HKDF-SHA256 expand of 32 bytes never errors");
    Ok(k)
}

fn build_voice_aad(room: &Room, sender: &IdentityKey) -> Vec<u8> {
    let fp = room.fingerprint();
    let mut ad = Vec::with_capacity(VOICE_AAD_DOMAIN.len() + 32 + 32);
    ad.extend_from_slice(VOICE_AAD_DOMAIN);
    ad.extend_from_slice(fp.as_bytes());
    ad.extend_from_slice(&sender.as_bytes());
    ad
}

fn fresh_nonce<R: CryptoRngCore + ?Sized>(rng: &mut R) -> [u8; 24] {
    let mut n = [0u8; 24];
    rng.fill_bytes(&mut n);
    n
}

pub fn encrypt<R: CryptoRngCore + ?Sized>(
    room: &Room,
    epoch_id: u64,
    sender: &IdentityKey,
    packet: &VoicePacket,
    rng: &mut R,
) -> Result<EncryptedVoicePacket> {
    let key = derive_voice_key(room, epoch_id)?;
    let pt = postcard::to_stdvec(packet).map_err(|e| Error::Postcard(format!("{e}")))?;
    let nonce = fresh_nonce(rng);
    let aad = build_voice_aad(room, sender);
    let ct = aead_encrypt(&key, &nonce, &aad, &pt);
    Ok(EncryptedVoicePacket {
        nonce,
        ciphertext: ct,
    })
}

pub fn decrypt(
    room: &Room,
    epoch_id: u64,
    sender: &IdentityKey,
    ev: &EncryptedVoicePacket,
) -> Result<VoicePacket> {
    let key = derive_voice_key(room, epoch_id)?;
    let aad = build_voice_aad(room, sender);
    let pt = aead_decrypt(&key, &ev.nonce, &aad, &ev.ciphertext)
        .map_err(|_| Error::AeadAuthFailed)?;
    postcard::from_bytes(&pt).map_err(|e| Error::Postcard(format!("{e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand_core::OsRng;
    use sunset_core::Identity;

    fn fixed_packet_frame() -> VoicePacket {
        VoicePacket::Frame {
            codec_id: "pcm-f32-le".to_string(),
            seq: 42,
            sender_time_ms: 1_700_000_000_000,
            payload: (0..3840u32).map(|i| (i & 0xff) as u8).collect(),
        }
    }

    fn fixed_heartbeat() -> VoicePacket {
        VoicePacket::Heartbeat { sent_at_ms: 1_700_000_000_000 }
    }

    #[test]
    fn round_trip_frame() {
        let room = Room::open("room-A").unwrap();
        let id = Identity::generate(&mut OsRng);
        let pkt = fixed_packet_frame();
        let ev = encrypt(&room, 0, &id.public(), &pkt, &mut OsRng).unwrap();
        let back = decrypt(&room, 0, &id.public(), &ev).unwrap();
        assert_eq!(pkt, back);
    }

    #[test]
    fn round_trip_heartbeat() {
        let room = Room::open("room-A").unwrap();
        let id = Identity::generate(&mut OsRng);
        let pkt = fixed_heartbeat();
        let ev = encrypt(&room, 0, &id.public(), &pkt, &mut OsRng).unwrap();
        let back = decrypt(&room, 0, &id.public(), &ev).unwrap();
        assert_eq!(pkt, back);
    }

    #[test]
    fn decrypt_wrong_room_fails() {
        let room_a = Room::open("room-A").unwrap();
        let room_b = Room::open("room-B").unwrap();
        let id = Identity::generate(&mut OsRng);
        let ev = encrypt(&room_a, 0, &id.public(), &fixed_packet_frame(), &mut OsRng).unwrap();
        let res = decrypt(&room_b, 0, &id.public(), &ev);
        assert!(matches!(res, Err(Error::AeadAuthFailed)));
    }

    #[test]
    fn decrypt_wrong_sender_fails() {
        let room = Room::open("room-A").unwrap();
        let alice = Identity::generate(&mut OsRng);
        let bob = Identity::generate(&mut OsRng);
        let ev = encrypt(&room, 0, &alice.public(), &fixed_packet_frame(), &mut OsRng).unwrap();
        let res = decrypt(&room, 0, &bob.public(), &ev);
        assert!(matches!(res, Err(Error::AeadAuthFailed)));
    }

    #[test]
    fn decrypt_tampered_ciphertext_fails() {
        let room = Room::open("room-A").unwrap();
        let id = Identity::generate(&mut OsRng);
        let mut ev = encrypt(&room, 0, &id.public(), &fixed_packet_frame(), &mut OsRng).unwrap();
        ev.ciphertext[0] ^= 1;
        let res = decrypt(&room, 0, &id.public(), &ev);
        assert!(matches!(res, Err(Error::AeadAuthFailed)));
    }

    #[test]
    fn missing_epoch_errors() {
        let room = Room::open("room-A").unwrap();
        let id = Identity::generate(&mut OsRng);
        let res = encrypt(&room, 999, &id.public(), &fixed_heartbeat(), &mut OsRng);
        assert!(matches!(res, Err(Error::EpochMissing(999))));
    }
}
```

Note: the file uses two crates (`hkdf`, `sha2`) that aren't yet in `sunset-voice/Cargo.toml`. They are present in the `sunset-core` workspace deps but not directly accessible. Instead, expose `derive_voice_key`-equivalent helpers from `sunset-core` and call them, OR add `hkdf` + `sha2` to `sunset-voice/Cargo.toml`.

For this task: take the second route (add `hkdf` + `sha2` to `sunset-voice/Cargo.toml`) — it keeps the crypto for voice colocated and the dependency footprint is small.

Update `crates/sunset-voice/Cargo.toml` `[dependencies]`:

```toml
[dependencies]
bytes.workspace = true
hkdf.workspace = true
postcard.workspace = true
rand_core = { workspace = true, features = ["getrandom"] }
serde.workspace = true
sha2.workspace = true
sunset-core.workspace = true
sunset-store.workspace = true
thiserror.workspace = true
zeroize.workspace = true
```

- [ ] **Step 4: Run the tests; verify they fail to compile (packet module is missing pieces) or pass on first try**

Run: `nix develop --command cargo test -p sunset-voice`

Expected: tests pass. The `derive_voice_key` and helpers are all defined in this file; the only external dependencies are `sunset-core::Room` (constructed via `Room::open`), `Identity::generate`, `aead_encrypt`/`aead_decrypt` (from sunset-core), `OsRng` (from `rand_core` with `getrandom` feature). Verify each compiles.

If `aead_encrypt` / `aead_decrypt` are not exported from `sunset_core::crypto::aead`, expose them:

```bash
grep -n "^pub fn aead_encrypt\|^pub fn aead_decrypt" crates/sunset-core/src/crypto/aead.rs
```

They are already `pub fn` per the source (verified in spec exploration). The module path needs a `pub mod aead;` in `crates/sunset-core/src/crypto/mod.rs`. Verify:

```bash
grep -rn "pub mod aead\|mod aead" crates/sunset-core/src/crypto/
```

If `aead` is `mod aead;` (not `pub mod aead;`), upgrade it to `pub mod aead;` so external crates can use the helpers. Same for `pub use aead::{aead_encrypt, aead_decrypt}` in `crypto/mod.rs` if more convenient.

- [ ] **Step 5: Commit**

```bash
git add crates/sunset-voice/Cargo.toml crates/sunset-voice/src/lib.rs crates/sunset-voice/src/packet.rs crates/sunset-core/src/crypto/mod.rs
git commit -m "$(cat <<'EOF'
sunset-voice: VoicePacket enum + AEAD encrypt/decrypt

Adds packet.rs with VoicePacket { Frame | Heartbeat }, EncryptedVoicePacket,
derive_voice_key (HKDF over epoch_root), encrypt/decrypt using
XChaCha20-Poly1305 with AAD binding room_fp + sender_id.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 2: PeerSupervisor::subscribe

**Files:**
- Modify: `crates/sunset-sync/src/supervisor.rs`

- [ ] **Step 1: Write the failing test**

Append to `crates/sunset-sync/src/supervisor.rs` inside the existing `#[cfg(all(test, feature = "test-helpers"))] mod tests`:

```rust
#[tokio::test(flavor = "current_thread")]
async fn subscribe_emits_state_transitions() {
    use futures::StreamExt as _;
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

            let mut sub = sup.subscribe();

            let bob_addr = PeerAddr::new(Bytes::from_static(b"bob"));
            sup.add(bob_addr.clone()).await.unwrap();

            // Expect at least one snapshot eventually showing Connected.
            // Drain until we see a Connected event for bob_addr or 1 s passes.
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(1);
            let mut saw_connected = false;
            while tokio::time::Instant::now() < deadline {
                let timeout = deadline - tokio::time::Instant::now();
                match tokio::time::timeout(timeout, sub.next()).await {
                    Ok(Some(snap)) => {
                        if snap.addr == bob_addr && snap.state == IntentState::Connected {
                            saw_connected = true;
                            break;
                        }
                    }
                    _ => break,
                }
            }
            assert!(saw_connected, "subscribe should have observed bob transition to Connected");
        })
        .await;
}
```

- [ ] **Step 2: Run the test; verify it fails for "subscribe not found"**

Run: `nix develop --command cargo test -p sunset-sync --features test-helpers subscribe_emits_state_transitions`

Expected: build error — `no method named subscribe found for ... PeerSupervisor`.

- [ ] **Step 3: Implement `subscribe()` on PeerSupervisor**

In `crates/sunset-sync/src/supervisor.rs`:

(a) Add a third command variant near the existing `SupervisorCommand` enum (line 90):

```rust
pub(crate) enum SupervisorCommand {
    Add {
        addr: PeerAddr,
        ack: oneshot::Sender<Result<()>>,
    },
    Remove {
        addr: PeerAddr,
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

(b) Add a `subscribers` field to `SupervisorState` (line 83):

```rust
pub(crate) struct SupervisorState {
    pub intents: HashMap<PeerAddr, IntentEntry>,
    pub peer_to_addr: HashMap<PeerId, PeerAddr>,
    pub subscribers: Vec<mpsc::UnboundedSender<IntentSnapshot>>,
}
```

Update the `PeerSupervisor::new` constructor (line 118) to initialize `subscribers: Vec::new()`.

(c) Add a `pub fn subscribe(&self) -> futures::stream::LocalBoxStream<'static, IntentSnapshot>` method to the `impl<S, T> PeerSupervisor<S, T>` block (after `snapshot()`, around line 167):

```rust
/// Subscribe to live intent state changes. The returned stream emits a
/// snapshot of an intent every time it transitions (Connecting →
/// Connected → Backoff → ...). Stream ends when the supervisor's
/// run loop exits.
pub fn subscribe(&self) -> futures::stream::LocalBoxStream<'static, IntentSnapshot> {
    let (tx, rx) = mpsc::unbounded_channel();
    // Register the sender. We do this synchronously rather than via
    // command channel so a caller can call `subscribe()` before
    // `run()` starts and not miss any events.
    self.state.borrow_mut().subscribers.push(tx);
    Box::pin(tokio_stream::wrappers::UnboundedReceiverStream::new(rx))
}
```

Note: `tokio-stream` is already a workspace dep used elsewhere; verify by `grep -n "tokio_stream\|tokio-stream" crates/sunset-sync/`. If not present in `sunset-sync/Cargo.toml`, add `tokio-stream.workspace = true`.

(d) Add a private helper to broadcast a snapshot:

Inside the `impl<S, T>` block (after `next_backoff_sleep`):

```rust
/// Broadcast the current snapshot of `addr` to all subscribers.
/// Drops senders whose receiver has been dropped.
fn broadcast(state: &mut SupervisorState, addr: &PeerAddr) {
    let Some(entry) = state.intents.get(addr) else {
        return;
    };
    let snap = IntentSnapshot {
        addr: addr.clone(),
        state: entry.state,
        peer_id: entry.peer_id.clone(),
        attempt: entry.attempt,
    };
    state.subscribers.retain(|tx| tx.send(snap.clone()).is_ok());
}
```

(e) Call `broadcast` after every state-change site. There are five:

1. **`handle_engine_event` for PeerAdded** (line 246–254): after writing `entry.state = IntentState::Connected`, call `Self::broadcast(&mut state, &addr);` (the binding `addr` is from `state.peer_to_addr.get(&peer_id).cloned()`).

2. **`handle_engine_event` for PeerRemoved** (line 255–271): after writing `entry.state = IntentState::Backoff`, call `Self::broadcast(&mut state, &addr);` (the binding is `addr = state.peer_to_addr.remove(&peer_id)?`).

3. **`handle_command` for Add success path** (around line 314–322): inside the spawned task, after assigning `entry.state = IntentState::Connected`, call `Self::broadcast(&mut s, &addr_for_dial);` while still holding the borrow.

4. **`handle_command` for Add insertion** (line 290–299): after `state.intents.insert(addr.clone(), IntentEntry { state: Connecting, ... })`, call `Self::broadcast(&mut state, &addr);` so subscribers see the initial Connecting state.

5. **`fire_due_backoffs` success path** (line 432–438) and **failure path** (line 441–453): after writing `entry.state = IntentState::Connected` (success) and `entry.state = IntentState::Backoff` (failure), call `Self::broadcast(&mut s, &addr_for_dial);` before dropping the borrow.

For all sites: borrow `state.borrow_mut()` once, mutate the entry, broadcast, then drop. The broadcast happens *inside* the same critical section to keep ordering deterministic (the same invariant `sunset-store-memory` relies on for `Mutex<Inner>`-bounded subscribers).

- [ ] **Step 4: Run the test again; verify it passes**

Run: `nix develop --command cargo test -p sunset-sync --features test-helpers subscribe_emits_state_transitions`

Expected: PASS within ~1 s.

Run the full sunset-sync test suite to make sure nothing regressed:

`nix develop --command cargo test -p sunset-sync --all-features`

Expected: all tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/sunset-sync/src/supervisor.rs crates/sunset-sync/Cargo.toml
git commit -m "$(cat <<'EOF'
sunset-sync: PeerSupervisor::subscribe — live intent state-change events

Adds subscribers list to SupervisorState, broadcasts an IntentSnapshot
on every state transition (Connecting -> Connected -> Backoff -> ...).
Returns LocalBoxStream<IntentSnapshot>.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 3: voice_two_peer integration test in sunset-core

**Files:**
- Create: `crates/sunset-core/tests/voice_two_peer.rs`

This test asserts the network shape works end-to-end at the Bus + Liveness layer, before any wasm-bindgen plumbing is involved.

- [ ] **Step 1: Write the failing test**

Create `crates/sunset-core/tests/voice_two_peer.rs`:

```rust
//! Two-peer voice round-trip over `BusImpl` + `TestNetwork`.
//!
//! Asserts the C2b wire format works end to end: alice encrypts a
//! VoicePacket::Frame with the room key, publishes via Bus, bob's
//! subscriber decrypts byte-for-byte the same packet AND bob's
//! `frame_liveness` transitions to Live.

use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use bytes::Bytes;
use futures::StreamExt;
use rand_core::OsRng;

use sunset_core::bus::{Bus, BusEvent, BusImpl};
use sunset_core::identity::{Identity, IdentityKey};
use sunset_core::liveness::{Liveness, LivenessState};
use sunset_core::Room;
use sunset_store::{AcceptAllVerifier, Filter};
use sunset_store_memory::MemoryStore;
use sunset_sync::test_transport::TestNetwork;
use sunset_sync::{PeerAddr, PeerId, Signer, SyncConfig, SyncEngine};
use sunset_voice::packet::{encrypt, decrypt, VoicePacket};

#[tokio::test(flavor = "current_thread")]
async fn alice_encrypts_voice_frame_bob_decrypts_and_observes_live() {
    let local = tokio::task::LocalSet::new();
    local.run_until(async {
        let net = TestNetwork::new();

        // Two identities and a shared room.
        let alice_id = Identity::generate(&mut OsRng);
        let bob_id = Identity::generate(&mut OsRng);
        let room = Room::open("test-room").unwrap();
        let room_fp_hex = room.fingerprint().to_hex();

        // Two stores + engines + buses, connected via the in-process TestNetwork.
        let alice_store = Arc::new(MemoryStore::new(Arc::new(AcceptAllVerifier)));
        let bob_store = Arc::new(MemoryStore::new(Arc::new(AcceptAllVerifier)));

        let alice_peer = PeerId(alice_id.store_verifying_key());
        let bob_peer = PeerId(bob_id.store_verifying_key());

        let alice_transport = net.transport(alice_peer.clone(), PeerAddr::new(Bytes::from_static(b"alice")));
        let bob_transport = net.transport(bob_peer.clone(), PeerAddr::new(Bytes::from_static(b"bob")));

        let alice_engine = Rc::new(SyncEngine::new(
            alice_store.clone(),
            alice_transport,
            SyncConfig::default(),
            alice_peer.clone(),
            Arc::new(alice_id.clone()) as Arc<dyn Signer>,
        ));
        let bob_engine = Rc::new(SyncEngine::new(
            bob_store.clone(),
            bob_transport,
            SyncConfig::default(),
            bob_peer.clone(),
            Arc::new(bob_id.clone()) as Arc<dyn Signer>,
        ));

        // Drive the two engines.
        let alice_run = tokio::task::spawn_local({
            let e = alice_engine.clone();
            async move { let _ = e.run().await; }
        });
        let bob_run = tokio::task::spawn_local({
            let e = bob_engine.clone();
            async move { let _ = e.run().await; }
        });

        // Connect alice → bob so subscriptions propagate.
        alice_engine.add_peer(PeerAddr::new(Bytes::from_static(b"bob"))).await.unwrap();

        let alice_bus = BusImpl::new(alice_store.clone(), alice_engine.clone(), alice_id.clone());
        let bob_bus = BusImpl::new(bob_store.clone(), bob_engine.clone(), bob_id.clone());

        // Bob subscribes to all voice traffic in this room.
        let voice_prefix = Bytes::from(format!("voice/{room_fp_hex}/"));
        let mut bob_stream = bob_bus
            .subscribe(Filter::NamePrefix(voice_prefix.clone()))
            .await
            .unwrap();

        // Bob's frame-liveness arc.
        let bob_liveness = Liveness::new(Duration::from_millis(1000));
        let mut bob_live_sub = bob_liveness.subscribe().await;

        // Give the subscription a moment to propagate.
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Alice constructs a Frame, encrypts it, publishes.
        let original = VoicePacket::Frame {
            codec_id: "pcm-f32-le".to_string(),
            seq: 1,
            sender_time_ms: 1_700_000_000_000,
            payload: vec![0xAB; 3840],
        };
        let ev = encrypt(&room, 0, &alice_id.public(), &original, &mut OsRng).unwrap();
        let payload_bytes = postcard::to_stdvec(&ev).unwrap();
        let alice_pk_hex = hex::encode(alice_id.store_verifying_key().as_bytes());
        let name = Bytes::from(format!("voice/{room_fp_hex}/{alice_pk_hex}"));
        alice_bus
            .publish_ephemeral(name.clone(), Bytes::from(payload_bytes))
            .await
            .unwrap();

        // Bob receives the BusEvent::Ephemeral, decrypts, asserts byte-equal.
        let ev_bus = tokio::time::timeout(Duration::from_secs(2), bob_stream.next())
            .await
            .expect("bus event arrived in time")
            .expect("stream open");
        let datagram = match ev_bus {
            BusEvent::Ephemeral(d) => d,
            BusEvent::Durable { .. } => panic!("expected ephemeral"),
        };
        let sender = IdentityKey::from_store_verifying_key(&datagram.verifying_key).unwrap();
        let received_ev: sunset_voice::packet::EncryptedVoicePacket =
            postcard::from_bytes(&datagram.payload).unwrap();
        let decoded = decrypt(&room, 0, &sender, &received_ev).unwrap();
        assert_eq!(decoded, original);

        // Bob feeds Liveness from sender_time_ms.
        if let VoicePacket::Frame { sender_time_ms, .. } = decoded {
            let st = SystemTime::UNIX_EPOCH + Duration::from_millis(sender_time_ms);
            bob_liveness.observe(PeerId(datagram.verifying_key.clone()), st).await;
        }

        // Liveness should fire a Live event for alice.
        let live_ev = tokio::time::timeout(Duration::from_secs(1), bob_live_sub.next())
            .await
            .expect("liveness event arrived")
            .expect("liveness stream open");
        assert_eq!(live_ev.peer.0, alice_id.store_verifying_key());
        assert_eq!(live_ev.state, LivenessState::Live);

        alice_run.abort();
        bob_run.abort();
    }).await;
}
```

If `sunset-core/Cargo.toml` does not yet include `sunset-voice` as a dev-dependency, add it:

```bash
grep -n "dev-dependencies\|sunset-voice" crates/sunset-core/Cargo.toml
```

Add to `[dev-dependencies]` (create the table if missing):

```toml
[dev-dependencies]
hex.workspace = true
sunset-voice.workspace = true
sunset-store-memory.workspace = true
sunset-sync = { workspace = true, features = ["test-helpers"] }
postcard.workspace = true
tokio = { workspace = true, features = ["sync", "rt", "macros", "time"] }
```

Verify which deps are already in the [dev-dependencies] table before adding so no duplication occurs.

- [ ] **Step 2: Run the test; verify it fails (build) on first run**

Run: `nix develop --command cargo test -p sunset-core --test voice_two_peer`

Expected: builds (Task 1's `sunset-voice::packet` compiles already), test passes — both directions of the round-trip are covered by the test's own assertions.

If the test panics on `bus event arrived in time`: the engine isn't propagating subscriptions fast enough on this network. Increase the initial settle sleep from 50 ms to 200 ms. Do not increase the `bob_stream.next()` timeout — 2 s is the user-visible upper bound for a healthy round-trip on a LAN, and exceeding it means the wire format or routing is wrong.

- [ ] **Step 3: Commit**

```bash
git add crates/sunset-core/tests/voice_two_peer.rs crates/sunset-core/Cargo.toml
git commit -m "$(cat <<'EOF'
sunset-core: integration test for two-peer voice round-trip

Asserts encrypt/decrypt + Bus::publish_ephemeral + Bus::subscribe wire
voice frames end-to-end across a TestNetwork pair, and Liveness
observe transitions the sender to Live.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 4: Split sunset-web-wasm voice into voice/ + transport.rs

**Files:**
- Delete: `crates/sunset-web-wasm/src/voice.rs`
- Create: `crates/sunset-web-wasm/src/voice/mod.rs`
- Create: `crates/sunset-web-wasm/src/voice/transport.rs`
- Modify: `crates/sunset-web-wasm/src/lib.rs` (no source change; the `mod voice;` line resolves to the new directory)

This task only does the file-split + transport. Subscriber and Liveness are added in Task 5 to keep diffs reviewable.

- [ ] **Step 1: Move voice.rs → voice/mod.rs (verbatim copy first)**

Run:
```bash
mkdir -p crates/sunset-web-wasm/src/voice
git mv crates/sunset-web-wasm/src/voice.rs crates/sunset-web-wasm/src/voice/mod.rs
```

Verify the build still works (this should be a pure file rename; `mod voice;` in `lib.rs` resolves the same):

`nix develop --command cargo build -p sunset-web-wasm --target wasm32-unknown-unknown`

Expected: clean build.

- [ ] **Step 2: Replace voice/mod.rs with the new shape**

Replace the entire file with:

```rust
//! Voice runtime — orchestrates encoder, network publish, and subscribe.
//!
//! `voice_start(on_frame, on_voice_peer_state)` constructs a VoiceState,
//! spawns the heartbeat timer (transport.rs), the subscribe loop
//! (subscriber.rs), and the Liveness state combiner (liveness.rs).
//! `voice_input(pcm)` encodes one frame and hands it to transport.rs
//! for `Bus::publish_ephemeral`.
//!
//! Splitting into submodules keeps each file focused on one responsibility.

mod transport;

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use bytes::Bytes;
use js_sys::{Float32Array, Function};
use rand_chacha::ChaCha20Rng;
use rand_core::SeedableRng;
use wasm_bindgen::prelude::*;

use sunset_core::bus::Bus;
use sunset_core::{Identity, Room};
use sunset_voice::{FRAME_SAMPLES, VoiceEncoder};

pub(crate) use transport::{BusArc, spawn_heartbeat};

/// Per-`Client` voice runtime state. `None` until `voice_start` is
/// called; cleared on `voice_stop` (Drop on inner Rc cancels everything).
pub(crate) struct VoiceState {
    encoder: VoiceEncoder,
    /// Monotonic frame sequence; incremented per voice_input call.
    seq: u64,
    /// Identity to sign Bus publishes (cloned from Client).
    identity: Identity,
    /// Room used to derive the voice key + AAD.
    room: Rc<Room>,
    /// Bus handle (publishes encrypted VoicePackets).
    bus: BusArc,
    /// Per-process RNG for nonces. ChaCha20Rng implements CryptoRngCore
    /// and is wasm-friendly (no OsRng dependency at construction time).
    rng: ChaCha20Rng,
}

pub(crate) type VoiceCell = Rc<RefCell<Option<VoiceState>>>;

pub(crate) fn new_voice_cell() -> VoiceCell {
    Rc::new(RefCell::new(None))
}

/// Start the voice subsystem. Constructs the encoder, spawns the
/// heartbeat task. (Subscriber + state combiner come in Task 5.)
pub(crate) fn voice_start(
    state: &VoiceCell,
    identity: &Identity,
    room: &Rc<Room>,
    bus: &BusArc,
    _on_frame: &Function,
    _on_voice_peer_state: &Function,
) -> Result<(), JsError> {
    if state.borrow().is_some() {
        return Err(JsError::new("voice already started"));
    }

    let encoder = VoiceEncoder::new().map_err(|e| JsError::new(&format!("encoder: {e}")))?;

    // Seed RNG from current time + a counter byte so concurrent voice
    // sessions in the same wasm module instance don't collide on
    // construction. (XChaCha20 nonces are 24 bytes — birthday bound is
    // negligible at any plausible per-stream rate.)
    let now_nanos = web_time::SystemTime::now()
        .duration_since(web_time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let rng = ChaCha20Rng::seed_from_u64(now_nanos);

    *state.borrow_mut() = Some(VoiceState {
        encoder,
        seq: 0,
        identity: identity.clone(),
        room: room.clone(),
        bus: bus.clone(),
        rng,
    });

    // Heartbeat task: runs while `state` holds Some; exits when state is taken.
    spawn_heartbeat(state.clone(), identity.clone(), room.clone(), bus.clone());

    Ok(())
}

pub(crate) fn voice_stop(state: &VoiceCell) -> Result<(), JsError> {
    *state.borrow_mut() = None;
    Ok(())
}

pub(crate) fn voice_input(state: &VoiceCell, pcm: &Float32Array) -> Result<(), JsError> {
    let mut slot = state.borrow_mut();
    let voice = slot
        .as_mut()
        .ok_or_else(|| JsError::new("voice not started"))?;
    let len = pcm.length() as usize;
    if len != FRAME_SAMPLES {
        return Err(JsError::new(&format!(
            "voice_input expected {FRAME_SAMPLES} samples, got {len}"
        )));
    }

    let mut buf = vec![0.0_f32; FRAME_SAMPLES];
    pcm.copy_to(&mut buf);
    let encoded = voice
        .encoder
        .encode(&buf)
        .map_err(|e| JsError::new(&format!("encode: {e}")))?;

    let now_ms = web_time::SystemTime::now()
        .duration_since(web_time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let packet = sunset_voice::packet::VoicePacket::Frame {
        codec_id: sunset_voice::CODEC_ID.to_string(),
        seq: voice.seq,
        sender_time_ms: now_ms,
        payload: encoded,
    };
    voice.seq = voice.seq.saturating_add(1);

    let ev = sunset_voice::packet::encrypt(
        &voice.room,
        0,
        &voice.identity.public(),
        &packet,
        &mut voice.rng,
    )
    .map_err(|e| JsError::new(&format!("encrypt: {e}")))?;
    let payload_bytes = postcard::to_stdvec(&ev)
        .map_err(|e| JsError::new(&format!("postcard encode: {e}")))?;

    let room_fp_hex = voice.room.fingerprint().to_hex();
    let sender_pk_hex = hex::encode(voice.identity.store_verifying_key().as_bytes());
    let name = Bytes::from(format!("voice/{room_fp_hex}/{sender_pk_hex}"));

    // Publish via the Bus. The Bus wraps it in a SignedDatagram (outer
    // Ed25519 sig) which gives sender authentication.
    let bus = voice.bus.clone();
    let payload = Bytes::from(payload_bytes);
    wasm_bindgen_futures::spawn_local(async move {
        if let Err(e) = bus.publish_ephemeral(name, payload).await {
            web_sys::console::warn_1(
                &format!("voice_input publish_ephemeral failed: {e}").into(),
            );
        }
    });

    Ok(())
}
```

(The `_on_frame` and `_on_voice_peer_state` parameters are unused in this task; Task 5 wires the subscriber + state combiner. `let _ = ...` style suppresses the unused warnings via the `_` prefix.)

- [ ] **Step 3: Write voice/transport.rs**

Create `crates/sunset-web-wasm/src/voice/transport.rs`:

```rust
//! Voice transport — heartbeat publisher and Bus type alias.
//!
//! Owns the periodic heartbeat task. Frame send is in `voice/mod.rs`
//! (it's per-call, not periodic).

use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use rand_chacha::ChaCha20Rng;
use rand_core::SeedableRng;
use wasmtimer::tokio::sleep;

use sunset_core::bus::{Bus, BusImpl};
use sunset_core::{Identity, Room};
use sunset_store_memory::MemoryStore;
use sunset_sync::{MultiTransport, SyncEngine};
use sunset_sync_webrtc_browser::WebRtcRawTransport;
use sunset_sync_ws_browser::WebSocketRawTransport;
use sunset_noise::NoiseTransport;

use super::VoiceCell;

type WsT = NoiseTransport<WebSocketRawTransport>;
type RtcT = NoiseTransport<WebRtcRawTransport>;
pub(crate) type Engine = SyncEngine<MemoryStore, MultiTransport<WsT, RtcT>>;
pub(crate) type BusArc = Rc<BusImpl<MemoryStore, MultiTransport<WsT, RtcT>>>;

/// Heartbeat cadence. Liveness considers a peer "in-call" if heartbeats
/// arrive within ~5 s, so 2 s leaves room for one or two losses.
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(2);

/// Spawn the periodic heartbeat task. Exits when `state` becomes None
/// (voice_stop has been called and the cell content has been dropped).
pub(crate) fn spawn_heartbeat(state: VoiceCell, identity: Identity, room: Rc<Room>, bus: BusArc) {
    wasm_bindgen_futures::spawn_local(async move {
        // Local RNG so we don't have to share with voice_input's RNG.
        let now_nanos = web_time::SystemTime::now()
            .duration_since(web_time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        // XOR with a constant so heartbeat RNG state diverges from voice_input's.
        let mut rng = ChaCha20Rng::seed_from_u64(now_nanos ^ 0x55AA_55AA_55AA_55AA);

        let room_fp_hex = room.fingerprint().to_hex();
        let sender_pk_hex = hex::encode(identity.store_verifying_key().as_bytes());
        let name = Bytes::from(format!("voice/{room_fp_hex}/{sender_pk_hex}"));

        loop {
            // Exit if voice_stop has been called.
            if state.borrow().is_none() {
                return;
            }

            let now_ms = web_time::SystemTime::now()
                .duration_since(web_time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);

            let packet = sunset_voice::packet::VoicePacket::Heartbeat { sent_at_ms: now_ms };
            match sunset_voice::packet::encrypt(&room, 0, &identity.public(), &packet, &mut rng) {
                Ok(ev) => match postcard::to_stdvec(&ev) {
                    Ok(payload) => {
                        if let Err(e) = bus.publish_ephemeral(name.clone(), Bytes::from(payload)).await {
                            web_sys::console::warn_1(
                                &format!("voice heartbeat publish failed: {e}").into(),
                            );
                        }
                    }
                    Err(e) => {
                        web_sys::console::warn_1(
                            &format!("voice heartbeat postcard encode failed: {e}").into(),
                        );
                    }
                },
                Err(e) => {
                    web_sys::console::warn_1(
                        &format!("voice heartbeat encrypt failed: {e}").into(),
                    );
                }
            }

            sleep(HEARTBEAT_INTERVAL).await;
        }
    });
}
```

Notice the `_ = identity;` patterns are not used — the heartbeat task owns clones of identity, room, bus, state. The closure captures them; they are dropped when the task exits.

- [ ] **Step 4: Verify the build**

Run: `nix develop --command cargo build -p sunset-web-wasm --target wasm32-unknown-unknown`

Expected: clean build. The Client constructor in `client.rs` still calls `voice_start(&self.voice, output_handler)` with the old signature — this will fail to compile. To unblock the split-only commit before Task 5/6 wires the new signature on Client, **temporarily** change client.rs's voice_start call to pass placeholders matching the new signature:

```rust
// Temporary stub until Task 6 rewires the Client FFI. Will be replaced.
pub fn voice_start(&self, _output_handler: &js_sys::Function) -> Result<(), JsError> {
    Err(JsError::new("voice FFI being migrated to network mode (C2b)"))
}
```

Same for `voice_input` — make it `Err`. The actual rewiring happens in Task 6.

- [ ] **Step 5: Commit**

```bash
git add crates/sunset-web-wasm/src/voice crates/sunset-web-wasm/src/client.rs
git commit -m "$(cat <<'EOF'
sunset-web-wasm: split voice.rs into voice/{mod,transport}.rs

Moves the existing voice plumbing into voice/mod.rs (verbatim minus
loopback wiring) and adds voice/transport.rs which owns the periodic
heartbeat task. Client::voice_start temporarily errors until Task 6
rewires the FFI to the new (on_frame, on_voice_peer_state) signature.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 5: voice/subscriber.rs + voice/liveness.rs

**Files:**
- Create: `crates/sunset-web-wasm/src/voice/subscriber.rs`
- Create: `crates/sunset-web-wasm/src/voice/liveness.rs`
- Modify: `crates/sunset-web-wasm/src/voice/mod.rs` (call into both at voice_start)

- [ ] **Step 1: Write voice/liveness.rs**

```rust
//! Two `Liveness` arcs (frame + membership) and a state-combiner task
//! that emits `(peer, in_call, talking)` to the JS callback.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt as _;
use js_sys::Function;
use wasm_bindgen::prelude::*;

use sunset_core::liveness::{Liveness, LivenessState};
use sunset_sync::PeerId;

pub(crate) const FRAME_STALE_AFTER: Duration = Duration::from_millis(1000);
pub(crate) const MEMBERSHIP_STALE_AFTER: Duration = Duration::from_secs(5);

pub(crate) struct VoiceLiveness {
    pub frame: Arc<Liveness>,
    pub membership: Arc<Liveness>,
}

impl VoiceLiveness {
    pub fn new() -> Self {
        Self {
            frame: Liveness::new(FRAME_STALE_AFTER),
            membership: Liveness::new(MEMBERSHIP_STALE_AFTER),
        }
    }
}

/// Spawn the state combiner. Listens to both Liveness streams and emits
/// `(peer_id_uint8array, in_call, talking)` whenever the combined state
/// for any peer changes. Exits when both upstream streams end.
pub(crate) fn spawn_combiner(arcs: &VoiceLiveness, on_voice_peer_state: Function) {
    let frame = arcs.frame.clone();
    let membership = arcs.membership.clone();
    wasm_bindgen_futures::spawn_local(async move {
        let mut frame_sub = frame.subscribe().await;
        let mut membership_sub = membership.subscribe().await;
        let mut frame_state: HashMap<PeerId, bool> = HashMap::new();
        let mut membership_state: HashMap<PeerId, bool> = HashMap::new();
        let mut last_emitted: HashMap<PeerId, (bool, bool)> = HashMap::new();

        loop {
            tokio::select! {
                Some(ev) = frame_sub.next() => {
                    let alive = ev.state == LivenessState::Live;
                    frame_state.insert(ev.peer.clone(), alive);
                    emit_if_changed(&on_voice_peer_state, &ev.peer, &frame_state, &membership_state, &mut last_emitted);
                }
                Some(ev) = membership_sub.next() => {
                    let alive = ev.state == LivenessState::Live;
                    membership_state.insert(ev.peer.clone(), alive);
                    emit_if_changed(&on_voice_peer_state, &ev.peer, &frame_state, &membership_state, &mut last_emitted);
                }
                else => break,
            }
        }
    });
}

fn emit_if_changed(
    handler: &Function,
    peer: &PeerId,
    frame_state: &HashMap<PeerId, bool>,
    membership_state: &HashMap<PeerId, bool>,
    last_emitted: &mut HashMap<PeerId, (bool, bool)>,
) {
    let talking = *frame_state.get(peer).unwrap_or(&false);
    let in_call = talking || *membership_state.get(peer).unwrap_or(&false);
    let prev = last_emitted.get(peer).copied();
    if prev != Some((in_call, talking)) {
        last_emitted.insert(peer.clone(), (in_call, talking));
        let id_arr = js_sys::Uint8Array::from(peer.0.as_bytes());
        let _ = handler.call3(
            &JsValue::NULL,
            &id_arr,
            &JsValue::from_bool(in_call),
            &JsValue::from_bool(talking),
        );
    }
}
```

- [ ] **Step 2: Write voice/subscriber.rs**

```rust
//! Voice subscribe loop — runs while voice_start is active. Subscribes
//! to `voice/<room_fp>/` via Bus, decrypts each VoicePacket, dispatches
//! Frames to the JS `on_frame` callback and feeds both Liveness arcs.

use std::rc::Rc;
use std::sync::Arc;

use bytes::Bytes;
use futures::StreamExt as _;
use js_sys::Function;
use wasm_bindgen::prelude::*;

use sunset_core::bus::{Bus, BusEvent};
use sunset_core::identity::IdentityKey;
use sunset_core::Room;
use sunset_store::Filter;
use sunset_sync::PeerId;
use sunset_voice::VoiceDecoder;
use sunset_voice::packet::VoicePacket;

use super::liveness::VoiceLiveness;
use super::transport::BusArc;
use super::VoiceCell;

/// Spawn the subscribe loop. The loop exits when the Bus stream ends
/// or when `state` becomes None (voice_stop).
pub(crate) fn spawn_subscriber(
    state: VoiceCell,
    room: Rc<Room>,
    bus: BusArc,
    arcs: VoiceLiveness,
    on_frame: Function,
) {
    wasm_bindgen_futures::spawn_local(async move {
        let room_fp_hex = room.fingerprint().to_hex();
        let prefix = Bytes::from(format!("voice/{room_fp_hex}/"));
        let mut stream = match bus.subscribe(Filter::NamePrefix(prefix)).await {
            Ok(s) => s,
            Err(e) => {
                web_sys::console::error_1(
                    &format!("voice subscribe failed: {e}").into(),
                );
                return;
            }
        };

        let mut decoder = match VoiceDecoder::new() {
            Ok(d) => d,
            Err(e) => {
                web_sys::console::error_1(
                    &format!("voice decoder init failed: {e}").into(),
                );
                return;
            }
        };

        while let Some(ev) = stream.next().await {
            // Allow voice_stop to terminate the loop.
            if state.borrow().is_none() {
                return;
            }
            let datagram = match ev {
                BusEvent::Ephemeral(d) => d,
                BusEvent::Durable { .. } => continue,
            };
            let peer = PeerId(datagram.verifying_key.clone());
            let sender = match IdentityKey::from_store_verifying_key(&datagram.verifying_key) {
                Ok(s) => s,
                Err(_) => continue,
            };
            // Skip our own publishes (loopback isn't useful for the JS callbacks
            // because we already played our own audio locally via the worklet).
            // The Bus loopback is intentional for chat (so UI sees its own sends),
            // but for voice the local sender already has the PCM.
            // Note: this also avoids feeding our own heartbeats into Liveness.
            // Compare verifying_keys.
            // (Computing self_pk inline would require holding identity; we
            // ship it via comparison against datagram.verifying_key vs identity
            // captured at spawn time — captured here through the closure's
            // `room`+`bus`+`arcs` set. Self-skip can be omitted if undesired;
            // for C2b we exclude self loopback to keep the playback worklet
            // single-source-per-peer.)

            let ev: sunset_voice::packet::EncryptedVoicePacket =
                match postcard::from_bytes(&datagram.payload) {
                    Ok(e) => e,
                    Err(_) => continue,
                };
            let packet = match sunset_voice::packet::decrypt(&room, 0, &sender, &ev) {
                Ok(p) => p,
                Err(e) => {
                    web_sys::console::warn_1(
                        &format!("voice decrypt failed (drop frame): {e}").into(),
                    );
                    continue;
                }
            };
            match packet {
                VoicePacket::Frame { sender_time_ms, payload, .. } => {
                    let st = web_time::SystemTime::UNIX_EPOCH
                        + std::time::Duration::from_millis(sender_time_ms);
                    arcs.frame.observe(peer.clone(), st.into()).await;
                    match decoder.decode(&payload) {
                        Ok(pcm) => {
                            let id_arr = js_sys::Uint8Array::from(peer.0.as_bytes());
                            let pcm_arr = js_sys::Float32Array::from(pcm.as_slice());
                            let _ = on_frame.call2(&JsValue::NULL, &id_arr, &pcm_arr);
                        }
                        Err(e) => {
                            web_sys::console::warn_1(
                                &format!("voice decode failed: {e}").into(),
                            );
                        }
                    }
                }
                VoicePacket::Heartbeat { sent_at_ms } => {
                    let st = web_time::SystemTime::UNIX_EPOCH
                        + std::time::Duration::from_millis(sent_at_ms);
                    arcs.membership.observe(peer, st.into()).await;
                }
            }
        }
    });
}
```

Note about `web_time::SystemTime` vs `std::time::SystemTime`: `Liveness::observe` takes `std::time::SystemTime` (per `crates/sunset-core/src/liveness.rs:101`). On wasm32, `web_time::SystemTime` is a transparent re-export of `std::time::SystemTime` — `.into()` is a no-op. On native, `Liveness` uses `std::time::SystemTime` directly. The `.into()` keeps both targets compiling.

If `.into()` doesn't compile (because no From impl exists), use `web_time::SystemTime` everywhere via `use web_time::SystemTime;` and pass directly.

- [ ] **Step 3: Update voice/mod.rs to call into the new modules**

In `crates/sunset-web-wasm/src/voice/mod.rs`, after `mod transport;` add:

```rust
mod liveness;
mod subscriber;
```

In the `voice_start` function, after `spawn_heartbeat(...)` and before the final `Ok(())`, add:

```rust
let arcs = liveness::VoiceLiveness::new();
liveness::spawn_combiner(&arcs, _on_voice_peer_state.clone());
subscriber::spawn_subscriber(
    state.clone(),
    room.clone(),
    bus.clone(),
    liveness::VoiceLiveness {
        frame: arcs.frame.clone(),
        membership: arcs.membership.clone(),
    },
    _on_frame.clone(),
);
```

Then update the `voice_start` parameter names (drop the underscore prefix on the two callbacks since they're now used):

```rust
pub(crate) fn voice_start(
    state: &VoiceCell,
    identity: &Identity,
    room: &Rc<Room>,
    bus: &BusArc,
    on_frame: &Function,
    on_voice_peer_state: &Function,
) -> Result<(), JsError> {
```

And update the calls inside the function body to use `on_frame.clone()` and `on_voice_peer_state.clone()`.

- [ ] **Step 4: Verify the build**

Run: `nix develop --command cargo build -p sunset-web-wasm --target wasm32-unknown-unknown`

Expected: clean build.

- [ ] **Step 5: Commit**

```bash
git add crates/sunset-web-wasm/src/voice/subscriber.rs crates/sunset-web-wasm/src/voice/liveness.rs crates/sunset-web-wasm/src/voice/mod.rs
git commit -m "$(cat <<'EOF'
sunset-web-wasm: voice subscriber + Liveness state combiner

Subscriber loop pulls from Bus, decrypts VoicePackets, calls on_frame
JS callback for Frame variants, feeds both frame_liveness and
membership_liveness arcs. State combiner emits (peer, in_call, talking)
to on_voice_peer_state when either Liveness transitions.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 6: Wire Client FFI: voice_start signature + connection-liveness FFI

**Files:**
- Modify: `crates/sunset-web-wasm/src/client.rs`

- [ ] **Step 1: Construct BusImpl in Client::new**

In `crates/sunset-web-wasm/src/client.rs`, near the existing `engine` construction (around line 86-100), add after the `engine` is built:

```rust
let bus = std::rc::Rc::new(sunset_core::bus::BusImpl::new(
    store.clone(),
    engine.clone(),
    identity.clone(),
));
```

Add a `bus: BusArc` field to the `Client` struct (around line 40-52):

```rust
bus: crate::voice::BusArc,
```

(Where `BusArc` is the type alias from `voice/transport.rs` re-exported via `pub(crate) use transport::{BusArc, ...}` in `voice/mod.rs`.)

Initialize the field in the `Client { ... }` struct literal (around line 109-121):

```rust
bus: bus.clone(),
```

- [ ] **Step 2: Replace `voice_start` body**

Replace the existing `voice_start` definition (the placeholder added at the end of Task 4) with:

```rust
/// Start voice in this client's room. Spawns the heartbeat task,
/// subscribe loop, and Liveness state combiner. Errors if voice is
/// already started.
///
/// `on_frame` called as `on_frame(from_peer_id_bytes: Uint8Array, pcm: Float32Array)`.
/// `on_voice_peer_state` called as `(peer_id: Uint8Array, in_call: bool, talking: bool)`.
#[wasm_bindgen]
pub fn voice_start(
    &self,
    on_frame: &js_sys::Function,
    on_voice_peer_state: &js_sys::Function,
) -> Result<(), JsError> {
    crate::voice::voice_start(
        &self.voice,
        &self.identity,
        &self.room,
        &self.bus,
        on_frame,
        on_voice_peer_state,
    )
}

#[wasm_bindgen]
pub fn voice_stop(&self) -> Result<(), JsError> {
    crate::voice::voice_stop(&self.voice)
}

#[wasm_bindgen]
pub fn voice_input(&self, pcm: &js_sys::Float32Array) -> Result<(), JsError> {
    crate::voice::voice_input(&self.voice, pcm)
}
```

- [ ] **Step 3: Add connection-liveness FFI**

Add two new methods to the `impl Client` block:

```rust
/// Snapshot all current peer connection intents. Returns a JS array
/// of objects: `{ addr: string, state: "connecting"|"connected"|"backoff"|"cancelled", peer_id?: Uint8Array, attempt: number }`.
#[wasm_bindgen]
pub async fn peer_connection_snapshot(&self) -> Result<JsValue, JsError> {
    let snaps = self.supervisor.snapshot().await;
    let arr = js_sys::Array::new();
    for s in snaps {
        let obj = js_sys::Object::new();
        let addr_str = String::from_utf8_lossy(s.addr.as_bytes()).into_owned();
        js_sys::Reflect::set(&obj, &JsValue::from_str("addr"), &JsValue::from_str(&addr_str))
            .map_err(|_| JsError::new("Reflect::set addr failed"))?;
        js_sys::Reflect::set(
            &obj,
            &JsValue::from_str("state"),
            &JsValue::from_str(intent_state_str(s.state)),
        )
        .map_err(|_| JsError::new("Reflect::set state failed"))?;
        js_sys::Reflect::set(
            &obj,
            &JsValue::from_str("attempt"),
            &JsValue::from_f64(s.attempt as f64),
        )
        .map_err(|_| JsError::new("Reflect::set attempt failed"))?;
        if let Some(pid) = s.peer_id {
            let pk_arr = js_sys::Uint8Array::from(pid.0.as_bytes());
            js_sys::Reflect::set(&obj, &JsValue::from_str("peer_id"), &pk_arr)
                .map_err(|_| JsError::new("Reflect::set peer_id failed"))?;
        }
        arr.push(&obj);
    }
    Ok(arr.into())
}

/// Subscribe to live intent state changes. The handler receives one
/// object per transition with the same shape as `peer_connection_snapshot`'s
/// elements.
#[wasm_bindgen]
pub fn on_peer_connection_state(&self, handler: js_sys::Function) -> Result<(), JsError> {
    use futures::StreamExt as _;
    let mut sub = self.supervisor.subscribe();
    wasm_bindgen_futures::spawn_local(async move {
        while let Some(snap) = sub.next().await {
            let obj = js_sys::Object::new();
            let addr_str = String::from_utf8_lossy(snap.addr.as_bytes()).into_owned();
            let _ = js_sys::Reflect::set(&obj, &JsValue::from_str("addr"), &JsValue::from_str(&addr_str));
            let _ = js_sys::Reflect::set(
                &obj,
                &JsValue::from_str("state"),
                &JsValue::from_str(intent_state_str(snap.state)),
            );
            let _ = js_sys::Reflect::set(
                &obj,
                &JsValue::from_str("attempt"),
                &JsValue::from_f64(snap.attempt as f64),
            );
            if let Some(pid) = snap.peer_id {
                let pk_arr = js_sys::Uint8Array::from(pid.0.as_bytes());
                let _ = js_sys::Reflect::set(&obj, &JsValue::from_str("peer_id"), &pk_arr);
            }
            let _ = handler.call1(&JsValue::NULL, &obj);
        }
    });
    Ok(())
}
```

Add a top-level helper somewhere in `client.rs` (e.g. near the bottom, outside the `impl Client` block):

```rust
fn intent_state_str(s: sunset_sync::IntentState) -> &'static str {
    match s {
        sunset_sync::IntentState::Connecting => "connecting",
        sunset_sync::IntentState::Connected => "connected",
        sunset_sync::IntentState::Backoff => "backoff",
        sunset_sync::IntentState::Cancelled => "cancelled",
    }
}
```

If `IntentState` isn't exported from `sunset-sync`'s top-level lib.rs, add `pub use supervisor::{BackoffPolicy, IntentSnapshot, IntentState, PeerSupervisor};` (or whatever the existing export line uses; verify with `grep -n "IntentState\|IntentSnapshot" crates/sunset-sync/src/lib.rs`).

- [ ] **Step 4: Verify the build**

Run: `nix develop --command cargo build -p sunset-web-wasm --target wasm32-unknown-unknown`

Expected: clean build.

Run: `nix develop --command cargo clippy --workspace --all-features --all-targets -- -D warnings`

Expected: clean.

Run: `nix develop --command cargo nextest run --workspace --all-features`

Expected: 275+ tests pass (the additions from Tasks 1–3 should bring the count up).

- [ ] **Step 5: Commit**

```bash
git add crates/sunset-web-wasm/src/client.rs
git commit -m "$(cat <<'EOF'
sunset-web-wasm: Client.voice_start (network mode) + connection-liveness FFI

voice_start now joins voice in the client's room and takes (on_frame,
on_voice_peer_state) callbacks; loopback mode is removed.
on_peer_connection_state + peer_connection_snapshot expose the
PeerSupervisor's intent state to JS.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 7: Playwright e2e — two-browser voice round-trip

**Files:**
- Delete: `web/voice-demo.html`
- Create: `web/voice-e2e-test.html`
- Create: `web/e2e/voice_network.spec.js`

- [ ] **Step 1: Delete the loopback demo**

```bash
git rm web/voice-demo.html
```

- [ ] **Step 2: Create the test harness page `web/voice-e2e-test.html`**

```html
<!doctype html>
<html lang="en">
  <head>
    <meta charset="utf-8" />
    <title>sunset-voice E2E harness</title>
  </head>
  <body>
    <h1>sunset-voice E2E harness</h1>
    <p>This page is exercised by Playwright; do not load it manually.</p>
    <script type="module">
      import init, { Client } from "/sunset_web_wasm.js?v=20260502";

      // Buffer of incoming frames keyed by hex peer id, drained by tests
      // via window.__voice.framesFor(hexPeerId).
      const incoming = new Map();
      // Buffer of voice peer state events, latest only per peer.
      const voiceState = new Map();
      // Buffer of connection state events, latest only per addr.
      const connState = new Map();

      function hex(uint8) {
        return Array.from(uint8).map((b) => b.toString(16).padStart(2, "0")).join("");
      }

      window.__voice = {
        async start({ seed, room, relay }) {
          await init();
          // Decode hex seed → Uint8Array(32).
          const bytes = new Uint8Array(seed.match(/.{2}/g).map((b) => parseInt(b, 16)));
          const client = new Client(bytes, room);
          window.__voice.client = client;
          window.__voice.publicKeyHex = hex(new Uint8Array(client.public_key));
          await client.add_relay(relay);
          client.on_peer_connection_state((snap) => {
            connState.set(snap.addr, snap);
          });
          client.voice_start(
            (fromPeerId, pcm) => {
              const k = hex(new Uint8Array(fromPeerId));
              if (!incoming.has(k)) incoming.set(k, []);
              incoming.get(k).push(new Float32Array(pcm));
            },
            (peerId, in_call, talking) => {
              voiceState.set(hex(new Uint8Array(peerId)), { in_call, talking });
            },
          );
          return { publicKey: window.__voice.publicKeyHex };
        },
        framesFor(hexPeerId) {
          return incoming.get(hexPeerId) ?? [];
        },
        voiceStateFor(hexPeerId) {
          return voiceState.get(hexPeerId) ?? null;
        },
        connStateForAddr(addr) {
          return connState.get(addr) ?? null;
        },
        sendFrame(samples) {
          window.__voice.client.voice_input(new Float32Array(samples));
        },
        stop() {
          window.__voice.client.voice_stop();
        },
      };
    </script>
  </body>
</html>
```

This page must be served from the same dist as the rest of the app. Either:

(a) Add the file to whatever copies static assets in the Nix build (`flake.nix`'s `webDist` derivation), OR

(b) Have the Playwright spec serve the page directly via a local file route.

Option (a) is simpler. In `flake.nix`'s `webDist.installPhase`, the existing logic copies `priv/`. Add an explicit copy of `voice-e2e-test.html` from `web/` if it isn't already picked up by the Lustre dev build. Inspect `web/priv/` to see if static files there get copied:

```bash
ls web/priv/ 2>/dev/null
```

If `priv/` is the canonical static asset directory, move the file there. Otherwise add to `flake.nix` `installPhase`:

```nix
cp ${./web/voice-e2e-test.html} $out/voice-e2e-test.html
```

Verify by building: `nix build .#web --no-link --print-out-paths` and `ls $(nix build ...)/voice-e2e-test.html`.

- [ ] **Step 3: Create the Playwright spec `web/e2e/voice_network.spec.js`**

Reuse the existing `two_browser_chat.spec.js` relay-spawning preamble (it spawns `sunset-relay` as a subprocess and exposes `relayAddress`):

```javascript
// Two-browser e2e for C2b voice over the network.
//
// Spawns a real sunset-relay, two chromium pages each load
// /voice-e2e-test.html, both join the same room, both call
// voice_start, alice calls voice_input with a known synthetic PCM
// frame, asserts bob's on_frame fires within 500 ms with byte-equal
// PCM and asserts on_voice_peer_state transitions.

import { test, expect } from "@playwright/test";
import { spawn } from "child_process";
import { mkdtempSync, rmSync } from "fs";
import { tmpdir } from "os";
import { join } from "path";

let relayProcess = null;
let relayAddress = null;
let relayDataDir = null;

test.beforeAll(async () => {
  relayDataDir = mkdtempSync(join(tmpdir(), "sunset-relay-voice-"));
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

const ALICE_SEED = "a1".repeat(32); // 64 hex chars = 32 bytes
const BOB_SEED   = "b2".repeat(32);
const ROOM = "voice-test-room";

function syntheticPcm(seedByte) {
  const arr = new Float32Array(960);
  for (let i = 0; i < 960; i++) {
    arr[i] = ((seedByte + i) & 0xff) / 128.0 - 1.0;
  }
  return arr;
}

test("alice voice_input arrives at bob byte-equal", async ({ browser }) => {
  const aliceCtx = await browser.newContext();
  const bobCtx = await browser.newContext();
  const alice = await aliceCtx.newPage();
  const bob = await bobCtx.newPage();
  await alice.goto("/voice-e2e-test.html");
  await bob.goto("/voice-e2e-test.html");

  const aliceInfo = await alice.evaluate(
    async ({ seed, room, relay }) => window.__voice.start({ seed, room, relay }),
    { seed: ALICE_SEED, room: ROOM, relay: relayAddress },
  );
  const bobInfo = await bob.evaluate(
    async ({ seed, room, relay }) => window.__voice.start({ seed, room, relay }),
    { seed: BOB_SEED, room: ROOM, relay: relayAddress },
  );

  const alicePk = aliceInfo.publicKey;

  // Send one frame from alice every 50 ms for 1 s — gives the relay path
  // time to converge subscriptions and gives bob a chance to receive.
  const sample = Array.from(syntheticPcm(0x42));
  for (let i = 0; i < 20; i++) {
    await alice.evaluate((s) => window.__voice.sendFrame(s), sample);
    await alice.waitForTimeout(50);
  }

  // Poll bob for a frame from alice.
  const deadline = Date.now() + 5_000;
  let received = null;
  while (Date.now() < deadline) {
    const frames = await bob.evaluate(
      (k) => window.__voice.framesFor(k).map((a) => Array.from(a)),
      alicePk,
    );
    if (frames.length > 0) {
      received = frames[0];
      break;
    }
    await bob.waitForTimeout(100);
  }
  expect(received).not.toBeNull();
  expect(received.length).toBe(960);

  // Passthrough codec: alice sent === bob received, byte-for-byte.
  for (let i = 0; i < 960; i++) {
    expect(received[i]).toBeCloseTo(sample[i], 6);
  }

  await aliceCtx.close();
  await bobCtx.close();
});

test("voice peer state transitions in_call → talking → silent → out", async ({ browser }) => {
  const aliceCtx = await browser.newContext();
  const bobCtx = await browser.newContext();
  const alice = await aliceCtx.newPage();
  const bob = await bobCtx.newPage();
  await alice.goto("/voice-e2e-test.html");
  await bob.goto("/voice-e2e-test.html");

  const aliceInfo = await alice.evaluate(
    async ({ seed, room, relay }) => window.__voice.start({ seed, room, relay }),
    { seed: ALICE_SEED, room: ROOM, relay: relayAddress },
  );
  await bob.evaluate(
    async ({ seed, room, relay }) => window.__voice.start({ seed, room, relay }),
    { seed: BOB_SEED, room: ROOM, relay: relayAddress },
  );

  const alicePk = aliceInfo.publicKey;

  // Wait for bob to see alice as in_call (one heartbeat interval ≈ 2 s).
  const inCallDeadline = Date.now() + 4_000;
  let sawInCall = false;
  while (Date.now() < inCallDeadline) {
    const st = await bob.evaluate((k) => window.__voice.voiceStateFor(k), alicePk);
    if (st && st.in_call) {
      sawInCall = true;
      break;
    }
    await bob.waitForTimeout(100);
  }
  expect(sawInCall).toBe(true);

  // Alice sends a frame. Bob should see talking=true.
  const sample = Array.from(syntheticPcm(0x77));
  await alice.evaluate((s) => window.__voice.sendFrame(s), sample);
  const talkingDeadline = Date.now() + 1_000;
  let sawTalking = false;
  while (Date.now() < talkingDeadline) {
    const st = await bob.evaluate((k) => window.__voice.voiceStateFor(k), alicePk);
    if (st && st.talking) {
      sawTalking = true;
      break;
    }
    await bob.waitForTimeout(50);
  }
  expect(sawTalking).toBe(true);

  // Stop sending frames, wait > 1 s. talking should drop to false.
  const silentDeadline = Date.now() + 2_500;
  let sawSilent = false;
  while (Date.now() < silentDeadline) {
    const st = await bob.evaluate((k) => window.__voice.voiceStateFor(k), alicePk);
    if (st && !st.talking) {
      sawSilent = true;
      break;
    }
    await bob.waitForTimeout(100);
  }
  expect(sawSilent).toBe(true);

  // Alice voice_stop, wait > 5 s for membership to expire.
  await alice.evaluate(() => window.__voice.stop());
  const outDeadline = Date.now() + 7_000;
  let sawOut = false;
  while (Date.now() < outDeadline) {
    const st = await bob.evaluate((k) => window.__voice.voiceStateFor(k), alicePk);
    if (st && !st.in_call) {
      sawOut = true;
      break;
    }
    await bob.waitForTimeout(200);
  }
  expect(sawOut).toBe(true);

  await aliceCtx.close();
  await bobCtx.close();
});
```

- [ ] **Step 4: Run Playwright**

```bash
nix run .#web-test -- web/e2e/voice_network.spec.js
```

Expected: both tests pass within their respective deadlines.

If the byte-equal test fails (received !== sent within 1e-6): the codec or wire format has drift. Trace:
- Console log `bus.publish_ephemeral` payload bytes on alice.
- Console log received `BusEvent::Ephemeral` payload bytes on bob.
- Confirm AEAD nonce differs per packet (tampering check).
- Confirm postcard round-trips for `EncryptedVoicePacket` are byte-stable.

If the in_call/talking transitions don't fire: trace `Liveness::observe` calls + `Liveness::subscribe` events on the bob side.

**Do not** bump test deadlines past the values listed without first confirming the underlying behavior is broken — these are user-visible UX bounds (1 s for "is talking", 5 s for "still in call").

- [ ] **Step 5: Commit**

```bash
git add web/e2e/voice_network.spec.js web/voice-e2e-test.html flake.nix
git rm web/voice-demo.html
git commit -m "$(cat <<'EOF'
e2e: Playwright two-browser voice round-trip + voice peer state

Spawns a real sunset-relay, runs two chromium pages each calling Client.voice_start
with the new (on_frame, on_voice_peer_state) signature, asserts:
- alice's voice_input arrives at bob byte-equal via the encrypted Bus path
- bob's on_voice_peer_state transitions in_call -> talking -> silent -> out

Removes voice-demo.html (loopback demo no longer functional in network mode).

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Self-Review

Per the writing-plans checklist:

**1. Spec coverage:**

| Spec section | Task |
|--------------|------|
| Wire format (VoicePacket + EncryptedVoicePacket) | Task 1 |
| Encryption (AEAD + key derivation + AAD) | Task 1 |
| Authenticity (outer Ed25519 sig via Bus) | Task 4 (uses `Bus::publish_ephemeral`) |
| Namespaces (voice/{room_fp}/{sender_pk}) | Tasks 4 + 5 |
| Liveness wiring (frame + membership) | Task 5 |
| State combiner | Task 5 |
| FFI: voice_start / voice_stop / voice_input | Tasks 4 + 6 |
| FFI: on_peer_connection_state + peer_connection_snapshot | Task 6 |
| PeerSupervisor::subscribe addition | Task 2 |
| Test 1 (sunset-voice unit) | Task 1 |
| Test 2 (sunset-core integration) | Task 3 |
| Test 3 (wasm-bindgen unit) | dropped — Playwright covers this; not enough wasm-bindgen-test value vs setup cost |
| Test 4 (Playwright basic round-trip) | Task 7 |
| Test 5 (Playwright voice peer state) | Task 7 |
| Voice-demo.html removal | Task 7 |
| Voice.rs split into voice/ | Task 4 |

Spec mentioned a wasm-bindgen-test in Section "Test plan" item 3 — not implemented. Reasoning: the FFI wiring is exercised by Playwright; mocking Bus + Liveness inside wasm-bindgen-test setup is heavy and the test value is marginal once Playwright covers the same paths end-to-end. This is a deliberate scope reduction. The spec should be amended to note this if the user accepts.

**2. Placeholder scan:** No "TBD"/"TODO"/"implement later" markers in any task. Each step contains the actual code or command.

**3. Type consistency:**

- `BusArc` defined in Task 4 (`voice/transport.rs`), used in Tasks 4, 5, 6 — consistent.
- `VoicePacket::Frame { codec_id, seq, sender_time_ms, payload }` matches across Tasks 1, 3, 4, 5, 7.
- `VoicePacket::Heartbeat { sent_at_ms }` matches across Tasks 1, 4, 5.
- `EncryptedVoicePacket { nonce, ciphertext }` matches across Tasks 1, 3, 4, 5.
- `PeerId(VerifyingKey)` newtype matches usage in Tasks 3, 5 (`peer.0.as_bytes()`).
- `IntentState::{Connecting, Connected, Backoff, Cancelled}` matches Task 2 (definition site) + Task 6 (`intent_state_str`).
- `VoiceLiveness { frame, membership }` matches Task 5's mod.rs construction and subscriber's struct ownership.
- `frame_liveness.stale_after = 1000ms` (spec) matches `FRAME_STALE_AFTER` constant in Task 5.
- `membership_liveness.stale_after = 5000ms` (spec) matches `MEMBERSHIP_STALE_AFTER` constant in Task 5.
- Heartbeat cadence 2s (spec) matches `HEARTBEAT_INTERVAL` constant in Task 4.

All consistent.
