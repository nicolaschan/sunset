# sunset-sync native WebSocket transport (Plan C) — Implementation Plan

> **For agentic workers:** Use superpowers:executing-plans (or superpowers:subagent-driven-development) to execute this plan task-by-task. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Land Plan C from the web roadmap. Two `SyncEngine` instances exchange real `sunset-core` encrypted+signed messages over a real localhost WebSocket connection wrapped in a Noise tunnel, with both stores using `Ed25519Verifier` end-to-end (no `AcceptAllVerifier` workaround).

**Spec:** `docs/superpowers/specs/2026-04-27-sunset-sync-ws-native-design.md`.

**Out of scope (deferred):**

- Browser WebSocket transport (separate plan).
- WebRTC, WebTransport (separate plans).
- `sunset-relay` binary, multi-relay integration tests (Plan D).
- Hybrid post-quantum cryptography (single later plan covering Noise + Plan 7 + Plan 9).
- Connection retry / backoff / multiplexing.
- Multi-relay client config.

---

## Architecture summary

```
SyncEngine
   ↑ consumes
Transport         (existing — authenticated; TransportConnection has peer_id)
   ↑ implemented by
NoiseTransport<R: RawTransport>     (in sunset-noise — only crate that knows Noise)
   ↑ decorates
RawTransport      (NEW — plain bytes pipe; no peer_id on RawConnection)
   ↑ implemented by
WebSocketRawTransport                 (in sunset-sync-ws-native — zero crypto deps)
```

Noise pattern: `Noise_IK_25519_XChaChaPoly_BLAKE2b` via `snow`.
Identity → X25519: standard SHA-512-clamp conversion of the Ed25519 secret seed.
PeerAddr scheme for WS transport: `wss://host:port#x25519=<64-hex>`.

---

## File structure

```
sunset/
├── Cargo.toml                                  # MODIFY: workspace add sunset-noise + sunset-sync-ws-native + crypto deps
├── crates/
│   ├── sunset-store/
│   │   └── src/
│   │       └── canonical.rs                    # MOVED from sunset-core (signing_payload)
│   ├── sunset-core/
│   │   └── src/
│   │       ├── canonical.rs                    # Becomes a re-export shim
│   │       └── identity.rs                     # MODIFY: impl sunset_sync::Signer + sunset_noise::NoiseIdentity
│   ├── sunset-sync/
│   │   └── src/
│   │       ├── signer.rs                       # NEW: Signer trait
│   │       ├── transport.rs                    # MODIFY: add RawTransport / RawConnection
│   │       ├── engine.rs                       # MODIFY: SyncEngine::new takes signer; do_publish_subscription signs
│   │       └── lib.rs                          # MODIFY: re-export Signer + RawTransport
│   ├── sunset-noise/                           # NEW
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── identity.rs                     # NoiseIdentity trait + Ed25519→X25519 helper
│   │       ├── pattern.rs                      # NOISE_PATTERN constant
│   │       └── handshake.rs                    # NoiseTransport, NoiseConnection
│   └── sunset-sync-ws-native/                  # NEW
│       ├── Cargo.toml
│       ├── src/
│       │   └── lib.rs                          # WebSocketRawTransport / RawConnection
│       └── tests/
│           └── two_peer_ws_noise.rs            # alice ↔ bob over real WS + Noise + Ed25519Verifier
```

---

## Tasks

### Task 1: Move `signing_payload` from sunset-core to sunset-store

**Files:**
- Move: `crates/sunset-core/src/canonical.rs` → `crates/sunset-store/src/canonical.rs`
- Modify: `crates/sunset-store/src/lib.rs` (add `pub mod canonical;` + `pub use canonical::signing_payload;`)
- Modify: `crates/sunset-core/src/canonical.rs` (replace contents with re-export shim)
- Modify: `crates/sunset-core/src/verifier.rs` (import path update)

- [ ] **Step 1:** Copy the entire content of `crates/sunset-core/src/canonical.rs` (function + tests) into a new file `crates/sunset-store/src/canonical.rs`. The imports change because we're now inside the store crate:

  - `use sunset_store::{Hash, SignedKvEntry, VerifyingKey};` → `use crate::types::{Hash, SignedKvEntry, VerifyingKey};` (or `use crate::{Hash, SignedKvEntry, VerifyingKey};` if those are re-exported at the crate root).
  - The frozen-vector test moves with the function. Its frozen hex is `d15d46aa02779b076df6f8223577aead0385307e3817112c65297661af2b3094` (from Plan 6) and must NOT change.

- [ ] **Step 2:** In `crates/sunset-store/src/lib.rs`, add:

  ```rust
  pub mod canonical;
  pub use canonical::signing_payload;
  ```

- [ ] **Step 3:** Replace `crates/sunset-core/src/canonical.rs` with a shim:

  ```rust
  //! Canonical signing payload — moved to `sunset_store::canonical` so
  //! `sunset-sync` can use it without depending on `sunset-core`.
  //! This module is a back-compat re-export.

  pub use sunset_store::canonical::signing_payload;
  ```

- [ ] **Step 4:** In `crates/sunset-core/src/verifier.rs`, change `use crate::canonical::signing_payload;` to `use sunset_store::canonical::signing_payload;`. (Both work because of the shim, but using the canonical home directly is cleaner.)

- [ ] **Step 5:** Verify nothing broke:
  ```
  nix develop --command cargo test -p sunset-store canonical::tests
  nix develop --command cargo test -p sunset-core canonical::tests
  nix develop --command cargo test -p sunset-core verifier::tests
  nix develop --command cargo build --workspace
  ```
  All should pass; the frozen-vector hex stays at `d15d46aa02779b076df6f8223577aead0385307e3817112c65297661af2b3094`.

- [ ] **Step 6:** Commit:
  ```
  git add crates/sunset-store/src/canonical.rs crates/sunset-store/src/lib.rs \
          crates/sunset-core/src/canonical.rs crates/sunset-core/src/verifier.rs
  git commit -m "Move signing_payload to sunset-store (sunset-core re-exports)"
  ```

---

### Task 2: `Signer` trait in sunset-sync

**Files:**
- Create: `crates/sunset-sync/src/signer.rs`
- Modify: `crates/sunset-sync/src/lib.rs`

- [ ] **Step 1:** Create `crates/sunset-sync/src/signer.rs`:

  ```rust
  //! Per-peer signing capability injected into `SyncEngine`.
  //!
  //! `sunset-core::Identity` implements this; tests can implement a stub.

  use bytes::Bytes;

  use sunset_store::VerifyingKey;

  pub trait Signer: Send + Sync {
      /// The verifying-key bytes that match this signer's signatures.
      fn verifying_key(&self) -> VerifyingKey;

      /// Produce an Ed25519 signature over `payload`. Returns 64 bytes.
      fn sign(&self, payload: &[u8]) -> Bytes;
  }
  ```

- [ ] **Step 2:** In `crates/sunset-sync/src/lib.rs`, add:
  ```rust
  pub mod signer;
  pub use signer::Signer;
  ```

- [ ] **Step 3:** Verify it builds:
  ```
  nix develop --command cargo build -p sunset-sync
  ```

- [ ] **Step 4:** Commit:
  ```
  git add crates/sunset-sync/src/signer.rs crates/sunset-sync/src/lib.rs
  git commit -m "Add Signer trait for sync-internal entry signing"
  ```

---

### Task 3: `SyncEngine::new` takes a Signer; `do_publish_subscription` actually signs

**Files:**
- Modify: `crates/sunset-sync/src/engine.rs`
- Modify: existing tests that call `SyncEngine::new` (in this crate + downstream)

This task changes a public API signature. After this task, `SyncEngine::new` requires a `signer` parameter; downstream call sites (sunset-core's integration test, sunset-sync's own tests) need updating.

