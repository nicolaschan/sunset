# sunset-core: identity + encrypted chat messages — Implementation Plan

> **For agentic workers:** Use superpowers:executing-plans (or superpowers:subagent-driven-development) to execute this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Stand up `sunset-core` as the chat-semantics layer with **end-to-end encryption, perfect forward secrecy, and per-message authentication from day 1**. Specifically, deliver "Phase A" of `docs/superpowers/specs/2026-04-26-sunset-crypto-design.md`: Ed25519 identity, the layered crypto envelope, the open-room `epoch 0` (name-derivable root key) case, and a two-peer integration test where alice composes → encrypts → signs → bob decrypts → verifies inner signature.

**Out of scope (deferred to follow-up plans, per the crypto spec's §"Implementation sequencing"):**

- **Plan 7 — epoch rotation and key bundles.** `EpochKeyStore` trait with the PFS wipe contract; `EpochKeyBundle` wire shape; X25519+HKDF wrap/unwrap; rotator code path; presence entries with ephemeral pubkeys; two-peer rotation test.
- **Plan 8 — membership ops.** `member-add` / `member-remove` ops; member-set reconstruction; invite-only-room admission enforcement; admin authorization model.
- Federated identity / handles / `sunset-trust`, hybrid PQC, multi-device, voice crypto, sender-identity hiding, sub-epoch ratcheting, time-based rotation. (See crypto spec §"Items deferred".)

What we **do** build in this plan: every chat message is AEAD-encrypted under a per-message key derived from a per-epoch root, every message body carries an inner Ed25519 signature bound to room + epoch, every entry the store sees has a verified outer Ed25519 signature, and the sunset-core public surface is enough for an open-room "post a message, receive a message" flow over `sunset-sync`.

---

## Architecture and design notes

### Layer placement

Per `docs/superpowers/specs/2026-04-25-sunset-chat-architecture-design.md` § "Layering inside `sunset-core`", this work begins the **Application layer**:

```
Application      ← sunset-core (this plan: identity + Phase A crypto envelope)
Sync             ← sunset-sync (done)
Store            ← sunset-store + backends (done)
Crypto/Transport
```

Per the crypto spec, what this plan implements concretely:

- **Layer 1 (`K_room`, fixed).** Argon2id over the room name → 32-byte `K_room` → blake3-keyed `room_fingerprint`. Used in this plan only as the *source* for `K_epoch_0` in open rooms; control-plane entries (presence, join requests, membership ops) using `K_room` directly are deferred to Plans 7–8.
- **Layer 2 (`K_epoch_n`, PFS).** This plan establishes the only epoch it needs: `K_epoch_0`. For open rooms in v1: `K_epoch_0 = HKDF(K_room, info = "sunset-chat-v1-epoch-0")`. Per-message AEAD key derived as `HKDF(K_epoch_0, info = "sunset-chat-v1-msg" || epoch_id || value_hash)`.
- **Layer 3 (per-recipient bundles).** Out of scope for this plan (no rotation). Plan 7 introduces.
- **Inner signature.** Each message body's plaintext contains a `SignedMessage` whose Ed25519 signature covers `(room_fingerprint || epoch_id || sent_at_ms || body)`. This is the *authentication* property the crypto spec mandates (the third non-negotiable).
- **Outer signature.** The store-level `SignedKvEntry.signature`, computed over the canonical entry encoding by the sender's Ed25519 identity. Same as the original Plan 6 sketch — sunset-store's `SignatureVerifier` gates acceptance.

### Cryptographic primitives chosen (matches crypto spec §"Cryptographic primitives")

| Primitive | Crate | Notes |
|---|---|---|
| Ed25519 sign/verify | `ed25519-dalek` 2.x | Already used by sunset-sync. |
| AEAD | `chacha20poly1305` 0.10.x | XChaCha20-Poly1305 — 24-byte random nonces dodge counter-management entirely. |
| HKDF | `hkdf` 0.12 + `sha2` 0.10 | HKDF-SHA256. |
| Password-KDF | `argon2` 0.5.x | Argon2id. Production params per OWASP 2023 (m=19MiB, t=2, p=1); a separate "test-fast" params set keeps tests under a second. |
| Hash / fingerprint | `blake3` 1.x | Already a workspace dep; keyed-hash for room fingerprint. |
| Secret wiping | `zeroize` 1.x | Wraps every secret 32-byte buffer in `Zeroizing<[u8; 32]>`. The PFS wipe contract becomes a real one in Plan 7; introducing `Zeroizing` here keeps every secret on the same discipline from day 1. |
| RNG (signature) | `rand_core` 0.6 | Caller injects an RNG; sunset-core itself does not pull in `getrandom`. |

### What `sunset-core` does NOT depend on (still wasm-clean)

- No `tokio`. No `getrandom` directly (callers pass an RNG explicitly).
- No `sunset-store-memory`, no `sunset-sync` — those are dev-dependencies for tests only.

### Wire-format invariants frozen in this plan

Each of these has a frozen test vector that must fail loudly on any accidental drift, mirroring the discipline of `crates/sunset-store/src/types.rs:200-211` (`content_block_hash_frozen_vector`):

1. The canonical signing payload encoding (postcard of `UnsignedEntryRef`). Same as the original Plan 6.
2. The `MessageBody` `ContentBlock` hash for a fixed sample. (We keep `MessageBody` as the *plaintext-inside-the-AEAD* type — its postcard encoding is what gets signed and then encrypted.)
3. The `EncryptedMessage` postcard encoding for a fixed sample.
4. The `K_room`, `room_fingerprint`, and `K_epoch_0` byte values for `Room::open_with_params("general", test_fast_params())`.
5. The `derive_msg_key` output for a fixed `(epoch_root, epoch_id, value_hash)` triple.
6. The crypto constant byte literals (`ROOM_KEY_SALT`, `FINGERPRINT_DOMAIN`, `EPOCH_0_DOMAIN`, `MSG_KEY_DOMAIN`).

A frozen vector failure is **not** a "fix the test" moment — it's a wire-format bump and every signature/ciphertext ever produced under v1 becomes invalid.

### Argon2id parameters

Two named parameter sets:

- `production_params()` → `Params::new(19_456, 2, 1, Some(32))` — m=19 MiB, t=2, p=1, 32-byte output. OWASP 2023 baseline.
- `test_fast_params()` → `Params::new(8, 1, 1, Some(32))` — m=8 KiB, t=1, p=1, 32-byte output. Used only by tests so the suite stays sub-second. The frozen vectors in items 4–5 above are computed under `test_fast_params()`.

Both are `argon2::Params` instances (the `argon2` crate's `Params::new` returns `Result`, not `const`-eligible, so they're behind small accessor functions in `crypto/constants.rs`).

---

## File structure

```
sunset/
├── Cargo.toml                                  # MODIFY: workspace add sunset-core member + crypto deps
├── crates/
│   └── sunset-core/                            # NEW
│       ├── Cargo.toml
│       ├── src/
│       │   ├── lib.rs                          # re-exports
│       │   ├── error.rs                        # Error + Result
│       │   ├── identity.rs                     # Identity + IdentityKey (Ed25519)
│       │   ├── canonical.rs                    # signing_payload + frozen vector (entry sigs)
│       │   ├── verifier.rs                     # Ed25519Verifier (impls sunset_store::SignatureVerifier)
│       │   ├── filters.rs                      # room_messages_filter helper
│       │   ├── crypto/
│       │   │   ├── mod.rs                      # re-export of constants/room/aead/envelope
│       │   │   ├── constants.rs                # frozen byte literals + Argon2 param accessors
│       │   │   ├── room.rs                     # Room, RoomFingerprint, K_room derivation, K_epoch_0
│       │   │   ├── aead.rs                     # derive_msg_key + AEAD encrypt/decrypt
│       │   │   └── envelope.rs                 # EncryptedMessage, SignedMessage wire shapes
│       │   └── message.rs                      # MessageBody + compose/decode (now encryption-aware)
│       └── tests/
│           └── two_peer_message.rs             # alice encrypts → bob decrypts via sunset-sync
```

Boundaries:

- `identity.rs`, `canonical.rs`, `verifier.rs`, `error.rs`, `filters.rs` — same shape as the original Plan 6 design, unchanged in intent.
- `crypto/constants.rs` — the only place that names the four domain-separation strings, the salt, and the Argon2 params. Frozen vectors live next to the constants they cover.
- `crypto/room.rs` — owns the Argon2id → `K_room`, the blake3-keyed `room_fingerprint`, and the HKDF → `K_epoch_0`. Returns secrets in `Zeroizing<[u8; 32]>`.
- `crypto/aead.rs` — pure functions: `derive_msg_key`, `aead_encrypt`, `aead_decrypt`. No state. No knowledge of envelope structure.
- `crypto/envelope.rs` — `EncryptedMessage` (the postcard-encoded ciphertext envelope that lives in `ContentBlock.data`) and `SignedMessage` (the plaintext-inside-the-AEAD with the inner signature). Pure types; no key handling.
- `message.rs` — top-level `compose_message` / `decode_message` that thread identity + room + body through identity → AEAD → store envelope. The only file that knows about all of `Identity`, `Room`, `Ed25519Verifier`, AEAD, and `SignedKvEntry` simultaneously.

---

## Tasks

### Task 1: Scaffold the `sunset-core` crate + add crypto deps to the workspace

**Files:**
- Modify: `Cargo.toml` (workspace root)
- Create: `crates/sunset-core/Cargo.toml`
- Create: `crates/sunset-core/src/lib.rs`
- Create: `crates/sunset-core/src/error.rs`
- Create placeholder modules under `crates/sunset-core/src/`

- [ ] **Step 1:** In the root `Cargo.toml`, add the new crypto deps to `[workspace.dependencies]`. Insert after the existing `bytes` line so the alphabetical-ish grouping by package family is preserved:

  ```toml
  argon2 = { version = "0.5", default-features = false, features = ["alloc"] }
  chacha20poly1305 = { version = "0.10", default-features = false, features = ["alloc"] }
  ed25519-dalek = { version = "2", default-features = false, features = ["std", "fast"] }
  hkdf = "0.12"
  rand_core = "0.6"
  sha2 = { version = "0.10", default-features = false }
  zeroize = { version = "1", default-features = false, features = ["alloc", "derive"] }
  ```

  (Defaults trimmed to keep the wasm dep tree minimal; we explicitly opt in only to features we need.)

- [ ] **Step 2:** Add `sunset-core` to the workspace members and a path-dep entry. In the root `Cargo.toml`:

  ```toml
  [workspace]
  members = ["crates/sunset-store", "crates/sunset-store-memory", "crates/sunset-store-fs", "crates/sunset-sync", "crates/sunset-core"]
  resolver = "2"
  ```

  And in `[workspace.dependencies]`, after the existing `sunset-sync = { path = ... }` line:

  ```toml
  sunset-core = { path = "crates/sunset-core" }
  ```

- [ ] **Step 3:** Create `crates/sunset-core/Cargo.toml`:

  ```toml
  [package]
  name = "sunset-core"
  version.workspace = true
  edition.workspace = true
  license.workspace = true
  rust-version.workspace = true

  [lints]
  workspace = true

  [dependencies]
  argon2.workspace = true
  blake3.workspace = true
  bytes.workspace = true
  chacha20poly1305.workspace = true
  ed25519-dalek.workspace = true
  hex.workspace = true
  hkdf.workspace = true
  postcard.workspace = true
  rand_core.workspace = true
  serde.workspace = true
  sha2.workspace = true
  sunset-store.workspace = true
  thiserror.workspace = true
  zeroize.workspace = true

  [dev-dependencies]
  rand_core = { workspace = true, features = ["getrandom"] }
  sunset-store = { workspace = true, features = ["test-helpers"] }
  sunset-store-memory.workspace = true
  sunset-sync = { workspace = true, features = ["test-helpers"] }
  tokio = { workspace = true, features = ["macros", "rt", "time", "sync"] }
  ```

- [ ] **Step 4:** Create `crates/sunset-core/src/lib.rs` (minimal — re-exports grow as later tasks add the items they name):

  ```rust
  //! sunset-core: chat-semantics layer on top of sunset-store.
  //!
  //! See `docs/superpowers/specs/2026-04-25-sunset-chat-architecture-design.md`
  //! for the layering, `docs/superpowers/specs/2026-04-26-sunset-crypto-design.md`
  //! for the crypto subsystem, and the v1 plan at
  //! `docs/superpowers/plans/2026-04-26-sunset-core-identity-and-encrypted-messages.md`
  //! for the scope of this layer.

  pub mod canonical;
  pub mod crypto;
  pub mod error;
  pub mod filters;
  pub mod identity;
  pub mod message;
  pub mod verifier;

  pub use error::{Error, Result};
  ```

- [ ] **Step 5:** Create `crates/sunset-core/src/error.rs`:

  ```rust
  //! Crate-level error type.

  use thiserror::Error;

  #[derive(Debug, Error)]
  pub enum Error {
      #[error("ed25519 signature error: {0}")]
      Signature(#[from] ed25519_dalek::SignatureError),

      #[error("postcard codec error: {0}")]
      Postcard(#[from] postcard::Error),

      #[error("AEAD authentication failed (forged or wrong key)")]
      AeadAuthFailed,

      #[error("argon2 key derivation failed: {0}")]
      Argon2(String),

      #[error("entry name did not match `<hex_fingerprint>/msg/<hex_value_hash>`: {0}")]
      BadName(String),

      #[error("content block hash did not match entry.value_hash")]
      BadValueHash,

      #[error("decoded message's room_fingerprint did not match the room used to decrypt")]
      RoomMismatch,

      #[error("decoded message's epoch_id did not match the epoch used to decrypt")]
      EpochMismatch,

      #[error("inner-signature payload too long for postcard encoding")]
      PayloadTooLarge,
  }

  pub type Result<T> = std::result::Result<T, Error>;
  ```

- [ ] **Step 6:** Create empty placeholder modules so `cargo build` succeeds. For each of `identity.rs`, `canonical.rs`, `verifier.rs`, `filters.rs`, `message.rs`, write a single line:

  ```rust
  //! Placeholder; populated in a later task of this plan.
  ```

  And create `crates/sunset-core/src/crypto/mod.rs` containing:

  ```rust
  //! Crypto primitives + envelope types. See
  //! `docs/superpowers/specs/2026-04-26-sunset-crypto-design.md`.

  pub mod aead;
  pub mod constants;
  pub mod envelope;
  pub mod room;
  ```

  Plus `crates/sunset-core/src/crypto/{constants,room,aead,envelope}.rs` each with `//! Placeholder; populated in a later task of this plan.`

- [ ] **Step 7:** Verify the crate compiles:

  ```
  nix develop --command cargo build -p sunset-core
  ```

  Expected: `Compiling sunset-core ...` then `Finished`. (Re-exports for items defined later are added incrementally; `lib.rs` ends with the full set of re-exports listed in the §"File structure" section.)

- [ ] **Step 8:** Commit:

  ```
  git add Cargo.toml crates/sunset-core/
  git commit -m "Scaffold sunset-core crate with error type + crypto module skeleton"
  ```

---

### Task 2: `Identity` and `IdentityKey` (Ed25519)

**Files:**
- Modify: `crates/sunset-core/src/identity.rs`

- [ ] **Step 1:** Replace the placeholder `identity.rs` with the production module:

  ```rust
  //! Ephemeral Ed25519 identities.
  //!
  //! `Identity` wraps a private signing key; `IdentityKey` wraps the matching
  //! public verifying key. Both round-trip losslessly through the byte form
  //! used by `sunset_store::VerifyingKey`.

  use bytes::Bytes;
  use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey as DalekVerifyingKey};
  use rand_core::CryptoRngCore;

  use sunset_store::VerifyingKey as StoreVerifyingKey;

  use crate::error::{Error, Result};

  /// A keypair that can sign messages on behalf of an ephemeral identity.
  #[derive(Clone)]
  pub struct Identity {
      signing: SigningKey,
  }

  impl std::fmt::Debug for Identity {
      fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
          // Never print the secret bytes.
          f.debug_struct("Identity")
              .field("public", &self.public())
              .finish()
      }
  }

  impl Identity {
      /// Generate a fresh identity from the supplied RNG.
      pub fn generate<R: CryptoRngCore + ?Sized>(rng: &mut R) -> Self {
          Self { signing: SigningKey::generate(rng) }
      }

      /// Reconstruct an identity from its 32-byte secret seed.
      pub fn from_secret_bytes(bytes: &[u8; 32]) -> Self {
          Self { signing: SigningKey::from_bytes(bytes) }
      }

      /// Export the 32-byte secret seed.
      pub fn secret_bytes(&self) -> [u8; 32] {
          self.signing.to_bytes()
      }

      /// Public half of this identity.
      pub fn public(&self) -> IdentityKey {
          IdentityKey { verifying: self.signing.verifying_key() }
      }

      /// Convenience: the public half encoded as a `sunset_store::VerifyingKey`.
      pub fn store_verifying_key(&self) -> StoreVerifyingKey {
          self.public().store_verifying_key()
      }

      /// Sign an arbitrary byte slice with this identity's secret key.
      pub fn sign(&self, msg: &[u8]) -> Signature {
          self.signing.sign(msg)
      }
  }

  /// The public side of an `Identity`.
  #[derive(Clone, Debug, PartialEq, Eq, Hash)]
  pub struct IdentityKey {
      verifying: DalekVerifyingKey,
  }

  impl IdentityKey {
      /// Parse a 32-byte Ed25519 verifying key.
      pub fn from_bytes(bytes: &[u8; 32]) -> Result<Self> {
          Ok(Self { verifying: DalekVerifyingKey::from_bytes(bytes)? })
      }

      /// Raw 32-byte encoding.
      pub fn as_bytes(&self) -> [u8; 32] {
          self.verifying.to_bytes()
      }

      /// Lossless conversion to the store's bytes-only form.
      pub fn store_verifying_key(&self) -> StoreVerifyingKey {
          StoreVerifyingKey::new(Bytes::copy_from_slice(&self.verifying.to_bytes()))
      }

      /// Inverse of `store_verifying_key`.
      pub fn from_store_verifying_key(vk: &StoreVerifyingKey) -> Result<Self> {
          let bytes: &[u8] = vk.as_bytes();
          let arr: [u8; 32] = bytes
              .try_into()
              .map_err(|_| Error::BadName(format!(
                  "verifying key must be 32 bytes, got {}",
                  bytes.len(),
              )))?;
          Self::from_bytes(&arr)
      }

      /// Verify a signature against this key.
      pub fn verify(&self, msg: &[u8], sig: &Signature) -> Result<()> {
          Ok(self.verifying.verify(msg, sig)?)
      }
  }

  #[cfg(test)]
  mod tests {
      use super::*;
      use rand_core::OsRng;

      fn fresh_identity() -> Identity {
          Identity::generate(&mut OsRng)
      }

      #[test]
      fn secret_bytes_roundtrip() {
          let id = fresh_identity();
          let bytes = id.secret_bytes();
          let id2 = Identity::from_secret_bytes(&bytes);
          assert_eq!(id.public(), id2.public());
          assert_eq!(id.secret_bytes(), id2.secret_bytes());
      }

      #[test]
      fn sign_verify_roundtrip_succeeds() {
          let id = fresh_identity();
          let msg = b"hello sunset";
          let sig = id.sign(msg);
          assert!(id.public().verify(msg, &sig).is_ok());
      }

      #[test]
      fn sign_verify_rejects_wrong_message() {
          let id = fresh_identity();
          let sig = id.sign(b"original");
          assert!(id.public().verify(b"tampered", &sig).is_err());
      }

      #[test]
      fn sign_verify_rejects_wrong_key() {
          let alice = fresh_identity();
          let bob = fresh_identity();
          let sig = alice.sign(b"msg");
          assert!(bob.public().verify(b"msg", &sig).is_err());
      }

      #[test]
      fn store_verifying_key_roundtrip() {
          let id = fresh_identity();
          let svk = id.store_verifying_key();
          assert_eq!(svk.as_bytes().len(), 32);
          let recovered = IdentityKey::from_store_verifying_key(&svk).unwrap();
          assert_eq!(recovered, id.public());
      }

      #[test]
      fn store_verifying_key_rejects_wrong_length() {
          let svk = StoreVerifyingKey::new(Bytes::from_static(b"not 32 bytes"));
          let err = IdentityKey::from_store_verifying_key(&svk).unwrap_err();
          assert!(matches!(err, Error::BadName(_)));
      }
  }
  ```

- [ ] **Step 2:** Run the new tests:

  ```
  nix develop --command cargo test -p sunset-core identity::tests
  ```

  Expected: 6 passed.

- [ ] **Step 3:** Commit:

  ```
  git add crates/sunset-core/src/identity.rs
  git commit -m "Add Ed25519 Identity + IdentityKey with store-vk conversion"
  ```

---

### Task 3: Canonical signing payload + frozen test vector (entry signatures)

**Files:**
- Modify: `crates/sunset-core/src/canonical.rs`

- [ ] **Step 1:** Replace the placeholder `canonical.rs`:

  ```rust
  //! Canonical signing payload for `SignedKvEntry`.
  //!
  //! The store-layer `SignatureVerifier` contract requires implementations to
  //! verify a signature over "the canonical encoding of the rest of the entry"
  //! (every field except `signature`). This module pins that encoding to
  //! `postcard::to_stdvec(&UnsignedEntryRef { ... })` with fields in the order
  //! they appear in `SignedKvEntry`.
  //!
  //! The frozen test vector at the bottom of this file is what keeps the wire
  //! format honest. If it ever fails, the canonical encoding has changed and
  //! every signature ever produced under the old encoding becomes invalid —
  //! treat that as a wire-format version bump, not a "fix the test" moment.

  use bytes::Bytes;
  use serde::Serialize;

  use sunset_store::{Hash, SignedKvEntry, VerifyingKey};

  /// The fields of `SignedKvEntry` that are covered by the signature, in the
  /// frozen canonical order.
  #[derive(Serialize)]
  struct UnsignedEntryRef<'a> {
      verifying_key: &'a VerifyingKey,
      name: &'a Bytes,
      value_hash: &'a Hash,
      priority: u64,
      expires_at: Option<u64>,
  }

  /// Build the canonical byte payload that an `Ed25519Verifier` (or any
  /// future verifier) signs and verifies over.
  pub fn signing_payload(entry: &SignedKvEntry) -> Vec<u8> {
      let unsigned = UnsignedEntryRef {
          verifying_key: &entry.verifying_key,
          name: &entry.name,
          value_hash: &entry.value_hash,
          priority: entry.priority,
          expires_at: entry.expires_at,
      };
      postcard::to_stdvec(&unsigned)
          .expect("postcard encoding of UnsignedEntryRef is infallible")
  }

  #[cfg(test)]
  mod tests {
      use super::*;

      fn sample_entry() -> SignedKvEntry {
          SignedKvEntry {
              verifying_key: VerifyingKey::new(Bytes::from_static(b"sample-vk-32-bytes-aaaaaaaaaaaaa")),
              name: Bytes::from_static(b"room/general/msg/abc"),
              value_hash: Hash::from_bytes([7u8; 32]),
              priority: 42,
              expires_at: Some(99),
              // signature is *not* included in the payload.
              signature: Bytes::from_static(b"ignored"),
          }
      }

      #[test]
      fn payload_excludes_signature_field() {
          let mut a = sample_entry();
          let mut b = sample_entry();
          b.signature = Bytes::from_static(b"completely different");
          assert_eq!(signing_payload(&a), signing_payload(&b));
          // Sanity: changing a covered field does change the payload.
          a.priority = 43;
          assert_ne!(signing_payload(&a), signing_payload(&b));
      }

      /// Frozen wire-format vector. If this hex changes, every existing
      /// signature in the wild becomes invalid — bump the wire-format version
      /// before updating the constant.
      #[test]
      fn payload_frozen_vector() {
          let entry = sample_entry();
          let payload = signing_payload(&entry);
          let digest = blake3::hash(&payload);
          assert_eq!(
              digest.to_hex().as_str(),
              "REPLACE_WITH_REAL_HASH_AFTER_FIRST_RUN",
              "If this fails the canonical signing encoding has drifted — DO NOT update this hex without bumping the wire-format version.",
          );
      }
  }
  ```

- [ ] **Step 2:** Run with the placeholder hex to capture the actual digest:

  ```
  nix develop --command cargo test -p sunset-core canonical::tests::payload_frozen_vector -- --nocapture
  ```

  Expected: FAIL revealing the actual hash.

- [ ] **Step 3:** Replace `REPLACE_WITH_REAL_HASH_AFTER_FIRST_RUN` with the captured 64-char hex string (paste exactly what the test produced — do not invent one).

- [ ] **Step 4:** Re-run:

  ```
  nix develop --command cargo test -p sunset-core canonical::tests
  ```

  Expected: 2 passed.

- [ ] **Step 5:** Commit:

  ```
  git add crates/sunset-core/src/canonical.rs
  git commit -m "Add canonical signing payload with frozen wire-format vector"
  ```

---

### Task 4: `Ed25519Verifier` plugged into `sunset_store::SignatureVerifier`

**Files:**
- Modify: `crates/sunset-core/src/verifier.rs`

- [ ] **Step 1:** Replace the placeholder `verifier.rs`:

  ```rust
  //! Ed25519 implementation of `sunset_store::SignatureVerifier`.

  use ed25519_dalek::{Signature, VerifyingKey as DalekVerifyingKey};

  use sunset_store::{Error as StoreError, Result as StoreResult, SignatureVerifier, SignedKvEntry};

  use crate::canonical::signing_payload;

  /// Stateless verifier for entries signed by Ed25519 keys.
  #[derive(Debug, Default, Clone, Copy)]
  pub struct Ed25519Verifier;

  impl SignatureVerifier for Ed25519Verifier {
      fn verify(&self, entry: &SignedKvEntry) -> StoreResult<()> {
          let vk_bytes: [u8; 32] = entry
              .verifying_key
              .as_bytes()
              .try_into()
              .map_err(|_| StoreError::SignatureInvalid)?;
          let vk = DalekVerifyingKey::from_bytes(&vk_bytes)
              .map_err(|_| StoreError::SignatureInvalid)?;

          let sig_bytes: &[u8] = &entry.signature;
          let sig_arr: &[u8; 64] = sig_bytes
              .try_into()
              .map_err(|_| StoreError::SignatureInvalid)?;
          let sig = Signature::from_bytes(sig_arr);

          let payload = signing_payload(entry);
          vk.verify_strict(&payload, &sig)
              .map_err(|_| StoreError::SignatureInvalid)
      }
  }

  #[cfg(test)]
  mod tests {
      use bytes::Bytes;
      use rand_core::OsRng;
      use sunset_store::{Hash, SignedKvEntry, VerifyingKey};

      use crate::identity::Identity;
      use crate::canonical::signing_payload;

      use super::*;

      fn signed_entry(id: &Identity) -> SignedKvEntry {
          let mut entry = SignedKvEntry {
              verifying_key: id.store_verifying_key(),
              name: Bytes::from_static(b"room/general/msg/00"),
              value_hash: Hash::from_bytes([1u8; 32]),
              priority: 1,
              expires_at: None,
              signature: Bytes::new(),
          };
          let sig = id.sign(&signing_payload(&entry));
          entry.signature = Bytes::copy_from_slice(&sig.to_bytes());
          entry
      }

      #[test]
      fn accepts_valid_signature() {
          let id = Identity::generate(&mut OsRng);
          let entry = signed_entry(&id);
          assert!(Ed25519Verifier.verify(&entry).is_ok());
      }

      #[test]
      fn rejects_tampered_payload() {
          let id = Identity::generate(&mut OsRng);
          let mut entry = signed_entry(&id);
          entry.priority += 1;
          assert!(Ed25519Verifier.verify(&entry).is_err());
      }

      #[test]
      fn rejects_wrong_signer() {
          let alice = Identity::generate(&mut OsRng);
          let bob = Identity::generate(&mut OsRng);
          let mut entry = signed_entry(&alice);
          entry.verifying_key = bob.store_verifying_key();
          assert!(Ed25519Verifier.verify(&entry).is_err());
      }

      #[test]
      fn rejects_malformed_verifying_key() {
          let id = Identity::generate(&mut OsRng);
          let mut entry = signed_entry(&id);
          entry.verifying_key = VerifyingKey::new(Bytes::from_static(b"too short"));
          assert!(Ed25519Verifier.verify(&entry).is_err());
      }

      #[test]
      fn rejects_malformed_signature() {
          let id = Identity::generate(&mut OsRng);
          let mut entry = signed_entry(&id);
          entry.signature = Bytes::from_static(b"too short");
          assert!(Ed25519Verifier.verify(&entry).is_err());
      }
  }
  ```

- [ ] **Step 2:** Run:

  ```
  nix develop --command cargo test -p sunset-core verifier::tests
  ```

  Expected: 5 passed.

- [ ] **Step 3:** Commit:

  ```
  git add crates/sunset-core/src/verifier.rs
  git commit -m "Add Ed25519Verifier implementing sunset_store::SignatureVerifier"
  ```

---

### Task 5: Crypto constants + frozen byte vectors

**Files:**
- Modify: `crates/sunset-core/src/crypto/constants.rs`

- [ ] **Step 1:** Replace the placeholder. Pin every domain-separation string, the Argon2id salt, and the two named parameter sets, with frozen byte literals:

  ```rust
  //! Cryptographic constants. Every literal here is part of the v1 wire
  //! format. Changing any of them invalidates every key, signature, and
  //! ciphertext ever produced under v1 — bump the wire-format version
  //! before touching them.

  use argon2::Params;

  /// 32-byte salt fed into Argon2id when deriving `K_room` from a room name.
  /// Right-padded with NUL to 32 bytes (the `argon2` crate accepts arbitrary
  /// salt bytes; we fix the length to keep the constant pinnable).
  pub const ROOM_KEY_SALT: &[u8; 32] = b"sunset-chat-v1-room\0\0\0\0\0\0\0\0\0\0\0\0\0";

  /// Domain-separation input for the blake3-keyed `room_fingerprint`.
  pub const FINGERPRINT_DOMAIN: &[u8] = b"sunset-chat-v1-fingerprint";

  /// HKDF `info` for deriving the open-room `K_epoch_0` from `K_room`.
  pub const EPOCH_0_DOMAIN: &[u8] = b"sunset-chat-v1-epoch-0";

  /// HKDF `info` *prefix* for deriving a per-message AEAD key from an epoch
  /// root. Per-message info is `MSG_KEY_DOMAIN || epoch_id_le_bytes || value_hash`.
  pub const MSG_KEY_DOMAIN: &[u8] = b"sunset-chat-v1-msg";

  /// AEAD additional-data prefix bound to every message ciphertext. The full
  /// AD is `MSG_AAD_DOMAIN || room_fingerprint || epoch_id_le_bytes || sender_id || sent_at_ms_le_bytes`.
  pub const MSG_AAD_DOMAIN: &[u8] = b"sunset-chat-v1-msg-aad";

  /// Production Argon2id parameters: m=19 MiB, t=2, p=1, 32-byte output.
  /// Matches OWASP 2023 baseline.
  ///
  /// `Params::new` returns `Result`, so this is a function rather than a const.
  pub fn production_params() -> Params {
      Params::new(19_456, 2, 1, Some(32))
          .expect("Argon2id production parameters are valid")
  }

  /// Test parameters tuned for sub-millisecond derivation: m=8 KiB, t=1, p=1.
  /// **Never use in production.** The frozen test vectors elsewhere in this
  /// crate are computed under these parameters; switching to production
  /// parameters changes every derived secret.
  pub fn test_fast_params() -> Params {
      Params::new(8, 1, 1, Some(32))
          .expect("Argon2id test-fast parameters are valid")
  }

  #[cfg(test)]
  mod tests {
      use super::*;

      // The byte literals below are part of the v1 wire format; failing
      // any of these means the constant has drifted.

      #[test]
      fn room_key_salt_is_32_bytes_with_label_prefix() {
          assert_eq!(ROOM_KEY_SALT.len(), 32);
          assert!(ROOM_KEY_SALT.starts_with(b"sunset-chat-v1-room"));
          // Trailing NUL pad.
          assert!(ROOM_KEY_SALT[19..].iter().all(|&b| b == 0));
      }

      #[test]
      fn fingerprint_domain_literal() {
          assert_eq!(FINGERPRINT_DOMAIN, b"sunset-chat-v1-fingerprint");
      }

      #[test]
      fn epoch_0_domain_literal() {
          assert_eq!(EPOCH_0_DOMAIN, b"sunset-chat-v1-epoch-0");
      }

      #[test]
      fn msg_key_domain_literal() {
          assert_eq!(MSG_KEY_DOMAIN, b"sunset-chat-v1-msg");
      }

      #[test]
      fn msg_aad_domain_literal() {
          assert_eq!(MSG_AAD_DOMAIN, b"sunset-chat-v1-msg-aad");
      }

      #[test]
      fn production_params_match_owasp_2023() {
          let p = production_params();
          assert_eq!(p.m_cost(), 19_456);
          assert_eq!(p.t_cost(), 2);
          assert_eq!(p.p_cost(), 1);
          assert_eq!(p.output_len(), Some(32));
      }

      #[test]
      fn test_fast_params_are_minimal() {
          let p = test_fast_params();
          assert_eq!(p.m_cost(), 8);
          assert_eq!(p.t_cost(), 1);
          assert_eq!(p.p_cost(), 1);
          assert_eq!(p.output_len(), Some(32));
      }
  }
  ```

- [ ] **Step 2:** Run:

  ```
  nix develop --command cargo test -p sunset-core crypto::constants::tests
  ```

  Expected: 7 passed.

- [ ] **Step 3:** Commit:

  ```
  git add crates/sunset-core/src/crypto/constants.rs
  git commit -m "Add crypto constants: domain-separation labels + Argon2 params"
  ```

---

### Task 6: `Room` — Argon2id `K_room`, blake3-keyed fingerprint, HKDF `K_epoch_0`

**Files:**
- Modify: `crates/sunset-core/src/crypto/room.rs`
- Modify: `crates/sunset-core/src/lib.rs` (add re-export)

- [ ] **Step 1:** Replace the placeholder `room.rs`:

  ```rust
  //! Room-derived secrets: `K_room` (Argon2id of room name), `room_fingerprint`
  //! (blake3-keyed hash), and `K_epoch_0` for open rooms (HKDF from `K_room`).

  use argon2::{Algorithm, Argon2, Params, Version};
  use hkdf::Hkdf;
  use sha2::Sha256;
  use zeroize::Zeroizing;

  use crate::crypto::constants::{
      EPOCH_0_DOMAIN, FINGERPRINT_DOMAIN, ROOM_KEY_SALT, production_params,
  };
  use crate::error::{Error, Result};

  /// 32-byte room identifier visible on the wire. Computed from `K_room` via
  /// blake3-keyed hashing — the room name itself is never on the wire.
  #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
  pub struct RoomFingerprint(pub [u8; 32]);

  impl RoomFingerprint {
      pub fn as_bytes(&self) -> &[u8; 32] {
          &self.0
      }

      pub fn to_hex(&self) -> String {
          hex::encode(self.0)
      }
  }

  /// All key material derived from a room name. Held entirely in
  /// `Zeroizing<[u8; 32]>` so process memory is wiped on drop.
  ///
  /// Open rooms in v1 use only `epoch_0_root` for message encryption.
  /// Invite-only rooms (Plan 8) will keep `epoch_0_root` randomly generated
  /// and distributed via key bundles.
  pub struct Room {
      fingerprint: RoomFingerprint,
      k_room: Zeroizing<[u8; 32]>,
      epoch_0_root: Zeroizing<[u8; 32]>,
  }

  impl std::fmt::Debug for Room {
      fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
          f.debug_struct("Room")
              .field("fingerprint", &self.fingerprint)
              .field("k_room", &"<redacted>")
              .field("epoch_0_root", &"<redacted>")
              .finish()
      }
  }

  impl Room {
      /// Open-room construction with **production** Argon2id parameters.
      /// Slow (~tens to hundreds of ms). Use `open_with_params` in tests.
      pub fn open(room_name: &str) -> Result<Self> {
          Self::open_with_params(room_name, &production_params())
      }

      /// Open-room construction with caller-supplied Argon2id parameters.
      /// The frozen test vectors below use `test_fast_params()`.
      pub fn open_with_params(room_name: &str, params: &Params) -> Result<Self> {
          // 1. K_room = Argon2id(room_name, ROOM_KEY_SALT, params).
          let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params.clone());
          let mut k_room = Zeroizing::new([0u8; 32]);
          argon2
              .hash_password_into(room_name.as_bytes(), ROOM_KEY_SALT, &mut *k_room)
              .map_err(|e| Error::Argon2(e.to_string()))?;

          // 2. room_fingerprint = blake3.keyed_hash(K_room, FINGERPRINT_DOMAIN).
          let fingerprint = RoomFingerprint(
              *blake3::keyed_hash(&*k_room, FINGERPRINT_DOMAIN).as_bytes(),
          );

          // 3. K_epoch_0 = HKDF-SHA256(K_room, info = EPOCH_0_DOMAIN, 32 bytes).
          let mut epoch_0_root = Zeroizing::new([0u8; 32]);
          let hkdf = Hkdf::<Sha256>::new(None, &*k_room);
          hkdf.expand(EPOCH_0_DOMAIN, &mut *epoch_0_root)
              .expect("HKDF-SHA256 expand of 32 bytes never errors");

          Ok(Self { fingerprint, k_room, epoch_0_root })
      }

      pub fn fingerprint(&self) -> RoomFingerprint {
          self.fingerprint
      }

      /// Layer-1 K_room. Used for control-plane entries (presence, membership ops).
      /// Plan 6 itself doesn't AEAD-encrypt anything with `K_room`; exposed for
      /// Plan 7 / Plan 8 callers and for tests.
      pub fn k_room(&self) -> &[u8; 32] {
          &self.k_room
      }

      /// Look up the root key for an epoch this `Room` knows about. In Plan 6's
      /// scope, only epoch 0 is known; higher epochs return `None`.
      pub fn epoch_root(&self, epoch_id: u64) -> Option<&[u8; 32]> {
          if epoch_id == 0 {
              Some(&self.epoch_0_root)
          } else {
              None
          }
      }
  }

  #[cfg(test)]
  mod tests {
      use super::*;
      use crate::crypto::constants::test_fast_params;

      #[test]
      fn two_opens_of_the_same_name_yield_the_same_secrets() {
          let a = Room::open_with_params("general", &test_fast_params()).unwrap();
          let b = Room::open_with_params("general", &test_fast_params()).unwrap();
          assert_eq!(a.fingerprint(), b.fingerprint());
          assert_eq!(a.k_room(), b.k_room());
          assert_eq!(a.epoch_root(0).unwrap(), b.epoch_root(0).unwrap());
      }

      #[test]
      fn different_names_yield_different_secrets() {
          let a = Room::open_with_params("general", &test_fast_params()).unwrap();
          let b = Room::open_with_params("random", &test_fast_params()).unwrap();
          assert_ne!(a.fingerprint(), b.fingerprint());
          assert_ne!(a.k_room(), b.k_room());
          assert_ne!(a.epoch_root(0).unwrap(), b.epoch_root(0).unwrap());
      }

      #[test]
      fn epoch_root_only_known_for_epoch_zero_in_v1() {
          let r = Room::open_with_params("general", &test_fast_params()).unwrap();
          assert!(r.epoch_root(0).is_some());
          assert!(r.epoch_root(1).is_none());
          assert!(r.epoch_root(u64::MAX).is_none());
      }

      /// Frozen wire-format vector for "general" under `test_fast_params()`.
      /// If any of these hashes change, the v1 chat wire format has drifted —
      /// bump the version before updating the constants.
      #[test]
      fn general_room_secrets_frozen_vector() {
          let r = Room::open_with_params("general", &test_fast_params()).unwrap();
          assert_eq!(
              hex::encode(r.k_room()),
              "REPLACE_K_ROOM_HEX",
              "If this fails, K_room derivation has drifted — DO NOT update without a wire-format bump.",
          );
          assert_eq!(
              r.fingerprint().to_hex(),
              "REPLACE_FINGERPRINT_HEX",
              "If this fails, room_fingerprint derivation has drifted — DO NOT update without a wire-format bump.",
          );
          assert_eq!(
              hex::encode(r.epoch_root(0).unwrap()),
              "REPLACE_EPOCH_0_HEX",
              "If this fails, K_epoch_0 derivation has drifted — DO NOT update without a wire-format bump.",
          );
      }
  }
  ```

- [ ] **Step 2:** Add `Room` and `RoomFingerprint` to `lib.rs`'s re-export block (after `pub use error::{Error, Result};`):

  ```rust
  pub use crypto::room::{Room, RoomFingerprint};
  ```

- [ ] **Step 3:** Run with placeholder hexes to capture real ones:

  ```
  nix develop --command cargo test -p sunset-core crypto::room::tests::general_room_secrets_frozen_vector -- --nocapture
  ```

  Expected: FAIL revealing one of the three hex values. Replace `REPLACE_K_ROOM_HEX`. Re-run; fill in `REPLACE_FINGERPRINT_HEX`. Re-run; fill in `REPLACE_EPOCH_0_HEX`. (You may also see all three on the first failure depending on assertion ordering — copy each in turn.)

- [ ] **Step 4:** Re-run:

  ```
  nix develop --command cargo test -p sunset-core crypto::room::tests
  ```

  Expected: 4 passed.

- [ ] **Step 5:** Commit:

  ```
  git add crates/sunset-core/src/crypto/room.rs crates/sunset-core/src/lib.rs
  git commit -m "Add Room: K_room (Argon2id), fingerprint (blake3), K_epoch_0 (HKDF)"
  ```

---

### Task 7: Per-message AEAD primitives — `derive_msg_key` + `aead_encrypt` / `aead_decrypt`

**Files:**
- Modify: `crates/sunset-core/src/crypto/aead.rs`

- [ ] **Step 1:** Replace the placeholder `aead.rs`:

  ```rust
  //! Per-message AEAD primitives.
  //!
  //! Per-message key:
  //!   K_msg = HKDF-SHA256(
  //!       ikm  = K_epoch_n,
  //!       salt = (none),
  //!       info = MSG_KEY_DOMAIN || epoch_id_le || value_hash,
  //!   ).expand(32 bytes)
  //!
  //! AEAD: XChaCha20-Poly1305 with a 24-byte random nonce.
  //!   ciphertext = AEAD(
  //!       key   = K_msg,
  //!       nonce = nonce,
  //!       ad    = MSG_AAD_DOMAIN || room_fingerprint || epoch_id_le || sender_id || sent_at_ms_le,
  //!       pt    = postcard(SignedMessage),
  //!   )

  use chacha20poly1305::aead::{Aead, AeadCore, KeyInit, OsRng, Payload};
  use chacha20poly1305::{Key, XChaCha20Poly1305, XNonce};
  use hkdf::Hkdf;
  use rand_core::CryptoRngCore;
  use sha2::Sha256;
  use zeroize::Zeroizing;

  use sunset_store::Hash;

  use crate::crypto::constants::{MSG_AAD_DOMAIN, MSG_KEY_DOMAIN};
  use crate::error::{Error, Result};
  use crate::identity::IdentityKey;

  /// Derive the per-message AEAD key from the epoch root.
  pub fn derive_msg_key(
      epoch_root: &[u8; 32],
      epoch_id: u64,
      value_hash: &Hash,
  ) -> Zeroizing<[u8; 32]> {
      let mut info = Vec::with_capacity(MSG_KEY_DOMAIN.len() + 8 + 32);
      info.extend_from_slice(MSG_KEY_DOMAIN);
      info.extend_from_slice(&epoch_id.to_le_bytes());
      info.extend_from_slice(value_hash.as_bytes());

      let hkdf = Hkdf::<Sha256>::new(None, epoch_root);
      let mut k = Zeroizing::new([0u8; 32]);
      hkdf.expand(&info, &mut *k)
          .expect("HKDF-SHA256 expand of 32 bytes never errors");
      k
  }

  /// Build the AEAD additional-data string. Binding sender + room + epoch +
  /// timestamp into the AD ensures any tamper of those fields fails decryption.
  pub fn build_msg_aad(
      room_fp: &[u8; 32],
      epoch_id: u64,
      sender: &IdentityKey,
      sent_at_ms: u64,
  ) -> Vec<u8> {
      let mut ad = Vec::with_capacity(MSG_AAD_DOMAIN.len() + 32 + 8 + 32 + 8);
      ad.extend_from_slice(MSG_AAD_DOMAIN);
      ad.extend_from_slice(room_fp);
      ad.extend_from_slice(&epoch_id.to_le_bytes());
      ad.extend_from_slice(&sender.as_bytes());
      ad.extend_from_slice(&sent_at_ms.to_le_bytes());
      ad
  }

  /// Generate a fresh 24-byte XChaCha20-Poly1305 nonce.
  pub fn fresh_nonce<R: CryptoRngCore + ?Sized>(rng: &mut R) -> [u8; 24] {
      // Use the rand_core RNG explicitly rather than the chacha20poly1305 OsRng
      // shortcut, so callers retain control over RNG selection (matches the
      // crate-wide convention from `Identity::generate`).
      let _ = OsRng; // silence unused-import warning if OsRng is brought in elsewhere
      let mut n = [0u8; 24];
      rng.fill_bytes(&mut n);
      n
  }

  /// AEAD-encrypt under the given key, nonce, and additional data.
  pub fn aead_encrypt(
      key: &[u8; 32],
      nonce: &[u8; 24],
      ad: &[u8],
      pt: &[u8],
  ) -> Vec<u8> {
      let aead = XChaCha20Poly1305::new(Key::from_slice(key));
      aead.encrypt(XNonce::from_slice(nonce), Payload { msg: pt, aad: ad })
          .expect("XChaCha20-Poly1305 encrypt is infallible for in-memory inputs")
  }

  /// AEAD-decrypt. Returns `Error::AeadAuthFailed` for any tag failure.
  pub fn aead_decrypt(
      key: &[u8; 32],
      nonce: &[u8; 24],
      ad: &[u8],
      ct: &[u8],
  ) -> Result<Vec<u8>> {
      let aead = XChaCha20Poly1305::new(Key::from_slice(key));
      aead.decrypt(XNonce::from_slice(nonce), Payload { msg: ct, aad: ad })
          .map_err(|_| Error::AeadAuthFailed)
  }

  /// `AeadCore` is re-exported so tests can confirm the nonce size at compile
  /// time without depending on the chacha20poly1305 crate directly.
  pub fn nonce_size() -> usize {
      <XChaCha20Poly1305 as AeadCore>::NonceSize::USIZE
  }

  #[cfg(test)]
  mod tests {
      use super::*;
      use rand_core::OsRng;
      use sunset_store::Hash;
      use bytes::Bytes;

      use crate::identity::Identity;

      fn sample_root() -> [u8; 32] { [42u8; 32] }
      fn sample_hash() -> Hash { Hash::from_bytes([7u8; 32]) }

      #[test]
      fn nonce_size_is_24_bytes() {
          assert_eq!(nonce_size(), 24);
      }

      #[test]
      fn derive_msg_key_is_deterministic() {
          let a = derive_msg_key(&sample_root(), 0, &sample_hash());
          let b = derive_msg_key(&sample_root(), 0, &sample_hash());
          assert_eq!(*a, *b);
      }

      #[test]
      fn derive_msg_key_separates_epochs() {
          let a = derive_msg_key(&sample_root(), 0, &sample_hash());
          let b = derive_msg_key(&sample_root(), 1, &sample_hash());
          assert_ne!(*a, *b);
      }

      #[test]
      fn derive_msg_key_separates_value_hashes() {
          let a = derive_msg_key(&sample_root(), 0, &Hash::from_bytes([7u8; 32]));
          let b = derive_msg_key(&sample_root(), 0, &Hash::from_bytes([8u8; 32]));
          assert_ne!(*a, *b);
      }

      #[test]
      fn aead_roundtrip_succeeds() {
          let key = [1u8; 32];
          let nonce = [2u8; 24];
          let ad = b"hello-ad";
          let pt = b"hello world";
          let ct = aead_encrypt(&key, &nonce, ad, pt);
          let recovered = aead_decrypt(&key, &nonce, ad, &ct).unwrap();
          assert_eq!(recovered, pt);
      }

      #[test]
      fn aead_rejects_wrong_key() {
          let nonce = [2u8; 24];
          let ad = b"x";
          let ct = aead_encrypt(&[1u8; 32], &nonce, ad, b"pt");
          assert!(matches!(
              aead_decrypt(&[9u8; 32], &nonce, ad, &ct),
              Err(Error::AeadAuthFailed),
          ));
      }

      #[test]
      fn aead_rejects_wrong_nonce() {
          let key = [1u8; 32];
          let ad = b"x";
          let ct = aead_encrypt(&key, &[2u8; 24], ad, b"pt");
          assert!(matches!(
              aead_decrypt(&key, &[3u8; 24], ad, &ct),
              Err(Error::AeadAuthFailed),
          ));
      }

      #[test]
      fn aead_rejects_wrong_ad() {
          let key = [1u8; 32];
          let nonce = [2u8; 24];
          let ct = aead_encrypt(&key, &nonce, b"original-ad", b"pt");
          assert!(matches!(
              aead_decrypt(&key, &nonce, b"different-ad", &ct),
              Err(Error::AeadAuthFailed),
          ));
      }

      #[test]
      fn aead_rejects_tampered_ciphertext() {
          let key = [1u8; 32];
          let nonce = [2u8; 24];
          let ad = b"x";
          let mut ct = aead_encrypt(&key, &nonce, ad, b"pt");
          ct[0] ^= 1;
          assert!(matches!(
              aead_decrypt(&key, &nonce, ad, &ct),
              Err(Error::AeadAuthFailed),
          ));
      }

      #[test]
      fn build_msg_aad_includes_all_components() {
          let id = Identity::generate(&mut OsRng);
          let ad = build_msg_aad(&[7u8; 32], 0, &id.public(), 1_700_000_000_000);
          assert!(ad.starts_with(MSG_AAD_DOMAIN));
          assert_eq!(ad.len(), MSG_AAD_DOMAIN.len() + 32 + 8 + 32 + 8);
      }

      /// Frozen vector: `derive_msg_key` for a fixed input triple.
      #[test]
      fn derive_msg_key_frozen_vector() {
          let k = derive_msg_key(&sample_root(), 7, &sample_hash());
          assert_eq!(
              hex::encode(*k),
              "REPLACE_DERIVE_MSG_KEY_HEX",
              "If this fails, the per-message HKDF derivation has drifted — DO NOT update without a wire-format bump.",
          );
          let _ = Bytes::new(); // keep `bytes` import live across cfg(test)
      }
  }
  ```

  Note the `aead.encrypt` call uses the `Aead` trait from `chacha20poly1305::aead`. The `OsRng` import is used purely so `aead::AeadCore` resolves; the `_ = OsRng;` line silences the unused-import lint if that resolution path doesn't trigger an OsRng use elsewhere — if rustc warns differently in your toolchain, simply remove the import + the line.

- [ ] **Step 2:** Run with placeholder hex to capture the real value:

  ```
  nix develop --command cargo test -p sunset-core crypto::aead::tests::derive_msg_key_frozen_vector -- --nocapture
  ```

  Expected: FAIL revealing the actual hex. Replace `REPLACE_DERIVE_MSG_KEY_HEX` with the captured 64-char string.

- [ ] **Step 3:** Re-run:

  ```
  nix develop --command cargo test -p sunset-core crypto::aead::tests
  ```

  Expected: 11 passed.

- [ ] **Step 4:** Commit:

  ```
  git add crates/sunset-core/src/crypto/aead.rs
  git commit -m "Add per-message AEAD: HKDF key derivation + XChaCha20-Poly1305"
  ```

---

### Task 8: `EncryptedMessage` + `SignedMessage` wire shapes

**Files:**
- Modify: `crates/sunset-core/src/crypto/envelope.rs`

- [ ] **Step 1:** Replace the placeholder `envelope.rs`:

  ```rust
  //! On-the-wire envelopes for an encrypted chat message.
  //!
  //! Wire layering (top is innermost — the AEAD plaintext):
  //!
  //!   SignedMessage   { inner_signature, sent_at_ms, body }
  //!         |  postcard
  //!         v
  //!   <plaintext bytes>
  //!         |  XChaCha20-Poly1305 with K_msg + AAD
  //!         v
  //!   EncryptedMessage { epoch_id, nonce, ciphertext }
  //!         |  postcard
  //!         v
  //!   ContentBlock.data
  //!
  //! The `inner_signature` covers the canonical `InnerSigPayload` (defined
  //! below) and is verified by recipients after AEAD-decrypt — this is the
  //! authentication property from the crypto spec's third non-negotiable.

  use bytes::Bytes;
  use serde::{Deserialize, Serialize};

  use crate::crypto::room::RoomFingerprint;

  /// Plaintext-inside-the-AEAD. The author's Ed25519 signature over
  /// `InnerSigPayload` is `inner_signature`.
  #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
  pub struct SignedMessage {
      pub inner_signature: [u8; 64],
      pub sent_at_ms: u64,
      pub body: String,
  }

  /// What the inner Ed25519 signature covers. Bound to room + epoch so a valid
  /// signature in one room/epoch cannot be replayed into another.
  #[derive(Serialize)]
  pub struct InnerSigPayload<'a> {
      pub room_fingerprint: &'a [u8; 32],
      pub epoch_id: u64,
      pub sent_at_ms: u64,
      pub body: &'a str,
  }

  pub fn inner_sig_payload_bytes(
      room_fp: &RoomFingerprint,
      epoch_id: u64,
      sent_at_ms: u64,
      body: &str,
  ) -> Vec<u8> {
      postcard::to_stdvec(&InnerSigPayload {
          room_fingerprint: room_fp.as_bytes(),
          epoch_id,
          sent_at_ms,
          body,
      })
      .expect("postcard encoding of InnerSigPayload is infallible for in-memory inputs")
  }

  /// What lives inside `ContentBlock.data` for a chat message.
  #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
  pub struct EncryptedMessage {
      pub epoch_id: u64,
      pub nonce: [u8; 24],
      pub ciphertext: Bytes,
  }

  impl EncryptedMessage {
      pub fn to_bytes(&self) -> Vec<u8> {
          postcard::to_stdvec(self)
              .expect("postcard encoding of EncryptedMessage is infallible")
      }

      pub fn from_bytes(bytes: &[u8]) -> Result<Self, postcard::Error> {
          postcard::from_bytes(bytes)
      }
  }

  #[cfg(test)]
  mod tests {
      use super::*;

      #[test]
      fn signed_message_postcard_roundtrip() {
          let m = SignedMessage {
              inner_signature: [9u8; 64],
              sent_at_ms: 1_700_000_000_000,
              body: "hello".into(),
          };
          let bytes = postcard::to_stdvec(&m).unwrap();
          let back: SignedMessage = postcard::from_bytes(&bytes).unwrap();
          assert_eq!(back, m);
      }

      #[test]
      fn encrypted_message_roundtrip() {
          let e = EncryptedMessage {
              epoch_id: 0,
              nonce: [3u8; 24],
              ciphertext: Bytes::from_static(b"opaque-ct"),
          };
          let bytes = e.to_bytes();
          let back = EncryptedMessage::from_bytes(&bytes).unwrap();
          assert_eq!(back, e);
      }

      #[test]
      fn inner_sig_payload_changes_with_each_field() {
          let fp = RoomFingerprint([1u8; 32]);
          let a = inner_sig_payload_bytes(&fp, 0, 100, "hi");
          let b = inner_sig_payload_bytes(&fp, 1, 100, "hi");           // epoch differs
          let c = inner_sig_payload_bytes(&fp, 0, 101, "hi");           // sent_at differs
          let d = inner_sig_payload_bytes(&fp, 0, 100, "hello");        // body differs
          let e = inner_sig_payload_bytes(&RoomFingerprint([2u8; 32]),
                                         0, 100, "hi");                 // room differs
          assert_ne!(a, b);
          assert_ne!(a, c);
          assert_ne!(a, d);
          assert_ne!(a, e);
      }

      /// Frozen wire-format vector for `EncryptedMessage`. Failing means the
      /// postcard encoding has drifted — bump the version before updating.
      #[test]
      fn encrypted_message_frozen_vector() {
          let e = EncryptedMessage {
              epoch_id: 0,
              nonce: [3u8; 24],
              ciphertext: Bytes::from_static(b"opaque-ct"),
          };
          let bytes = e.to_bytes();
          let digest = blake3::hash(&bytes);
          assert_eq!(
              digest.to_hex().as_str(),
              "REPLACE_ENCRYPTED_MESSAGE_HEX",
              "If this fails, the EncryptedMessage wire format has drifted — DO NOT update without a wire-format bump.",
          );
      }
  }
  ```

- [ ] **Step 2:** Run with placeholder hex to capture the real value:

  ```
  nix develop --command cargo test -p sunset-core crypto::envelope::tests::encrypted_message_frozen_vector -- --nocapture
  ```

  Expected: FAIL revealing the actual hex. Replace `REPLACE_ENCRYPTED_MESSAGE_HEX`.

- [ ] **Step 3:** Re-run:

  ```
  nix develop --command cargo test -p sunset-core crypto::envelope::tests
  ```

  Expected: 4 passed.

- [ ] **Step 4:** Commit:

  ```
  git add crates/sunset-core/src/crypto/envelope.rs
  git commit -m "Add EncryptedMessage + SignedMessage envelope types"
  ```

---

### Task 9: `compose_message` + `decode_message` (full encrypt/decrypt pipeline)

**Files:**
- Modify: `crates/sunset-core/src/message.rs`
- Modify: `crates/sunset-core/src/lib.rs` (re-exports)

- [ ] **Step 1:** Replace the placeholder `message.rs`:

  ```rust
  //! End-to-end chat message envelope: ties identity + room + AEAD + store
  //! together. The only file that simultaneously knows about `Identity`,
  //! `Room`, `Ed25519Verifier`, the AEAD primitives, and `SignedKvEntry`.

  use bytes::Bytes;
  use ed25519_dalek::Signature;
  use rand_core::CryptoRngCore;

  use sunset_store::{ContentBlock, Hash, SignedKvEntry};

  use crate::canonical::signing_payload;
  use crate::crypto::aead::{aead_decrypt, aead_encrypt, build_msg_aad, derive_msg_key, fresh_nonce};
  use crate::crypto::envelope::{
      EncryptedMessage, SignedMessage, inner_sig_payload_bytes,
  };
  use crate::crypto::room::{Room, RoomFingerprint};
  use crate::error::{Error, Result};
  use crate::identity::{Identity, IdentityKey};

  /// The pair of values `compose_message` produces: the signed KV entry
  /// (carries metadata + outer signature) and the content block (carries the
  /// encrypted message envelope). Insert both atomically with `Store::insert`.
  #[derive(Clone, Debug, PartialEq, Eq)]
  pub struct ComposedMessage {
      pub entry: SignedKvEntry,
      pub block: ContentBlock,
  }

  /// The structured form recovered from a `(SignedKvEntry, ContentBlock)` pair
  /// after AEAD-decrypt + inner-signature verification.
  #[derive(Clone, Debug, PartialEq, Eq)]
  pub struct DecodedMessage {
      pub author_key: IdentityKey,
      pub room_fingerprint: RoomFingerprint,
      pub epoch_id: u64,
      pub value_hash: Hash,
      pub sent_at_ms: u64,
      pub body: String,
  }

  /// Build the entry name for a message:
  ///   `<hex(room_fingerprint)>/msg/<hex(value_hash)>`
  fn message_name(room_fp: &RoomFingerprint, value_hash: &Hash) -> Bytes {
      Bytes::from(format!("{}/msg/{}", room_fp.to_hex(), value_hash.to_hex()))
  }

  /// Encrypt + sign a message into the on-the-wire pair.
  ///
  /// In v1, only `epoch_id == 0` (open-room K_epoch_0) is supported by `Room`.
  /// Higher epochs become available in Plan 7.
  pub fn compose_message<R: CryptoRngCore + ?Sized>(
      identity: &Identity,
      room: &Room,
      epoch_id: u64,
      sent_at_ms: u64,
      body: &str,
      rng: &mut R,
  ) -> Result<ComposedMessage> {
      let epoch_root = room
          .epoch_root(epoch_id)
          .ok_or(Error::EpochMismatch)?;
      let room_fp = room.fingerprint();

      // 1. Inner signature over (room_fp, epoch_id, sent_at_ms, body).
      let inner_payload = inner_sig_payload_bytes(&room_fp, epoch_id, sent_at_ms, body);
      let inner_sig = identity.sign(&inner_payload).to_bytes();

      // 2. AEAD-encrypt postcard(SignedMessage).
      let signed = SignedMessage {
          inner_signature: inner_sig,
          sent_at_ms,
          body: body.to_owned(),
      };
      let pt = postcard::to_stdvec(&signed)?;
      let nonce = fresh_nonce(rng);

      // 3. Build the EncryptedMessage envelope. We need `value_hash` to feed
      //    HKDF, and the value_hash depends on the EncryptedMessage bytes —
      //    so we derive the per-message key from a *placeholder* value_hash
      //    that's stable: blake3 of the AEAD plaintext. This binds the key to
      //    the message content while remaining deterministic before the outer
      //    envelope is finalized. (The outer `value_hash` stored in the entry
      //    is computed below from the final ContentBlock and is verified by
      //    `decode_message`.)
      let pt_hash: Hash = blake3::hash(&pt).into();
      let k_msg = derive_msg_key(epoch_root, epoch_id, &pt_hash);
      let aad = build_msg_aad(room_fp.as_bytes(), epoch_id, &identity.public(), sent_at_ms);
      let ciphertext = aead_encrypt(&*k_msg, &nonce, &aad, &pt);

      // 4. Wrap in EncryptedMessage and ContentBlock.
      let envelope = EncryptedMessage {
          epoch_id,
          nonce,
          ciphertext: Bytes::from(ciphertext),
      };
      let block = ContentBlock {
          data: Bytes::from(envelope.to_bytes()),
          // pt_hash is *also* embedded as a single reference — this is what
          // ties the outer ContentBlock.hash() to the inner per-message HKDF
          // input. Decoders re-derive it the same way.
          references: vec![pt_hash],
      };
      let value_hash = block.hash();

      // 5. Build + outer-sign the SignedKvEntry.
      let mut entry = SignedKvEntry {
          verifying_key: identity.store_verifying_key(),
          name: message_name(&room_fp, &value_hash),
          value_hash,
          priority: sent_at_ms,
          expires_at: None,
          signature: Bytes::new(),
      };
      let outer_sig = identity.sign(&signing_payload(&entry));
      entry.signature = Bytes::copy_from_slice(&outer_sig.to_bytes());

      Ok(ComposedMessage { entry, block })
  }

  /// Decrypt + verify a `(SignedKvEntry, ContentBlock)` pair.
  ///
  /// This performs all five steps of the crypto spec's §"Authentication
  /// invariant" except step 1 (which the store enforces on insert via
  /// `Ed25519Verifier`):
  ///
  /// 2. `value_hash` matches `block.hash()`.
  /// 3. `EncryptedMessage.epoch_id` corresponds to a known root.
  /// 4. AEAD decryption with `K_msg` + AAD succeeds.
  /// 5. Inner signature verifies under `entry.verifying_key`.
  pub fn decode_message(
      room: &Room,
      entry: &SignedKvEntry,
      block: &ContentBlock,
  ) -> Result<DecodedMessage> {
      // Step 2.
      if block.hash() != entry.value_hash {
          return Err(Error::BadValueHash);
      }

      // Step 3.
      let envelope = EncryptedMessage::from_bytes(&block.data)?;
      let epoch_root = room
          .epoch_root(envelope.epoch_id)
          .ok_or(Error::EpochMismatch)?;

      // The per-message HKDF input is the inner plaintext-hash, carried as
      // the single ContentBlock reference. (See compose step 3 + 4.)
      let pt_hash = *block.references.first().ok_or_else(|| Error::BadValueHash)?;

      // Sender identity (parsed from the entry's outer verifying key).
      let author_key = IdentityKey::from_store_verifying_key(&entry.verifying_key)?;

      // Step 4. AEAD-decrypt.
      let k_msg = derive_msg_key(epoch_root, envelope.epoch_id, &pt_hash);
      // For AAD, sent_at_ms must match what the sender used. The entry's
      // `priority` is set to sent_at_ms by `compose_message`, so we use that.
      let aad = build_msg_aad(
          room.fingerprint().as_bytes(),
          envelope.epoch_id,
          &author_key,
          entry.priority,
      );
      let pt = aead_decrypt(&*k_msg, &envelope.nonce, &aad, &envelope.ciphertext)?;

      // Sanity: re-derive pt_hash and confirm it matches the carried reference.
      let recomputed: Hash = blake3::hash(&pt).into();
      if recomputed != pt_hash {
          return Err(Error::BadValueHash);
      }

      // Decode SignedMessage.
      let signed: SignedMessage = postcard::from_bytes(&pt)?;

      // Cross-check sent_at_ms vs entry.priority (would otherwise be redundant
      // with AAD binding, but explicit makes the contract clear).
      if signed.sent_at_ms != entry.priority {
          return Err(Error::AeadAuthFailed);
      }

      // Verify entry name matches what compose_message would have produced.
      let expected_name = message_name(&room.fingerprint(), &entry.value_hash);
      if entry.name != expected_name {
          return Err(Error::BadName(format!(
              "name does not match `<hex_fp>/msg/<hex_value_hash>` for this room",
          )));
      }

      // Step 5. Inner signature.
      let inner_payload = inner_sig_payload_bytes(
          &room.fingerprint(),
          envelope.epoch_id,
          signed.sent_at_ms,
          &signed.body,
      );
      let inner_sig = Signature::from_bytes(&signed.inner_signature);
      author_key.verify(&inner_payload, &inner_sig)?;

      Ok(DecodedMessage {
          author_key,
          room_fingerprint: room.fingerprint(),
          epoch_id: envelope.epoch_id,
          value_hash: entry.value_hash,
          sent_at_ms: signed.sent_at_ms,
          body: signed.body,
      })
  }

  #[cfg(test)]
  mod tests {
      use super::*;
      use rand_core::OsRng;
      use sunset_store::SignatureVerifier;

      use crate::crypto::constants::test_fast_params;
      use crate::verifier::Ed25519Verifier;

      fn alice() -> Identity {
          Identity::generate(&mut OsRng)
      }
      fn general() -> Room {
          Room::open_with_params("general", &test_fast_params()).unwrap()
      }

      #[test]
      fn compose_then_decode_roundtrip() {
          let id = alice();
          let room = general();
          let composed = compose_message(&id, &room, 0, 1_700_000_000_000, "hi", &mut OsRng).unwrap();
          let decoded = decode_message(&room, &composed.entry, &composed.block).unwrap();
          assert_eq!(decoded.author_key, id.public());
          assert_eq!(decoded.room_fingerprint, room.fingerprint());
          assert_eq!(decoded.epoch_id, 0);
          assert_eq!(decoded.body, "hi");
          assert_eq!(decoded.sent_at_ms, 1_700_000_000_000);
      }

      #[test]
      fn composed_entry_passes_ed25519_verifier() {
          let id = alice();
          let room = general();
          let composed = compose_message(&id, &room, 0, 1, "x", &mut OsRng).unwrap();
          assert!(Ed25519Verifier.verify(&composed.entry).is_ok());
      }

      #[test]
      fn decode_rejects_wrong_room() {
          let id = alice();
          let alice_room = general();
          let other_room = Room::open_with_params("random", &test_fast_params()).unwrap();
          let composed = compose_message(&id, &alice_room, 0, 1, "x", &mut OsRng).unwrap();
          let err = decode_message(&other_room, &composed.entry, &composed.block).unwrap_err();
          // Wrong room → wrong fingerprint → name mismatch OR AAD mismatch.
          assert!(matches!(err, Error::BadName(_) | Error::AeadAuthFailed));
      }

      #[test]
      fn decode_rejects_block_hash_mismatch() {
          let id = alice();
          let room = general();
          let composed = compose_message(&id, &room, 0, 1, "x", &mut OsRng).unwrap();
          let mut bad_block = composed.block.clone();
          bad_block.data = Bytes::from_static(b"junk");
          let err = decode_message(&room, &composed.entry, &bad_block).unwrap_err();
          assert!(matches!(err, Error::BadValueHash));
      }

      #[test]
      fn decode_rejects_tampered_ciphertext() {
          let id = alice();
          let room = general();
          let composed = compose_message(&id, &room, 0, 1, "x", &mut OsRng).unwrap();
          // Re-derive with a flipped byte inside the EncryptedMessage ct.
          let mut envelope = EncryptedMessage::from_bytes(&composed.block.data).unwrap();
          let mut ct = envelope.ciphertext.to_vec();
          ct[0] ^= 1;
          envelope.ciphertext = Bytes::from(ct);
          let new_block = ContentBlock {
              data: Bytes::from(envelope.to_bytes()),
              references: composed.block.references.clone(),
          };
          // The entry's value_hash no longer matches new_block.hash() — this
          // is caught by step 2 before decryption is even attempted.
          let err = decode_message(&room, &composed.entry, &new_block).unwrap_err();
          assert!(matches!(err, Error::BadValueHash));
      }

      #[test]
      fn decode_rejects_forged_inner_signature() {
          let alice = alice();
          let mallory = Identity::generate(&mut OsRng);
          let room = general();

          // alice composes legitimately
          let composed = compose_message(&alice, &room, 0, 1, "real", &mut OsRng).unwrap();

          // mallory tries to construct a message that *claims* to be from
          // alice but signs the inner payload with mallory's key. The outer
          // signature is also mallory's, so we have to set the outer
          // verifying_key to mallory (otherwise outer Ed25519Verifier rejects).
          // The inner_payload signing key is what matters for *authentication*;
          // since the outer vk is mallory's, decode_message verifies the inner
          // sig against mallory — and mallory CAN forge for themselves. The
          // genuine attack is: leave outer vk = alice, sign inner with mallory.
          // That fails outer Ed25519Verifier (sig over canonical entry doesn't
          // match alice). Confirm this end-to-end:

          let mut forged = composed.clone();
          // Replace the inner payload's signature with mallory's signature.
          let env = EncryptedMessage::from_bytes(&forged.block.data).unwrap();
          let mut signed: SignedMessage = {
              // We can't re-decrypt without the key, so construct a brand-new
              // SignedMessage with mallory's inner sig, then re-encrypt it
              // under the same key derivation. The outer vk stays alice's,
              // and Ed25519Verifier (in step 1, by the store) catches the
              // outer-sig mismatch — but step 1 happens at insert time, not in
              // decode_message. For *decode_message*, the inner sig must verify
              // under the OUTER verifying_key. So if outer says alice and the
              // inner is mallory's, decode_message returns Signature error.

              // Simulate by faking decryption: re-derive plaintext, swap the
              // signature to mallory's, re-encrypt with the same K_msg + AAD
              // (same key inputs), and rebuild the block.
              let pt_hash = *forged.block.references.first().unwrap();
              let k_msg = derive_msg_key(room.epoch_root(0).unwrap(), 0, &pt_hash);
              let aad = build_msg_aad(room.fingerprint().as_bytes(), 0, &alice.public(), 1);
              let pt = aead_decrypt(&*k_msg, &env.nonce, &aad, &env.ciphertext).unwrap();
              postcard::from_bytes(&pt).unwrap()
          };
          // Forge inner signature with mallory.
          let mallory_sig = mallory.sign(&inner_sig_payload_bytes(
              &room.fingerprint(),
              0,
              signed.sent_at_ms,
              &signed.body,
          ));
          signed.inner_signature = mallory_sig.to_bytes();

          // Re-encrypt + re-build the block with the new pt_hash.
          let pt_new = postcard::to_stdvec(&signed).unwrap();
          let pt_hash_new: Hash = blake3::hash(&pt_new).into();
          let k_msg_new = derive_msg_key(room.epoch_root(0).unwrap(), 0, &pt_hash_new);
          let aad = build_msg_aad(room.fingerprint().as_bytes(), 0, &alice.public(), 1);
          let nonce = env.nonce;
          let ct_new = aead_encrypt(&*k_msg_new, &nonce, &aad, &pt_new);
          let env_new = EncryptedMessage {
              epoch_id: 0,
              nonce,
              ciphertext: Bytes::from(ct_new),
          };
          let block_new = ContentBlock {
              data: Bytes::from(env_new.to_bytes()),
              references: vec![pt_hash_new],
          };

          // Recompute entry.value_hash to keep step 2 from short-circuiting.
          forged.entry.value_hash = block_new.hash();
          forged.entry.name = message_name(&room.fingerprint(), &forged.entry.value_hash);
          forged.block = block_new;

          // decode_message should now reach step 5 and reject the inner sig
          // (signed by mallory, verified against alice's outer vk).
          let err = decode_message(&room, &forged.entry, &forged.block).unwrap_err();
          assert!(matches!(err, Error::Signature(_)));
      }

      #[test]
      fn decode_rejects_unknown_epoch() {
          let id = alice();
          let room = general();
          let mut composed = compose_message(&id, &room, 0, 1, "x", &mut OsRng).unwrap();
          // Mutate the inner envelope to claim epoch 99.
          let mut env = EncryptedMessage::from_bytes(&composed.block.data).unwrap();
          env.epoch_id = 99;
          composed.block = ContentBlock {
              data: Bytes::from(env.to_bytes()),
              references: composed.block.references,
          };
          composed.entry.value_hash = composed.block.hash();
          composed.entry.name = message_name(&room.fingerprint(), &composed.entry.value_hash);

          let err = decode_message(&room, &composed.entry, &composed.block).unwrap_err();
          assert!(matches!(err, Error::EpochMismatch));
      }
  }
  ```

- [ ] **Step 2:** Add the new public types to `lib.rs` (after `pub use crypto::room::{Room, RoomFingerprint};`):

  ```rust
  pub use crypto::envelope::{EncryptedMessage, SignedMessage};
  pub use identity::{Identity, IdentityKey};
  pub use message::{ComposedMessage, DecodedMessage, compose_message, decode_message};
  pub use verifier::Ed25519Verifier;
  ```

  (Insert in alphabetical order; `room_messages_filter` comes in Task 10.)

- [ ] **Step 3:** Build to confirm the re-exports resolve:

  ```
  nix develop --command cargo build -p sunset-core
  ```

  Expected: `Finished`.

- [ ] **Step 4:** Run the message tests:

  ```
  nix develop --command cargo test -p sunset-core message::tests
  ```

  Expected: 7 passed.

- [ ] **Step 5:** Commit:

  ```
  git add crates/sunset-core/src/message.rs crates/sunset-core/src/lib.rs
  git commit -m "Add compose_message + decode_message: full encrypt/sign/decrypt pipeline"
  ```

---

### Task 10: `room_messages_filter` helper

**Files:**
- Modify: `crates/sunset-core/src/filters.rs`
- Modify: `crates/sunset-core/src/lib.rs`

- [ ] **Step 1:** Replace the placeholder `filters.rs`:

  ```rust
  //! Sync interest-set helpers.

  use bytes::Bytes;

  use sunset_store::Filter;

  use crate::crypto::room::Room;

  /// All messages currently in (or arriving in) the given room.
  ///
  /// Pairs with the name format chosen by `compose_message`:
  ///   `<hex(room_fingerprint)>/msg/<hex(value_hash)>`.
  pub fn room_messages_filter(room: &Room) -> Filter {
      Filter::NamePrefix(Bytes::from(format!("{}/msg/", room.fingerprint().to_hex())))
  }

  #[cfg(test)]
  mod tests {
      use rand_core::OsRng;

      use sunset_store::VerifyingKey;

      use crate::crypto::constants::test_fast_params;
      use crate::identity::Identity;
      use crate::message::compose_message;

      use super::*;

      fn general() -> Room {
          Room::open_with_params("general", &test_fast_params()).unwrap()
      }

      #[test]
      fn matches_a_composed_message_in_the_same_room() {
          let id = Identity::generate(&mut OsRng);
          let room = general();
          let composed = compose_message(&id, &room, 0, 1, "x", &mut OsRng).unwrap();

          let filter = room_messages_filter(&room);
          assert!(filter.matches(&composed.entry.verifying_key, &composed.entry.name));
      }

      #[test]
      fn rejects_a_message_in_a_different_room() {
          let id = Identity::generate(&mut OsRng);
          let alice_room = general();
          let other_room = Room::open_with_params("other", &test_fast_params()).unwrap();
          let composed = compose_message(&id, &alice_room, 0, 1, "x", &mut OsRng).unwrap();

          let filter = room_messages_filter(&other_room);
          assert!(!filter.matches(&composed.entry.verifying_key, &composed.entry.name));
      }

      #[test]
      fn rejects_unrelated_namespaces() {
          let room = general();
          let filter = room_messages_filter(&room);
          let vk = VerifyingKey::new(Bytes::from_static(b"anyone"));
          assert!(!filter.matches(&vk, b"presence/anything"));
      }
  }
  ```

- [ ] **Step 2:** Add `room_messages_filter` to the `pub use` block in `lib.rs` (alphabetical order — between `error` and `identity`):

  ```rust
  pub use filters::room_messages_filter;
  ```

- [ ] **Step 3:** Run the filter tests:

  ```
  nix develop --command cargo test -p sunset-core filters::tests
  ```

  Expected: 3 passed.

- [ ] **Step 4:** Commit:

  ```
  git add crates/sunset-core/src/filters.rs crates/sunset-core/src/lib.rs
  git commit -m "Add room_messages_filter sync interest helper"
  ```

---

### Task 11: Two-peer integration test (alice encrypts → bob decrypts via sunset-sync)

**Files:**
- Create: `crates/sunset-core/tests/two_peer_message.rs`

- [ ] **Step 1:** Write the integration test. Both peers open the same room name, alice composes a real encrypted+signed message, bob receives the entry + content block via sunset-sync, decodes, and recovers alice's identity + body:

  ```rust
  //! End-to-end: alice composes an encrypted+signed chat message in an open
  //! room; bob (who opened the same room name) receives the entry and the
  //! content block via sunset-sync, then `decode_message` reconstructs the
  //! exact author key + body on the receiving peer.
  //!
  //! Demonstrates the full crypto spec authentication invariant traversed in
  //! anger: outer signature on insert (Ed25519Verifier), block-hash check,
  //! AEAD decryption with the shared K_epoch_0, and inner-signature verify.

  use std::rc::Rc;
  use std::sync::Arc;
  use std::time::Duration;

  use rand_core::OsRng;

  use sunset_core::{
      ComposedMessage, Ed25519Verifier, Identity, Room, compose_message, decode_message,
      room_messages_filter,
  };
  use sunset_core::crypto::constants::test_fast_params;
  use sunset_store::{ContentBlock, Hash, Store as _};
  use sunset_store_memory::MemoryStore;
  use sunset_sync::test_transport::TestNetwork;
  use sunset_sync::{PeerAddr, PeerId, SyncConfig, SyncEngine};

  #[tokio::test(flavor = "current_thread")]
  async fn alice_encrypts_bob_decrypts() {
      let local = tokio::task::LocalSet::new();
      local
          .run_until(async {
              // ---- identities + rooms ----
              let alice = Identity::generate(&mut OsRng);
              let bob = Identity::generate(&mut OsRng);
              let alice_room = Room::open_with_params("general", &test_fast_params()).unwrap();
              let bob_room = Room::open_with_params("general", &test_fast_params()).unwrap();
              assert_eq!(alice_room.fingerprint(), bob_room.fingerprint());

              // ---- per-peer stores, both verifying with Ed25519 ----
              let alice_store = Arc::new(MemoryStore::new(Arc::new(Ed25519Verifier)));
              let bob_store = Arc::new(MemoryStore::new(Arc::new(Ed25519Verifier)));

              // ---- transport + engines ----
              let net = TestNetwork::new();
              let alice_addr = PeerAddr::new("alice");
              let bob_addr = PeerAddr::new("bob");
              let alice_peer = PeerId(alice.store_verifying_key());
              let bob_peer = PeerId(bob.store_verifying_key());

              let alice_transport = net.transport(alice_peer.clone(), alice_addr.clone());
              let bob_transport = net.transport(bob_peer.clone(), bob_addr.clone());

              let alice_engine = Rc::new(SyncEngine::new(
                  alice_store.clone(),
                  alice_transport,
                  SyncConfig::default(),
                  alice_peer.clone(),
              ));
              let bob_engine = Rc::new(SyncEngine::new(
                  bob_store.clone(),
                  bob_transport,
                  SyncConfig::default(),
                  bob_peer.clone(),
              ));

              let alice_run = tokio::task::spawn_local({
                  let e = alice_engine.clone();
                  async move { e.run().await }
              });
              let bob_run = tokio::task::spawn_local({
                  let e = bob_engine.clone();
                  async move { e.run().await }
              });

              // ---- bob declares interest in #general ----
              bob_engine
                  .publish_subscription(
                      room_messages_filter(&bob_room),
                      Duration::from_secs(60),
                  )
                  .await
                  .unwrap();

              // ---- alice connects to bob ----
              alice_engine.add_peer(bob_addr).await.unwrap();

              let registered = wait_for(
                  Duration::from_secs(2),
                  Duration::from_millis(20),
                  || async {
                      alice_engine
                          .knows_peer_subscription(&bob.store_verifying_key())
                          .await
                  },
              )
              .await;
              assert!(registered, "alice did not learn bob's subscription");

              // ---- alice composes + inserts a real encrypted+signed message ----
              let body = "hello bob, this is encrypted";
              let sent_at = 1_700_000_000_000u64;
              let ComposedMessage { entry, block } =
                  compose_message(&alice, &alice_room, 0, sent_at, body, &mut OsRng).unwrap();
              let expected_hash: Hash = block.hash();

              alice_store
                  .insert(entry.clone(), Some(block.clone()))
                  .await
                  .expect("alice's own store accepts her signed entry");

              // ---- wait for bob's store to have both the entry and the block ----
              let bob_has_entry = wait_for(
                  Duration::from_secs(2),
                  Duration::from_millis(20),
                  || async {
                      bob_store
                          .get_entry(&alice.store_verifying_key(), &entry.name)
                          .await
                          .unwrap()
                          .is_some()
                  },
              )
              .await;
              assert!(bob_has_entry, "bob did not receive alice's entry");

              let bob_has_block = wait_for(
                  Duration::from_secs(2),
                  Duration::from_millis(20),
                  || async {
                      bob_store.get_content(&expected_hash).await.unwrap().is_some()
                  },
              )
              .await;
              assert!(bob_has_block, "bob did not receive alice's content block");

              // ---- bob decodes ----
              let bob_entry = bob_store
                  .get_entry(&alice.store_verifying_key(), &entry.name)
                  .await
                  .unwrap()
                  .unwrap();
              let bob_block: ContentBlock = bob_store
                  .get_content(&expected_hash)
                  .await
                  .unwrap()
                  .unwrap();

              let decoded = decode_message(&bob_room, &bob_entry, &bob_block).unwrap();
              assert_eq!(decoded.author_key, alice.public());
              assert_eq!(decoded.room_fingerprint, bob_room.fingerprint());
              assert_eq!(decoded.epoch_id, 0);
              assert_eq!(decoded.body, body);
              assert_eq!(decoded.sent_at_ms, sent_at);

              // ---- a third party who never joined cannot decrypt ----
              let charlie_room =
                  Room::open_with_params("not-the-right-name", &test_fast_params()).unwrap();
              let err = decode_message(&charlie_room, &bob_entry, &bob_block).unwrap_err();
              assert!(matches!(
                  err,
                  sunset_core::Error::BadName(_) | sunset_core::Error::AeadAuthFailed,
              ));

              alice_run.abort();
              bob_run.abort();
          })
          .await;
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

- [ ] **Step 2:** Run the integration test:

  ```
  nix develop --command cargo test -p sunset-core --test two_peer_message --all-features -- --nocapture
  ```

  Expected: 1 passed.

  If it hangs or times out, the most likely cause is `sunset-sync`'s `test-helpers` feature not being enabled — confirm `Cargo.toml`'s `[dev-dependencies]` line for `sunset-sync` has `features = ["test-helpers"]`.

- [ ] **Step 3:** Commit:

  ```
  git add crates/sunset-core/tests/two_peer_message.rs
  git commit -m "Add two-peer integration test: encrypted + signed message end-to-end"
  ```

---

### Task 12: Final pass — fmt, clippy, full test, wasm-build

- [ ] **Step 1:** Format check across the workspace:

  ```
  nix develop --command cargo fmt --all --check
  ```

  Expected: clean. If anything wants reformatting, run `cargo fmt --all` and stage the changes.

- [ ] **Step 2:** Clippy across the workspace with the project's lint gate:

  ```
  nix develop --command cargo clippy --workspace --all-features --all-targets -- -D warnings
  ```

  Expected: no warnings. Fix anything reported — typical hits at this stage are needless borrows, doc-comment whitespace, and `must_use` on pure constructors.

- [ ] **Step 3:** Run the full workspace test suite to confirm nothing else regressed:

  ```
  nix develop --command cargo test --workspace --all-features
  ```

  Expected: all tests pass.

- [ ] **Step 4:** Confirm sunset-core compiles to `wasm32-unknown-unknown` (per CLAUDE.md "WASM compatibility constraints"):

  ```
  nix develop --command cargo build -p sunset-core --target wasm32-unknown-unknown --lib
  ```

  Expected: `Finished`. (`--lib` excludes dev-deps so test-only RNG features don't pollute the wasm build.)

- [ ] **Step 5:** If any cleanup commits were needed in Steps 1–2, commit:

  ```
  git add -u
  git commit -m "Final fmt + clippy pass"
  ```

---

## Verification (end-state acceptance)

After all 12 tasks land:

- `nix develop --command cargo test --workspace --all-features` — green, including the new `crates/sunset-core/tests/two_peer_message.rs`.
- `nix develop --command cargo clippy --workspace --all-features --all-targets -- -D warnings` — clean.
- `nix develop --command cargo fmt --all --check` — clean.
- `nix develop --command cargo build -p sunset-core --target wasm32-unknown-unknown --lib` — succeeds.
- `git log --oneline master..HEAD` — roughly 12 task-by-task commits, each named for the slice it adds.
- The crypto spec's frozen-vector discipline is enforced by tests in **six places**: `canonical::tests::payload_frozen_vector`, `crypto::room::tests::general_room_secrets_frozen_vector` (three values), `crypto::aead::tests::derive_msg_key_frozen_vector`, `crypto::envelope::tests::encrypted_message_frozen_vector`, and the literal-bytes assertions in `crypto::constants::tests`.
- The integration test demonstrates the full chain: `Identity::generate` → `Room::open_with_params` → `compose_message` (Ed25519 inner sig + XChaCha20-Poly1305 AEAD + Ed25519 outer sig) → propagates across `sunset-sync`'s `TestNetwork` → `decode_message` (AEAD-decrypt + inner sig verify) reconstructs the exact author key + body. A third party with the *wrong* room name cannot decrypt.

---

## What this unlocks (informational; actual plans live in plans/)

After Plan 6, the crypto spec's deferred items become independently plannable:

- **Plan 7 — epoch rotation and key bundles.** `EpochKeyStore` trait with the explicit PFS wipe contract; `EpochKeyBundle` wire shape; X25519 + HKDF wrap/unwrap; the rotator code path; presence entries with ephemeral pubkeys; integration test where alice rotates and bob (who participated) reads epoch-1 messages while charlie (who has the room name but did not participate) cannot.
- **Plan 8 — membership ops.** Signed `member-add` / `member-remove`; member-set reconstruction from the op chain; admin authorization for invite-only rooms.
- **Plan 9 — sender-identity hiding** (per-room ephemeral signing keys with "endorsed by identity X" in the ciphertext).
- **Plan 10 — sub-epoch ratcheting** (per-message PFS within an epoch; sender-key-style chains).
- **Plan 11 — hybrid PQC** for both signing (ML-DSA + Ed25519) and KEM (ML-KEM + X25519).
- **Plan 12 — voice channel crypto** (separate session key from a Noise handshake at call setup).

Each goes through its own writing-plans cycle.