- [ ] **Step 1:** Modify `SyncEngine::new` in `crates/sunset-sync/src/engine.rs` to take `signer: Arc<dyn Signer>` as the last parameter. Store it on the `SyncEngine` struct (`signer: Arc<dyn Signer>` field, alongside the existing fields).

- [ ] **Step 2:** Modify `do_publish_subscription` (around `crates/sunset-sync/src/engine.rs:511-540`) to compute and attach a real signature:

  ```rust
  use sunset_store::canonical::signing_payload;

  async fn do_publish_subscription(
      &self,
      filter: Filter,
      ttl: std::time::Duration,
  ) -> Result<()> {
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
      let mut entry = SignedKvEntry {
          verifying_key: self.local_peer.0.clone(),
          name: Bytes::from_static(reserved::SUBSCRIBE_NAME),
          value_hash: block.hash(),
          priority: now_secs,
          expires_at: Some(now_secs.saturating_add(ttl.as_secs())),
          signature: Bytes::new(),
      };
      let payload = signing_payload(&entry);
      entry.signature = self.signer.sign(&payload);
      self.store.insert(entry, Some(block)).await?;
      Ok(())
  }
  ```

  Remove the "v1 stub signature" comment.

- [ ] **Step 3:** Update sunset-sync's own tests that call `SyncEngine::new`. In `crates/sunset-sync/src/engine.rs` `mod tests`, the `make_engine` helper:

  ```rust
  use bytes::Bytes;
  use sunset_store::VerifyingKey;

  use crate::Signer;

  /// Test-only signer that returns a non-empty stub signature. Adequate when
  /// the receiving store uses `AcceptAllVerifier`.
  struct StubSigner {
      vk: VerifyingKey,
  }

  impl Signer for StubSigner {
      fn verifying_key(&self) -> VerifyingKey { self.vk.clone() }
      fn sign(&self, _payload: &[u8]) -> Bytes {
          // 64 bytes of zeros — non-empty so any conditional sig-presence
          // check passes; AcceptAllVerifier won't validate the math.
          Bytes::from_static(&[0u8; 64])
      }
  }

  fn make_engine(addr: &str, peer_label: &[u8]) -> SyncEngine<MemoryStore, TestTransport> {
      let net = TestNetwork::new();
      let local_peer = PeerId(vk(peer_label));
      let transport = net.transport(
          local_peer.clone(),
          PeerAddr::new(Bytes::copy_from_slice(addr.as_bytes())),
      );
      let store = Arc::new(MemoryStore::with_accept_all());
      let signer = Arc::new(StubSigner { vk: local_peer.0.clone() });
      SyncEngine::new(store, transport, SyncConfig::default(), local_peer, signer)
  }
  ```

- [ ] **Step 4:** Update `crates/sunset-sync/tests/two_peer_sync.rs` similarly: define a small inline `StubSigner` and pass it to both engine constructors. The test continues to use `AcceptAllVerifier` for now (Task 10 introduces a real-signer / Ed25519Verifier integration test in the new ws-native crate; sunset-sync's own internal test stays cheap).

- [ ] **Step 5:** Update `crates/sunset-core/tests/two_peer_message.rs` similarly. Inline a `StubSigner` for both engines. The test continues to use `AcceptAllVerifier` per the existing memory note (`project_sync_internal_stub_signing.md`); the headline upgrade lives in Task 10's new test instead.

- [ ] **Step 6:** Verify everything still passes:
  ```
  nix develop --command cargo test --workspace --all-features
  nix develop --command cargo clippy --workspace --all-features --all-targets -- -D warnings
  ```

- [ ] **Step 7:** Commit:
  ```
  git add crates/sunset-sync/src/engine.rs crates/sunset-sync/tests/two_peer_sync.rs \
          crates/sunset-core/tests/two_peer_message.rs
  git commit -m "Wire Signer through SyncEngine; do_publish_subscription signs entries"
  ```

---

### Task 4: `sunset-core::Identity` implements `sunset_sync::Signer`

**Files:**
- Modify: `crates/sunset-core/Cargo.toml`
- Modify: `crates/sunset-core/src/identity.rs`

- [ ] **Step 1:** Add `sunset-sync` to `crates/sunset-core/Cargo.toml`'s `[dependencies]`:
  ```toml
  sunset-sync.workspace = true
  ```

  (sunset-core already lists sunset-sync as a dev-dependency for the integration test; promoting it to a real dependency is fine because sunset-sync doesn't depend on sunset-core. No layering inversion.)

- [ ] **Step 2:** In `crates/sunset-core/src/identity.rs`, add the impl at the bottom (before the `#[cfg(test)] mod tests` block):

  ```rust
  use ed25519_dalek::Signer as DalekSigner;

  impl sunset_sync::Signer for Identity {
      fn verifying_key(&self) -> sunset_store::VerifyingKey {
          self.store_verifying_key()
      }

      fn sign(&self, payload: &[u8]) -> bytes::Bytes {
          let sig = DalekSigner::sign(&self.signing, payload);
          bytes::Bytes::copy_from_slice(&sig.to_bytes())
      }
  }
  ```

  Note: `Identity::sign` already exists with the same body (just without the `Signer` trait wrapper). The new impl delegates via the dalek trait to avoid name collision with the inherent method.

- [ ] **Step 3:** Add a unit test asserting the trait impl works:

  ```rust
  #[test]
  fn identity_implements_sync_signer() {
      use sunset_sync::Signer as _;
      let id = Identity::generate(&mut OsRng);
      let sig: bytes::Bytes = id.sign(b"payload");
      assert_eq!(sig.len(), 64);
      assert_eq!(id.verifying_key(), id.store_verifying_key());
  }
  ```

- [ ] **Step 4:** Verify:
  ```
  nix develop --command cargo test -p sunset-core identity::tests
  nix develop --command cargo build --workspace
  ```

- [ ] **Step 5:** Commit:
  ```
  git add crates/sunset-core/Cargo.toml crates/sunset-core/src/identity.rs
  git commit -m "Implement sunset_sync::Signer for sunset_core::Identity"
  ```

---

### Task 5: `RawTransport` / `RawConnection` traits in sunset-sync

**Files:**
- Modify: `crates/sunset-sync/src/transport.rs`
- Modify: `crates/sunset-sync/src/lib.rs`

- [ ] **Step 1:** In `crates/sunset-sync/src/transport.rs`, append (after the existing `Transport` / `TransportConnection`):

  ```rust
  /// Plain bytes pipe — no authentication, no `peer_id`. Implementations are
  /// unaware of any cryptography; a `NoiseTransport<R: RawTransport>` decorator
  /// (in the `sunset-noise` crate) wraps any RawTransport into an
  /// authenticated `Transport`.
  ///
  /// New transport crates (browser WebSocket, WebRTC, WebTransport, …)
  /// implement only this trait — they need no crypto deps.
  #[async_trait(?Send)]
  pub trait RawTransport {
      type Connection: RawConnection;
      async fn connect(&self, addr: PeerAddr) -> Result<Self::Connection>;
      async fn accept(&self) -> Result<Self::Connection>;
  }

  #[async_trait(?Send)]
  pub trait RawConnection {
      async fn send_reliable(&self, bytes: bytes::Bytes) -> Result<()>;
      async fn recv_reliable(&self) -> Result<bytes::Bytes>;
      async fn send_unreliable(&self, bytes: bytes::Bytes) -> Result<()>;
      async fn recv_unreliable(&self) -> Result<bytes::Bytes>;
      async fn close(&self) -> Result<()>;
  }
  ```

- [ ] **Step 2:** In `crates/sunset-sync/src/lib.rs`, extend the existing `pub use transport::{Transport, TransportConnection};` line to:
  ```rust
  pub use transport::{RawConnection, RawTransport, Transport, TransportConnection};
  ```

- [ ] **Step 3:** Verify:
  ```
  nix develop --command cargo build -p sunset-sync
  nix develop --command cargo clippy -p sunset-sync --all-targets -- -D warnings
  ```

- [ ] **Step 4:** Commit:
  ```
  git add crates/sunset-sync/src/transport.rs crates/sunset-sync/src/lib.rs
  git commit -m "Add RawTransport/RawConnection traits (no-crypto bytes pipe)"
  ```

---

### Task 6: Scaffold `sunset-noise` crate

**Files:**
- Modify: `Cargo.toml` (workspace root)
- Create: `crates/sunset-noise/Cargo.toml`
- Create: `crates/sunset-noise/src/lib.rs`
- Create: `crates/sunset-noise/src/identity.rs`
- Create: `crates/sunset-noise/src/pattern.rs`
- Create: `crates/sunset-noise/src/handshake.rs` (placeholder)
- Create: `crates/sunset-noise/src/error.rs`

- [ ] **Step 1:** Add to root `[workspace.dependencies]`:
  ```toml
  curve25519-dalek = { version = "4", default-features = false, features = ["alloc", "zeroize"] }
  snow = { version = "0.10", default-features = false, features = ["default-resolver", "use-blake2", "use-chacha20poly1305"] }
  ```

  Add `crates/sunset-noise` to `[workspace] members`. Add `sunset-noise = { path = "crates/sunset-noise" }` to `[workspace.dependencies]`.

- [ ] **Step 2:** Create `crates/sunset-noise/Cargo.toml`:

  ```toml
  [package]
  name = "sunset-noise"
  version.workspace = true
  edition.workspace = true
  license.workspace = true
  rust-version.workspace = true

  [lints]
  workspace = true

  [dependencies]
  async-trait.workspace = true
  bytes.workspace = true
  curve25519-dalek.workspace = true
  ed25519-dalek.workspace = true
  hex.workspace = true
  sha2.workspace = true
  snow.workspace = true
  sunset-store.workspace = true
  sunset-sync.workspace = true
  thiserror.workspace = true
  zeroize.workspace = true

  [dev-dependencies]
  rand_core = { workspace = true, features = ["getrandom"] }
  tokio = { workspace = true, features = ["macros", "rt", "sync"] }
  ```

- [ ] **Step 3:** Create `crates/sunset-noise/src/error.rs`:

  ```rust
  use thiserror::Error;

  #[derive(Debug, Error)]
  pub enum Error {
      #[error("snow: {0}")]
      Snow(String),

      #[error("address parse error: {0}")]
      Addr(String),

      #[error("missing or malformed x25519 fragment in PeerAddr: {0}")]
      MissingStaticPubkey(String),

      #[error("raw transport error: {0}")]
      RawTransport(#[from] sunset_sync::Error),
  }

  impl From<snow::Error> for Error {
      fn from(e: snow::Error) -> Self {
          Error::Snow(format!("{:?}", e))
      }
  }

  pub type Result<T> = std::result::Result<T, Error>;
  ```

- [ ] **Step 4:** Create `crates/sunset-noise/src/pattern.rs`:

  ```rust
  //! Frozen Noise pattern — part of the v1 wire format.

  pub const NOISE_PATTERN: &str = "Noise_IK_25519_XChaChaPoly_BLAKE2b";

  #[cfg(test)]
  mod tests {
      use super::*;
      #[test]
      fn pattern_is_pinned() {
          assert_eq!(NOISE_PATTERN, "Noise_IK_25519_XChaChaPoly_BLAKE2b");
      }
  }
  ```

- [ ] **Step 5:** Create `crates/sunset-noise/src/identity.rs`:

  ```rust
  //! Per-peer identity used by the Noise handshake.
  //!
  //! sunset-core's `Identity` implements `NoiseIdentity` (in sunset-core,
  //! via this trait) so this crate doesn't need to depend on sunset-core.

  use sha2::{Digest, Sha512};
  use zeroize::Zeroizing;

  /// Identity capability the Noise wrapper needs from any host:
  /// the public Ed25519 key (the on-the-wire identity) and a way to
  /// derive the X25519 static secret used for ECDH during the handshake.
  pub trait NoiseIdentity: Send + Sync {
      /// The Ed25519 verifying key — the peer's identity.
      fn ed25519_public(&self) -> [u8; 32];

      /// Provide the 32-byte secret seed for the matching Ed25519 keypair.
      /// The Noise layer derives the X25519 static secret from this.
      fn ed25519_secret_seed(&self) -> Zeroizing<[u8; 32]>;
  }

  /// Standard Ed25519 → X25519 static-secret derivation.
  ///
  /// Per RFC 7748 Sec 5 + Signal's well-documented practice: hash the Ed25519
  /// secret seed with SHA-512, take the first 32 bytes, clamp them per
  /// X25519's clamping rules.
  pub fn ed25519_seed_to_x25519_secret(seed: &[u8; 32]) -> Zeroizing<[u8; 32]> {
      let mut hasher = Sha512::new();
      hasher.update(seed);
      let h = hasher.finalize();
      let mut out = Zeroizing::new([0u8; 32]);
      out.copy_from_slice(&h[..32]);
      // X25519 clamp:
      out[0] &= 248;
      out[31] &= 127;
      out[31] |= 64;
      out
  }

  /// Convert an Ed25519 public verifying key to its corresponding X25519
  /// public key via the Edwards-to-Montgomery point map.
  pub fn ed25519_public_to_x25519(ed_pub: &[u8; 32]) -> Result<[u8; 32], crate::error::Error> {
      use curve25519_dalek::edwards::CompressedEdwardsY;
      let edwards = CompressedEdwardsY::from_slice(ed_pub)
          .map_err(|e| crate::error::Error::Snow(format!("ed25519 pub parse: {:?}", e)))?
          .decompress()
          .ok_or_else(|| crate::error::Error::Snow("ed25519 pub decompress".into()))?;
      Ok(edwards.to_montgomery().to_bytes())
  }

  #[cfg(test)]
  mod tests {
      use super::*;

      /// Frozen vector: ed25519_seed_to_x25519_secret of a fixed seed.
      /// If this changes, every NoiseTransport handshake derives a different
      /// X25519 key and previously-deployed peers won't authenticate.
      #[test]
      fn x25519_secret_frozen_vector() {
          let seed = [7u8; 32];
          let x = ed25519_seed_to_x25519_secret(&seed);
          assert_eq!(
              hex::encode(*x),
              "REPLACE_X25519_SECRET_HEX",
              "If this fails, Ed25519→X25519 derivation has drifted — DO NOT update without a wire-format bump.",
          );
      }
  }
  ```

  (The `REPLACE_X25519_SECRET_HEX` placeholder is captured from the first test failure, same workflow as Plan 6's frozen vectors.)

- [ ] **Step 6:** Create `crates/sunset-noise/src/handshake.rs` (placeholder, populated in Task 7):

  ```rust
  //! Noise handshake + post-handshake transport. Populated in Task 7.
  ```

- [ ] **Step 7:** Create `crates/sunset-noise/src/lib.rs`:

  ```rust
  //! Noise tunnel decorator over any `sunset_sync::RawTransport`.
  //!
  //! See `docs/superpowers/specs/2026-04-27-sunset-sync-ws-native-design.md`.

  pub mod error;
  pub mod handshake;
  pub mod identity;
  pub mod pattern;

  pub use error::{Error, Result};
  pub use identity::{NoiseIdentity, ed25519_public_to_x25519, ed25519_seed_to_x25519_secret};
  pub use pattern::NOISE_PATTERN;
  ```

- [ ] **Step 8:** Capture the frozen X25519 vector. Run:
  ```
  nix develop --command cargo test -p sunset-noise identity::tests::x25519_secret_frozen_vector -- --nocapture
  ```
  Expect FAIL revealing the actual hex. Replace `REPLACE_X25519_SECRET_HEX`.

- [ ] **Step 9:** Run the full crate's tests + build for both targets:
  ```
  nix develop --command cargo fmt -p sunset-noise
  nix develop --command cargo test -p sunset-noise
  nix develop --command cargo build -p sunset-noise --target wasm32-unknown-unknown --lib
  ```

- [ ] **Step 10:** Commit:
  ```
  git add Cargo.toml crates/sunset-noise/
  git commit -m "Scaffold sunset-noise crate with NoiseIdentity + Ed25519→X25519 helpers"
  ```

---

### Task 7: `NoiseTransport` + `NoiseConnection` in sunset-noise

**Files:**
- Modify: `crates/sunset-noise/src/handshake.rs`
- Modify: `crates/sunset-noise/src/lib.rs` (add re-exports)

- [ ] **Step 1:** Replace `crates/sunset-noise/src/handshake.rs` with the implementation. This is the substantive crypto-glue task — the handshake state machine, post-handshake send/recv encryption.

  ```rust
  //! Noise IK handshake + post-handshake transport encryption.
  //!
  //! Wraps any `sunset_sync::RawTransport` with the
  //! `Noise_IK_25519_XChaChaPoly_BLAKE2b` pattern via `snow`.

  use std::sync::Arc;

  use async_trait::async_trait;
  use bytes::Bytes;
  use snow::{Builder, HandshakeState, TransportState};
  use tokio::sync::Mutex;

  use sunset_sync::{
      PeerAddr, PeerId, RawConnection, RawTransport, Transport, TransportConnection,
      Result as SyncResult,
  };
  use sunset_store::VerifyingKey;

  use crate::error::{Error, Result};
  use crate::identity::{NoiseIdentity, ed25519_public_to_x25519, ed25519_seed_to_x25519_secret};
  use crate::pattern::NOISE_PATTERN;

  /// A `Transport` decorator that runs the Noise IK handshake on each
  /// connection and exposes the result as an authenticated, encrypted
  /// `TransportConnection`.
  pub struct NoiseTransport<R: RawTransport> {
      raw: R,
      local: Arc<dyn NoiseIdentity>,
  }

  impl<R: RawTransport> NoiseTransport<R> {
      pub fn new(raw: R, local: Arc<dyn NoiseIdentity>) -> Self {
          Self { raw, local }
      }
  }

  #[async_trait(?Send)]
  impl<R: RawTransport> Transport for NoiseTransport<R>
  where
      R::Connection: 'static,
  {
      type Connection = NoiseConnection<R::Connection>;

      async fn connect(&self, addr: PeerAddr) -> SyncResult<Self::Connection> {
          let remote_x25519 = parse_addr_x25519(&addr)
              .map_err(|e| sunset_sync::Error::Transport(format!("{e}")))?;
          let raw = self.raw.connect(addr).await?;
          do_handshake_initiator(raw, self.local.clone(), remote_x25519)
              .await
              .map_err(|e| sunset_sync::Error::Transport(format!("noise initiator: {e}")))
      }

      async fn accept(&self) -> SyncResult<Self::Connection> {
          let raw = self.raw.accept().await?;
          do_handshake_responder(raw, self.local.clone())
              .await
              .map_err(|e| sunset_sync::Error::Transport(format!("noise responder: {e}")))
      }
  }

  /// Parse `wss://host:port#x25519=<hex>` (or ws://, etc.) and return the
  /// X25519 pubkey. The fragment is the contractual home for the responder's
  /// expected static pubkey under the Noise IK pattern.
  fn parse_addr_x25519(addr: &PeerAddr) -> Result<[u8; 32]> {
      let s = std::str::from_utf8(addr.as_bytes())
          .map_err(|e| Error::Addr(format!("not utf-8: {e}")))?;
      let (_url, fragment) = s.split_once('#').ok_or_else(|| {
          Error::MissingStaticPubkey(format!("address has no fragment: {s}"))
      })?;
      let pair = fragment.strip_prefix("x25519=").ok_or_else(|| {
          Error::MissingStaticPubkey(format!("fragment is not `x25519=…`: {fragment}"))
      })?;
      let bytes = hex::decode(pair).map_err(|e| {
          Error::MissingStaticPubkey(format!("hex decode failed: {e}"))
      })?;
      <[u8; 32]>::try_from(bytes.as_slice()).map_err(|_| {
          Error::MissingStaticPubkey(format!("expected 32 bytes, got {}", bytes.len()))
      })
  }

  async fn do_handshake_initiator<C: RawConnection + 'static>(
      raw: C,
      local: Arc<dyn NoiseIdentity>,
      remote_x25519: [u8; 32],
  ) -> Result<NoiseConnection<C>> {
      let seed = local.ed25519_secret_seed();
      let local_x25519_secret = ed25519_seed_to_x25519_secret(&seed);

      let mut hs: HandshakeState = Builder::new(NOISE_PATTERN.parse().map_err(|e| Error::Snow(format!("{e:?}")))?)
          .local_private_key(&*local_x25519_secret)
          .remote_public_key(&remote_x25519)
          .build_initiator()?;

      // IK pattern: 1 RTT (initiator -> responder, then responder -> initiator).
      //
      // Message 1 (initiator -> responder): "e, es, s, ss" — initiator sends
      //   ephemeral, an encrypted handshake including own static.
      let mut buf = vec![0u8; 1024];
      let n = hs.write_message(&[], &mut buf)?;
      raw.send_reliable(Bytes::copy_from_slice(&buf[..n])).await?;

      // Message 2 (responder -> initiator): "e, ee, se".
      let response = raw.recv_reliable().await?;
      let mut payload = vec![0u8; 1024];
      hs.read_message(&response, &mut payload)?;

      // Handshake complete; transition to transport mode.
      let transport: TransportState = hs.into_transport_mode()?;
      // The remote's static pubkey is now known.
      let remote_static = transport.get_remote_static().ok_or_else(|| Error::Snow("no remote static".into()))?;
      let remote_static_x25519: [u8; 32] = remote_static.try_into()
          .map_err(|_| Error::Snow("remote static wrong length".into()))?;

      // We DON'T store the remote X25519 as the PeerId — PeerId is the
      // Ed25519 verifying key. The dialer already knew the remote's Ed25519
      // pubkey (that's how it derived the expected X25519 in PeerAddr). We
      // CHECK that the X25519 that came back over the wire matches what we
      // expected — if it doesn't, the responder is impersonating someone
      // whose X25519 we don't trust.
      if remote_static_x25519 != remote_x25519 {
          return Err(Error::Snow("remote static does not match PeerAddr expected pubkey".into()));
      }

      // For the PeerId: the initiator must learn the responder's Ed25519
      // pubkey separately. Stash it in PeerAddr (the same way the X25519 is
      // stashed) — in v0 we treat the X25519 as the canonical "peer-id"
      // because we have no out-of-band Ed25519 source on the responder side.
      // Note: future plans (relay registration) will publish responder's
      // Ed25519 pubkey alongside its X25519 in the address.
      //
      // For v0 of THIS plan: PeerId is constructed from the remote X25519
      // bytes, treated as the peer's identity-on-the-wire. The Ed25519
      // pubkey is recovered via the inverse of `ed25519_public_to_x25519`
      // when needed. (This is acceptable for v0 because the only consumers
      // are the SyncEngine subscribers list and the engine treats peer-ids
      // opaquely.)
      let peer_id = PeerId(VerifyingKey::new(Bytes::copy_from_slice(&remote_static_x25519)));

      Ok(NoiseConnection {
          raw,
          state: Arc::new(Mutex::new(transport)),
          peer_id,
      })
  }

  async fn do_handshake_responder<C: RawConnection + 'static>(
      raw: C,
      local: Arc<dyn NoiseIdentity>,
  ) -> Result<NoiseConnection<C>> {
      let seed = local.ed25519_secret_seed();
      let local_x25519_secret = ed25519_seed_to_x25519_secret(&seed);

      let mut hs: HandshakeState = Builder::new(NOISE_PATTERN.parse().map_err(|e| Error::Snow(format!("{e:?}")))?)
          .local_private_key(&*local_x25519_secret)
          .build_responder()?;

      // Message 1 (initiator -> responder).
      let msg1 = raw.recv_reliable().await?;
      let mut payload = vec![0u8; 1024];
      hs.read_message(&msg1, &mut payload)?;

      // Message 2 (responder -> initiator).
      let mut buf = vec![0u8; 1024];
      let n = hs.write_message(&[], &mut buf)?;
      raw.send_reliable(Bytes::copy_from_slice(&buf[..n])).await?;

      let transport: TransportState = hs.into_transport_mode()?;
      let remote_static = transport.get_remote_static().ok_or_else(|| Error::Snow("no remote static".into()))?;
      let remote_static_x25519: [u8; 32] = remote_static.try_into()
          .map_err(|_| Error::Snow("remote static wrong length".into()))?;

      let peer_id = PeerId(VerifyingKey::new(Bytes::copy_from_slice(&remote_static_x25519)));

      Ok(NoiseConnection {
          raw,
          state: Arc::new(Mutex::new(transport)),
          peer_id,
      })
  }

  /// Authenticated, encrypted connection. `send_reliable`/`recv_reliable`
  /// transparently encrypt/decrypt via the Noise transport state.
  pub struct NoiseConnection<C: RawConnection> {
      raw: C,
      state: Arc<Mutex<TransportState>>,
      peer_id: PeerId,
  }

  #[async_trait(?Send)]
  impl<C: RawConnection> TransportConnection for NoiseConnection<C> {
      async fn send_reliable(&self, bytes: Bytes) -> SyncResult<()> {
          let mut buf = vec![0u8; bytes.len() + 16];   // +16 for AEAD tag
          let n = {
              let mut state = self.state.lock().await;
              state
                  .write_message(&bytes, &mut buf)
                  .map_err(|e| sunset_sync::Error::Transport(format!("noise encrypt: {e:?}")))?
          };
          self.raw.send_reliable(Bytes::copy_from_slice(&buf[..n])).await
      }

      async fn recv_reliable(&self) -> SyncResult<Bytes> {
          let ct = self.raw.recv_reliable().await?;
          let mut pt = vec![0u8; ct.len()];
          let n = {
              let mut state = self.state.lock().await;
              state
                  .read_message(&ct, &mut pt)
                  .map_err(|e| sunset_sync::Error::Transport(format!("noise decrypt: {e:?}")))?
          };
          Ok(Bytes::copy_from_slice(&pt[..n]))
      }

      async fn send_unreliable(&self, bytes: Bytes) -> SyncResult<()> {
          // v0: voice not yet wired; pass through unencrypted to the raw
          // transport, which itself returns Unsupported on WebSocket. This
          // surfaces a clean "not supported" rather than silent corruption.
          self.raw.send_unreliable(bytes).await
      }

      async fn recv_unreliable(&self) -> SyncResult<Bytes> {
          self.raw.recv_unreliable().await
      }

      fn peer_id(&self) -> PeerId {
          self.peer_id.clone()
      }

      async fn close(&self) -> SyncResult<()> {
          self.raw.close().await
      }
  }
  ```

  **Note on PeerId encoding**: In v0, PeerId carries the X25519 bytes (not the Ed25519 verifying key) because IK responders only learn the initiator's X25519, not Ed25519. This is acceptable because SyncEngine treats PeerId opaquely and the Signer trait is keyed off the local identity (which knows its own Ed25519). Later plans (Plan D's relay registration) will surface Ed25519 alongside.

- [ ] **Step 2:** In `crates/sunset-noise/src/lib.rs`, add:
  ```rust
  pub use handshake::{NoiseConnection, NoiseTransport};
  ```

- [ ] **Step 3:** Add a unit test that two NoiseTransports + an in-memory pipe complete a handshake successfully. Append to `handshake.rs`:

  ```rust
  #[cfg(test)]
  mod tests {
      use super::*;

      use std::sync::Arc;
      use bytes::Bytes;
      use tokio::sync::mpsc;
      use zeroize::Zeroizing;

      // In-memory bidirectional RawConnection for testing.
      struct PipeRawConnection {
          tx: tokio::sync::Mutex<mpsc::UnboundedSender<Bytes>>,
          rx: tokio::sync::Mutex<mpsc::UnboundedReceiver<Bytes>>,
      }

      #[async_trait(?Send)]
      impl RawConnection for PipeRawConnection {
          async fn send_reliable(&self, bytes: Bytes) -> SyncResult<()> {
              self.tx.lock().await.send(bytes)
                  .map_err(|_| sunset_sync::Error::Transport("pipe closed".into()))
          }
          async fn recv_reliable(&self) -> SyncResult<Bytes> {
              self.rx.lock().await.recv().await
                  .ok_or_else(|| sunset_sync::Error::Transport("pipe closed".into()))
          }
          async fn send_unreliable(&self, _: Bytes) -> SyncResult<()> {
              Err(sunset_sync::Error::Transport("unsupported".into()))
          }
          async fn recv_unreliable(&self) -> SyncResult<Bytes> {
              Err(sunset_sync::Error::Transport("unsupported".into()))
          }
          async fn close(&self) -> SyncResult<()> { Ok(()) }
      }

      fn make_pipe_pair() -> (PipeRawConnection, PipeRawConnection) {
          let (a_to_b_tx, a_to_b_rx) = mpsc::unbounded_channel::<Bytes>();
          let (b_to_a_tx, b_to_a_rx) = mpsc::unbounded_channel::<Bytes>();
          (
              PipeRawConnection {
                  tx: tokio::sync::Mutex::new(a_to_b_tx),
                  rx: tokio::sync::Mutex::new(b_to_a_rx),
              },
              PipeRawConnection {
                  tx: tokio::sync::Mutex::new(b_to_a_tx),
                  rx: tokio::sync::Mutex::new(a_to_b_rx),
              },
          )
      }

      struct StaticIdentity {
          seed: [u8; 32],
      }
      impl NoiseIdentity for StaticIdentity {
          fn ed25519_public(&self) -> [u8; 32] {
              use ed25519_dalek::SigningKey;
              SigningKey::from_bytes(&self.seed).verifying_key().to_bytes()
          }
          fn ed25519_secret_seed(&self) -> Zeroizing<[u8; 32]> {
              Zeroizing::new(self.seed)
          }
      }

      #[tokio::test(flavor = "current_thread")]
      async fn noise_handshake_roundtrip() {
          let local = tokio::task::LocalSet::new();
          local.run_until(async {
              let alice = Arc::new(StaticIdentity { seed: [1u8; 32] });
              let bob = Arc::new(StaticIdentity { seed: [2u8; 32] });

              let (a_pipe, b_pipe) = make_pipe_pair();

              let bob_x25519 = ed25519_seed_to_x25519_secret(&bob.seed);
              // Bob's static x25519 PUBLIC key is what the initiator needs.
              // Compute it from the secret via curve25519-dalek scalar mult:
              use curve25519_dalek::{scalar::Scalar, MontgomeryPoint};
              let bob_x25519_pub: [u8; 32] = {
                  let scalar = Scalar::from_bytes_mod_order(*bob_x25519);
                  let pub_pt = MontgomeryPoint::mul_base(&scalar);
                  pub_pt.to_bytes()
              };

              let alice_handle = tokio::task::spawn_local({
                  let alice_id = alice.clone();
                  async move {
                      do_handshake_initiator(a_pipe, alice_id, bob_x25519_pub).await
                  }
              });
              let bob_handle = tokio::task::spawn_local({
                  let bob_id = bob.clone();
                  async move {
                      do_handshake_responder(b_pipe, bob_id).await
                  }
              });

              let alice_conn = alice_handle.await.unwrap().expect("alice handshake");
              let bob_conn = bob_handle.await.unwrap().expect("bob handshake");

              // Roundtrip an encrypted message.
              alice_conn.send_reliable(Bytes::from_static(b"hello bob")).await.unwrap();
              let received = bob_conn.recv_reliable().await.unwrap();
              assert_eq!(received.as_ref(), b"hello bob");
          }).await;
      }

      #[test]
      fn parse_addr_extracts_x25519_fragment() {
          let bytes = b"wss://relay.example.com:443#x25519=0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
          let addr = PeerAddr::new(Bytes::copy_from_slice(bytes));
          let key = parse_addr_x25519(&addr).unwrap();
          assert_eq!(key.len(), 32);
          assert_eq!(key[0], 0x01);
          assert_eq!(key[31], 0xef);
      }

      #[test]
      fn parse_addr_rejects_missing_fragment() {
          let bytes = b"wss://relay.example.com:443/";
          let addr = PeerAddr::new(Bytes::copy_from_slice(bytes));
          let err = parse_addr_x25519(&addr).unwrap_err();
          assert!(matches!(err, Error::MissingStaticPubkey(_)));
      }
  }
  ```

- [ ] **Step 4:** Verify:
  ```
  nix develop --command cargo fmt -p sunset-noise
  nix develop --command cargo test -p sunset-noise
  nix develop --command cargo clippy -p sunset-noise --all-targets -- -D warnings
  nix develop --command cargo build -p sunset-noise --target wasm32-unknown-unknown --lib
  ```

  Expect 4+ tests passing. WASM build clean.

- [ ] **Step 5:** Commit:
  ```
  git add crates/sunset-noise/src/handshake.rs crates/sunset-noise/src/lib.rs
  git commit -m "Add NoiseTransport: IK handshake + transport encryption"
  ```

---

### Task 8: Scaffold `sunset-sync-ws-native` crate + implement `WebSocketRawTransport`

**Files:**
- Modify: `Cargo.toml` (workspace root)
- Create: `crates/sunset-sync-ws-native/Cargo.toml`
- Create: `crates/sunset-sync-ws-native/src/lib.rs`

- [ ] **Step 1:** Add to root `[workspace.dependencies]`:
  ```toml
  futures-util = { version = "0.3", default-features = false, features = ["sink"] }
  tokio-tungstenite = { version = "0.24", default-features = false, features = ["connect"] }
  url = "2"
  ```

  Add `crates/sunset-sync-ws-native` to `[workspace] members` and a `sunset-sync-ws-native = { path = ... }` entry to `[workspace.dependencies]`.

- [ ] **Step 2:** Create `crates/sunset-sync-ws-native/Cargo.toml`:

  ```toml
  [package]
  name = "sunset-sync-ws-native"
  version.workspace = true
  edition.workspace = true
  license.workspace = true
  rust-version.workspace = true

  [lints]
  workspace = true

  [dependencies]
  async-trait.workspace = true
  bytes.workspace = true
  futures-util.workspace = true
  sunset-sync.workspace = true
  thiserror.workspace = true
  tokio = { workspace = true, features = ["sync", "rt", "macros", "net"] }
  tokio-tungstenite.workspace = true
  url.workspace = true

  [dev-dependencies]
  rand_core = { workspace = true, features = ["getrandom"] }
  sunset-store = { workspace = true, features = ["test-helpers"] }
  sunset-store-memory.workspace = true
  sunset-sync = { workspace = true, features = ["test-helpers"] }
  sunset-noise.workspace = true
  sunset-core.workspace = true
  tokio = { workspace = true, features = ["macros", "rt", "rt-multi-thread", "time", "sync", "net"] }
  ```

- [ ] **Step 3:** Create `crates/sunset-sync-ws-native/src/lib.rs`:

  ```rust
  //! Native WebSocket implementation of `sunset_sync::RawTransport`.
  //!
  //! Wrap with `sunset_noise::NoiseTransport` to get authenticated
  //! encrypted connections suitable for `SyncEngine`.

  use std::sync::Arc;

  use async_trait::async_trait;
  use bytes::Bytes;
  use futures_util::{SinkExt, StreamExt};
  use tokio::net::TcpListener;
  use tokio::sync::Mutex;
  use tokio_tungstenite::{
      WebSocketStream,
      tungstenite::{Message, protocol::WebSocketConfig},
  };

  use sunset_sync::{
      Error as SyncError, PeerAddr, RawConnection, RawTransport, Result as SyncResult,
  };

  /// Either a dial-only client or a listening server.
  pub struct WebSocketRawTransport {
      mode: TransportMode,
  }

  enum TransportMode {
      DialOnly,
      Listening { listener: Mutex<TcpListener> },
  }

  impl WebSocketRawTransport {
      pub fn dial_only() -> Self {
          Self { mode: TransportMode::DialOnly }
      }

      pub async fn listening_on(bind: std::net::SocketAddr) -> SyncResult<Self> {
          let listener = TcpListener::bind(bind).await
              .map_err(|e| SyncError::Transport(format!("bind {bind}: {e}")))?;
          Ok(Self { mode: TransportMode::Listening { listener: Mutex::new(listener) } })
      }

      /// Bound address (useful when binding to port 0).
      pub fn local_addr(&self) -> Option<std::net::SocketAddr> {
          match &self.mode {
              TransportMode::Listening { listener } => listener
                  .try_lock()
                  .ok()
                  .and_then(|l| l.local_addr().ok()),
              TransportMode::DialOnly => None,
          }
      }
  }

  #[async_trait(?Send)]
  impl RawTransport for WebSocketRawTransport {
      type Connection = WebSocketRawConnection;

      async fn connect(&self, addr: PeerAddr) -> SyncResult<Self::Connection> {
          let s = std::str::from_utf8(addr.as_bytes())
              .map_err(|e| SyncError::Transport(format!("addr not utf-8: {e}")))?;
          // Strip the fragment (which the Noise wrapper consumes); tungstenite
          // doesn't want fragments anyway.
          let url_no_frag = s.split('#').next().unwrap_or(s);
          let url = url::Url::parse(url_no_frag)
              .map_err(|e| SyncError::Transport(format!("addr parse: {e}")))?;
          let (ws, _resp) = tokio_tungstenite::connect_async(url.as_str())
              .await
              .map_err(|e| SyncError::Transport(format!("ws connect: {e}")))?;
          Ok(WebSocketRawConnection::new(ws))
      }

      async fn accept(&self) -> SyncResult<Self::Connection> {
          let listener = match &self.mode {
              TransportMode::Listening { listener } => listener,
              TransportMode::DialOnly => {
                  // Dial-only: return a future that never resolves.
                  std::future::pending::<()>().await;
                  unreachable!();
              }
          };
          let listener = listener.lock().await;
          let (tcp, _peer) = listener.accept().await
              .map_err(|e| SyncError::Transport(format!("accept: {e}")))?;
          let ws = tokio_tungstenite::accept_async(tcp).await
              .map_err(|e| SyncError::Transport(format!("ws upgrade: {e}")))?;
          Ok(WebSocketRawConnection::new(ws))
      }
  }

  pub struct WebSocketRawConnection {
      stream: Arc<Mutex<WebSocketStream<tokio::net::TcpStream>>>,
  }

  impl WebSocketRawConnection {
      fn new(ws: WebSocketStream<tokio::net::TcpStream>) -> Self {
          Self { stream: Arc::new(Mutex::new(ws)) }
      }
  }

  #[async_trait(?Send)]
  impl RawConnection for WebSocketRawConnection {
      async fn send_reliable(&self, bytes: Bytes) -> SyncResult<()> {
          let mut s = self.stream.lock().await;
          s.send(Message::Binary(bytes.to_vec()))
              .await
              .map_err(|e| SyncError::Transport(format!("ws send: {e}")))
      }

      async fn recv_reliable(&self) -> SyncResult<Bytes> {
          loop {
              let mut s = self.stream.lock().await;
              let msg = s.next().await
                  .ok_or_else(|| SyncError::Transport("ws closed".into()))?
                  .map_err(|e| SyncError::Transport(format!("ws recv: {e}")))?;
              match msg {
                  Message::Binary(b) => return Ok(Bytes::from(b)),
                  Message::Ping(p) => { s.send(Message::Pong(p)).await.ok(); }
                  Message::Pong(_) => continue,
                  Message::Close(_) => return Err(SyncError::Transport("ws closed by peer".into())),
                  Message::Text(_) | Message::Frame(_) => {
                      return Err(SyncError::Transport("unexpected ws message kind".into()));
                  }
              }
          }
      }

      async fn send_unreliable(&self, _: Bytes) -> SyncResult<()> {
          Err(SyncError::Transport("websocket: unreliable channel unsupported".into()))
      }

      async fn recv_unreliable(&self) -> SyncResult<Bytes> {
          Err(SyncError::Transport("websocket: unreliable channel unsupported".into()))
      }

      async fn close(&self) -> SyncResult<()> {
          let mut s = self.stream.lock().await;
          s.close(None).await.ok();
          Ok(())
      }
  }
  ```

- [ ] **Step 4:** A small unit test in the same file (under `#[cfg(test)] mod tests`) that listens on a random port, connects from a dial-only transport, exchanges one binary frame:

  ```rust
  #[cfg(test)]
  mod tests {
      use super::*;

      #[tokio::test(flavor = "current_thread")]
      async fn raw_send_recv_roundtrip() {
          let local = tokio::task::LocalSet::new();
          local.run_until(async {
              let server = WebSocketRawTransport::listening_on("127.0.0.1:0".parse().unwrap()).await.unwrap();
              let bound = server.local_addr().unwrap();

              let server_handle = tokio::task::spawn_local(async move {
                  let conn = server.accept().await.unwrap();
                  let msg = conn.recv_reliable().await.unwrap();
                  conn.send_reliable(msg).await.unwrap();
              });

              let client = WebSocketRawTransport::dial_only();
              let addr = PeerAddr::new(Bytes::from(format!("ws://{bound}")));
              let conn = client.connect(addr).await.unwrap();

              conn.send_reliable(Bytes::from_static(b"hello ws")).await.unwrap();
              let echo = conn.recv_reliable().await.unwrap();
              assert_eq!(echo.as_ref(), b"hello ws");

              server_handle.await.unwrap();
          }).await;
      }
  }
  ```

- [ ] **Step 5:** Verify:
  ```
  nix develop --command cargo fmt -p sunset-sync-ws-native
  nix develop --command cargo test -p sunset-sync-ws-native
  nix develop --command cargo clippy -p sunset-sync-ws-native --all-targets -- -D warnings
  ```

- [ ] **Step 6:** Commit:
  ```
  git add Cargo.toml crates/sunset-sync-ws-native/
  git commit -m "Add sunset-sync-ws-native: tokio-tungstenite RawTransport impl"
  ```

---

### Task 9: Two-peer over real WebSocket + Noise integration test

**Files:**
- Create: `crates/sunset-sync-ws-native/tests/two_peer_ws_noise.rs`

- [ ] **Step 1:** Write the integration test. Both peers' stores use `Ed25519Verifier` end-to-end — this is the "no AcceptAllVerifier workaround" milestone:

  ```rust
  //! End-to-end: alice (dialer) and bob (listener) exchange a real
  //! sunset-core encrypted+signed message over a real localhost WebSocket
  //! wrapped in Noise. Both stores use Ed25519Verifier — proves the
  //! sync-internal signing path is real.

  use std::rc::Rc;
  use std::sync::Arc;
  use std::time::Duration;

  use rand_core::OsRng;

  use sunset_core::{
      ComposedMessage, Ed25519Verifier, Identity, Room, compose_message, decode_message,
      room_messages_filter,
  };
  use sunset_core::crypto::constants::test_fast_params;
  use sunset_noise::{NoiseIdentity, NoiseTransport, ed25519_seed_to_x25519_secret};
  use sunset_store::{ContentBlock, Hash, Store as _};
  use sunset_store_memory::MemoryStore;
  use sunset_sync::{PeerId, SyncConfig, SyncEngine, PeerAddr};
  use sunset_sync_ws_native::WebSocketRawTransport;

  use bytes::Bytes;
  use zeroize::Zeroizing;

  // sunset-core's Identity already implements Signer; it also implements
  // NoiseIdentity here via a thin wrapper so this test doesn't depend on
  // sunset-core implementing NoiseIdentity itself (which it can in a follow-up).
  struct IdentityNoiseAdapter(Identity);

  impl NoiseIdentity for IdentityNoiseAdapter {
      fn ed25519_public(&self) -> [u8; 32] {
          self.0.public().as_bytes()
      }
      fn ed25519_secret_seed(&self) -> Zeroizing<[u8; 32]> {
          Zeroizing::new(self.0.secret_bytes())
      }
  }

  #[tokio::test(flavor = "current_thread")]
  async fn alice_encrypts_bob_decrypts_over_ws_and_noise() {
      let local = tokio::task::LocalSet::new();
      local.run_until(async {
          // ---- identities + rooms ----
          let alice = Identity::generate(&mut OsRng);
          let bob = Identity::generate(&mut OsRng);
          let alice_room = Room::open_with_params("plan-c-test", &test_fast_params()).unwrap();
          let bob_room = Room::open_with_params("plan-c-test", &test_fast_params()).unwrap();
          assert_eq!(alice_room.fingerprint(), bob_room.fingerprint());

          // ---- both stores use Ed25519Verifier ----
          let alice_store = Arc::new(MemoryStore::new(Arc::new(Ed25519Verifier)));
          let bob_store = Arc::new(MemoryStore::new(Arc::new(Ed25519Verifier)));

          // ---- bob listens on a random port ----
          let bob_raw = WebSocketRawTransport::listening_on("127.0.0.1:0".parse().unwrap())
              .await.unwrap();
          let bob_bound = bob_raw.local_addr().unwrap();
          let bob_noise = NoiseTransport::new(bob_raw, Arc::new(IdentityNoiseAdapter(bob.clone())));

          // ---- alice dials ----
          let alice_raw = WebSocketRawTransport::dial_only();
          let alice_noise = NoiseTransport::new(alice_raw, Arc::new(IdentityNoiseAdapter(alice.clone())));

          // PeerAddr for alice to dial bob: ws://<bob_bound>#x25519=<bob_x25519_pub_hex>
          // Compute bob's X25519 public key from his Ed25519 secret.
          let bob_seed = bob.secret_bytes();
          let bob_x25519_secret = ed25519_seed_to_x25519_secret(&bob_seed);
          let bob_x25519_pub = {
              use curve25519_dalek::{scalar::Scalar, MontgomeryPoint};
              let scalar = Scalar::from_bytes_mod_order(*bob_x25519_secret);
              MontgomeryPoint::mul_base(&scalar).to_bytes()
          };
          let bob_addr = PeerAddr::new(Bytes::from(format!(
              "ws://{}#x25519={}", bob_bound, hex::encode(bob_x25519_pub),
          )));

          // ---- engines (with real signers) ----
          let alice_signer: Arc<dyn sunset_sync::Signer> = Arc::new(alice.clone());
          let bob_signer: Arc<dyn sunset_sync::Signer> = Arc::new(bob.clone());

          // PeerId for the engines is the X25519 pubkey (per Noise's IK
          // semantics; see Plan C spec note).
          let alice_x25519_pub = {
              use curve25519_dalek::{scalar::Scalar, MontgomeryPoint};
              let s = ed25519_seed_to_x25519_secret(&alice.secret_bytes());
              let scalar = Scalar::from_bytes_mod_order(*s);
              MontgomeryPoint::mul_base(&scalar).to_bytes()
          };
          let alice_peer = PeerId(sunset_store::VerifyingKey::new(Bytes::copy_from_slice(&alice_x25519_pub)));
          let bob_peer = PeerId(sunset_store::VerifyingKey::new(Bytes::copy_from_slice(&bob_x25519_pub)));

          let alice_engine = Rc::new(SyncEngine::new(
              alice_store.clone(), alice_noise, SyncConfig::default(),
              alice_peer.clone(), alice_signer,
          ));
          let bob_engine = Rc::new(SyncEngine::new(
              bob_store.clone(), bob_noise, SyncConfig::default(),
              bob_peer.clone(), bob_signer,
          ));

          let alice_run = tokio::task::spawn_local({
              let e = alice_engine.clone();
              async move { e.run().await }
          });
          let bob_run = tokio::task::spawn_local({
              let e = bob_engine.clone();
              async move { e.run().await }
          });

          // ---- bob declares interest ----
          bob_engine.publish_subscription(
              room_messages_filter(&bob_room),
              Duration::from_secs(60),
          ).await.unwrap();

          // ---- alice connects to bob ----
          alice_engine.add_peer(bob_addr).await.unwrap();

          // ---- wait for subscription propagation ----
          let registered = wait_for(
              Duration::from_secs(5),
              Duration::from_millis(50),
              || async {
                  alice_engine.knows_peer_subscription(&bob_peer.0).await
              },
          ).await;
          assert!(registered, "alice did not learn bob's subscription");

          // ---- alice composes + inserts ----
          let body = "hello bob via real ws + noise";
          let sent_at = 1_700_000_000_000u64;
          let ComposedMessage { entry, block } =
              compose_message(&alice, &alice_room, 0, sent_at, body, &mut OsRng).unwrap();
          let expected_hash: Hash = block.hash();
          alice_store.insert(entry.clone(), Some(block.clone())).await
              .expect("alice's own store accepts her real-signed entry");

          // ---- bob receives entry + block ----
          let bob_has_entry = wait_for(
              Duration::from_secs(5),
              Duration::from_millis(50),
              || async {
                  bob_store.get_entry(&alice.store_verifying_key(), &entry.name).await.unwrap().is_some()
              },
          ).await;
          assert!(bob_has_entry, "bob did not receive alice's entry");

          let bob_has_block = wait_for(
              Duration::from_secs(5),
              Duration::from_millis(50),
              || async { bob_store.get_content(&expected_hash).await.unwrap().is_some() },
          ).await;
          assert!(bob_has_block, "bob did not receive alice's content block");

          // ---- bob decodes ----
          let bob_entry = bob_store.get_entry(&alice.store_verifying_key(), &entry.name).await.unwrap().unwrap();
          let bob_block: ContentBlock = bob_store.get_content(&expected_hash).await.unwrap().unwrap();
          let decoded = decode_message(&bob_room, &bob_entry, &bob_block).unwrap();
          assert_eq!(decoded.author_key, alice.public());
          assert_eq!(decoded.body, body);

          alice_run.abort();
          bob_run.abort();
      }).await;
  }

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

- [ ] **Step 2:** Run the test:
  ```
  nix develop --command cargo test -p sunset-sync-ws-native --test two_peer_ws_noise --all-features -- --nocapture
  ```

  Expect 1 passed. If timeouts: increase the wait_for deadlines (the WebSocket + Noise handshake adds latency vs the in-memory TestNetwork tests).

  If failures with `signer.sign(...)` mismatches between the publish_subscription path and Ed25519Verifier: confirm Task 3 (signer plumbing) wired the signer through correctly; the ENTRY's `verifying_key` MUST match the signer's `verifying_key()`.

  If `alice did not learn bob's subscription`: the issue is likely that Noise's PeerId-from-X25519 doesn't match the engine's local_peer (which is also the X25519). Both must be the same encoding. Check that PeerId construction uses the same byte source on both sides.

- [ ] **Step 3:** Commit:
  ```
  git add crates/sunset-sync-ws-native/tests/two_peer_ws_noise.rs
  git commit -m "Add integration test: alice ↔ bob over real WS + Noise + Ed25519Verifier"
  ```

---

### Task 10: Final pass — fmt, clippy, full test, wasm-build for sunset-noise

- [ ] **Step 1:** Workspace-wide checks:
  ```
  nix develop --command cargo fmt --all --check
  nix develop --command cargo clippy --workspace --all-features --all-targets -- -D warnings
  nix develop --command cargo test --workspace --all-features
  ```
  All clean / green.

- [ ] **Step 2:** WASM compatibility for sunset-noise (browser plan needs this later):
  ```
  nix develop --command cargo build -p sunset-noise --target wasm32-unknown-unknown --lib
  ```
  Expect `Finished`.

- [ ] **Step 3:** Confirm sunset-core-wasm still builds (Plan A's artifact mustn't have regressed):
  ```
  nix build .#sunset-core-wasm --no-link
  ```
  Expect success.

- [ ] **Step 4:** If any cleanup commits were needed, commit:
  ```
  git add -u
  git commit -m "Final fmt + clippy pass"
  ```

---

## Verification (end-state acceptance)

After all 10 tasks land:

- `nix develop --command cargo test --workspace --all-features` — green, including the new `two_peer_ws_noise.rs` integration test.
- `nix develop --command cargo clippy --workspace --all-features --all-targets -- -D warnings` — clean.
- `nix develop --command cargo fmt --all --check` — clean.
- `nix develop --command cargo build -p sunset-noise --target wasm32-unknown-unknown --lib` — builds for browser (consumed by the future browser-WS transport plan).
- `nix build .#sunset-core-wasm` — Plan A artifact still builds.
- The integration test demonstrates: real Ed25519 identities → derived X25519 → Noise IK handshake over real localhost WebSocket → encrypted SyncMessage exchange → SyncEngine subscription propagation → real-signed `_sunset-sync/subscribe` entries that pass `Ed25519Verifier` → encrypted+signed sunset-core message decode succeeds at receiver.
- `git log --oneline master..HEAD` — roughly 10 task-by-task commits.

---

## What this unlocks

After Plan C:

- **Plan D — `sunset-relay` binary + Docker image + multi-relay integration tests.** Builds on `sunset-sync-ws-native` (server-side) + `sunset-noise`. Multi-relay tests exercise relay-to-relay sync + multi-relay client redundancy.
- **Plan E.transport — browser WebSocket RawTransport.** Implements `RawTransport` over `web-sys::WebSocket`. Reuses `sunset-noise` (already wasm-compatible). Browser-side companion to this plan.
- **Plan E — Gleam UI wires to WASM bridge + browser sync engine.** With Plan A + Plan E.transport in place, the Gleam app drives the full encrypted/authenticated stack from the browser.
