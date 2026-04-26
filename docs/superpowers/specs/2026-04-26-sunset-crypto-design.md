# sunset.chat — Crypto subsystem design (v1)

- **Date:** 2026-04-26
- **Status:** Draft (subsystem-level)
- **Scope:** End-to-end encryption, perfect forward secrecy, and per-message authentication for sunset.chat. This spec covers the wire envelope, key hierarchy, key rotation rules, and the trait surface the rest of the codebase relies on. It deliberately does **not** specify SQL schemas, file layouts, or wire framing for `sunset-sync` — those live in their own subsystem specs.
- **Supersedes (in part):** the brief §"Wire envelope and encryption" sketch in the architecture spec (`2026-04-25-sunset-chat-architecture-design.md`). See §"Architecture-spec amendment" below.

## Non-negotiable goals

The three properties this design must satisfy from day 1, captured directly from the requirements brainstorm:

1. **Full end-to-end encryption** of message bodies. No party that lacks the room key material — neither relays, nor passive network observers, nor anyone who has the room name but never joined the room — can read message content.
2. **Perfect forward secrecy** for message bodies. If a peer's long-term identity key is later compromised, past message bodies cannot be decrypted from that compromise alone. PFS is bounded at *epoch* granularity for v1 (see §"PFS scope and v1 limits").
3. **Authentication** of message senders. A room member cannot forge a message attributed to another member. Every message body carries a per-message signature that binds it to the sender's identity, the room, and the epoch.

## Non-goals (v1)

- **Sender anonymity inside a room.** A `SignedKvEntry`'s `verifying_key` field is plaintext on the wire — anyone observing the store learns "identity X published into namespace Y." Cross-room linkability of an identity is therefore visible. Hiding this requires per-room ephemeral signing keys with "endorsed by identity X" carried in the ciphertext; deferred to a follow-up subsystem spec.
- **Timing-analysis resistance.** Message size and timing are unmodified. No cover traffic, no batching.
- **Sub-epoch PFS** (per-message ratcheting like Signal's Double Ratchet). v1 is per-epoch root key; if a peer's device is compromised mid-epoch, every message from the start of that epoch is exposed. The wire envelope leaves room for an in-epoch ratchet to be added later without a wire-format break.
- **Post-compromise security.** v1 does not target healing after a member compromise beyond the next epoch rotation. Real PCS is an MLS-shaped property; out of scope.
- **Hybrid PQC.** v1 uses pure classical primitives (X25519, Ed25519, ChaCha20-Poly1305). The architecture spec calls for hybrid PQC at the *handshake* layer eventually; introducing it requires bumping the wire-format version and adding a KEM beside X25519. Tracked but deferred.
- **Open-room admission control.** Open rooms accept anyone who knows the room name. Spam / abuse mitigation is a per-room admin concern, not a crypto concern.
- **Multi-device identity.** v1 treats one identity key = one device. Multi-device is an identity-subsystem concern.

## Threat model

Aligned with the architecture spec §"Threat model summary" and tightened where v1 makes a stronger claim:

| Adversary | What they see | What they cannot do |
|---|---|---|
| Passive network observer | Relay traffic. Encrypted CRDT events. Connection metadata (peers + relays + timing + size). | Read message bodies. Identify which room any event belongs to without the room name (room fingerprint hides this). |
| Eavesdropper holding the room name (open room, present-day) | Same as above, *plus* the ability to derive `K_room` and decrypt control-plane entries (presence, join requests). For epoch 0 specifically, also message bodies. | Decrypt epoch ≥ 1 message bodies (PFS). Forge messages (no identity key). |
| Eavesdropper acquiring the room name *later* | Same as above for any retained ciphertext. | Decrypt past message bodies if the room ever rotated past epoch 0 — the prior epoch keys are gone. **This is the property (A) decision from the brainstorm.** |
| Relay (correct or malicious) | Same as a passive observer. May drop or delay events. | Forge or modify events (signatures fail). Decrypt anything beyond what an eavesdropper with the room name can decrypt. |
| Compromised peer device today | Everything that peer ever held: their identity key, the current epoch root key, every past epoch root key that hasn't been wiped. | (See §"Key wipe contract" — past epoch keys MUST be wiped on rotation, so a compromise today only exposes back to the current epoch boundary.) |
| Compromised peer device replaying *to themselves* later | Same as above. | Re-derive past epoch root keys not stored locally. |

## Key hierarchy

Three layers, each with a distinct purpose. Each layer's key is computable only from inputs available to the parties who are supposed to read it.

### Layer 1: `K_room` — fixed name-derived key (control plane)

Used **only** for control-plane entries. Never for message bodies.

```
K_room          = Argon2id(
                      password = room_name,
                      salt     = "sunset-chat-v1-room",  // 32-byte literal, defined below
                      params   = OWASP_2023,             // m=19MiB, t=2, p=1
                      out_len  = 32,
                  )
room_fingerprint = blake3.keyed_hash(key = K_room, input = b"sunset-chat-v1-fingerprint")
                   .truncate_to(32 bytes)
```

`room_fingerprint` is the public identifier used as the prefix of every `SignedKvEntry.name` for that room. An eavesdropper without the room name cannot derive the fingerprint and so cannot link an event to a specific room.

`K_room` is used to AEAD-encrypt:

- **Presence entries** — each peer's per-epoch ephemeral X25519 pubkey. Members refresh these periodically while online.
- **Join requests** — a candidate joiner's offer to participate, carrying their ephemeral pubkey.
- **Membership ops** in invite-only rooms — `member-add` / `member-remove`. Their plaintext content is the affected member's identity key + role; the signature is by the room admin's identity key.

`K_room` is **not** used to encrypt:

- Message bodies. (Layer 2 owns those.)
- Key bundles for epoch ≥ 1. (Each recipient gets a per-recipient ciphertext encrypted to *their* ephemeral pubkey, not to `K_room`.)

The salt `"sunset-chat-v1-room"` is a fixed 32-byte string with the literal bytes:

```
73 75 6e 73 65 74 2d 63 68 61 74 2d 76 31 2d 72   "sunset-chat-v1-r"
6f 6f 6d 00 00 00 00 00 00 00 00 00 00 00 00 00   "oom" + 13 NULs
```

(Right-padded with NUL to 32 bytes. Encoded in `sunset-core/src/crypto/constants.rs` as a literal.)

### Layer 2: `K_epoch_n` — per-epoch root key (message plane, PFS)

Each room has a sequence of epochs `0, 1, 2, ...`. Each epoch has a 32-byte root AEAD key `K_epoch_n`. **Every message body in the room is AEAD-encrypted under a key derived from `K_epoch_n` for the epoch the message was sent in.**

`K_epoch_n` is initialized differently depending on room policy:

- **Open rooms, epoch 0:** `K_epoch_0 = HKDF-SHA256(ikm = K_room, salt = "", info = "sunset-chat-v1-epoch-0").derive(32 bytes)`. Anyone with the room name can re-derive. **Note: this means epoch 0 of an open room has no PFS — anyone who later acquires the room name reads epoch-0 message bodies. Open rooms gain real PFS only after rotating to epoch 1.**
- **Invite-only rooms, epoch 0:** `K_epoch_0 = CSPRNG(32 bytes)` generated by the room creator at room-creation time. The creator writes a key bundle addressed only to themselves.
- **Any room, epoch n+1 (rotation):** `K_epoch_{n+1} = CSPRNG(32 bytes)` generated by the rotator (see §"Who rotates"). Distributed via Layer 3.

Per-message AEAD key derivation from the epoch root:

```
K_msg = HKDF-SHA256(ikm = K_epoch_n,
                    salt = b"",
                    info = b"sunset-chat-v1-msg" || epoch_id || message_value_hash)
        .derive(32 bytes)
```

This binds each message's ciphertext to the epoch it lives in and to its own content hash. A message ciphertext therefore cannot be replayed into a different epoch even if the AEAD nonce collides (which it won't — see below).

AEAD nonce: 24 random bytes (XChaCha20-Poly1305) generated per message and stored in the ciphertext envelope. With 192 random bits, collision probability is negligible at any realistic message volume.

### Layer 3: per-recipient key bundles (epoch distribution)

When a rotator generates `K_epoch_{n+1}`, they distribute it to every legitimate recipient by encrypting it to that recipient's *epoch-public ephemeral X25519 key*. Each recipient learned of every other member's ephemeral key from the Layer-1-encrypted presence entries.

For each recipient `r`:

```
shared_secret_r = X25519(
                      sk_rotator_ephemeral_for_epoch_{n+1},
                      pk_r_ephemeral_for_epoch_n,            // r's currently-published presence key
                  )
K_wrap_r        = HKDF-SHA256(
                      ikm  = shared_secret_r,
                      salt = b"",
                      info = b"sunset-chat-v1-bundle" || rotator_id || r_id || epoch_id_{n+1},
                  ).derive(32 bytes)
ct_r            = ChaCha20Poly1305(
                      key   = K_wrap_r,
                      nonce = 12 zero bytes,                  // safe: K_wrap_r is unique per (rotator, recipient, epoch)
                      ad    = epoch_id_{n+1} || rotator_id || r_id,
                      pt    = K_epoch_{n+1},
                  )
```

The rotator's per-bundle ephemeral X25519 keypair is generated fresh per bundle (not per epoch — per bundle). Its public half is included in the bundle entry's plaintext alongside the per-recipient ciphertexts.

Wire shape of an epoch key bundle (the plaintext payload of a `SignedKvEntry`'s `ContentBlock`):

```text
EpochKeyBundle {
    epoch_id:                u64,                           // monotonic per room
    rotator_identity:        VerifyingKey,                  // 32 bytes (Ed25519)
    rotator_bundle_pk:       [u8; 32],                      // X25519 ephemeral, fresh per bundle
    member_set_hash:         [u8; 32],                      // blake3 of canonical-encoded member list
    recipients: Vec<{
        recipient_identity:  VerifyingKey,                  // 32 bytes (Ed25519)
        ciphertext:          Bytes,                         // ChaCha20Poly1305(K_wrap_r) of K_epoch_{n+1}
    }>,
}
```

`member_set_hash` lets a recipient cheaply confirm their view of the room's membership matches the rotator's view — a divergence means there's a contested membership op the recipient hasn't applied yet, and they should not adopt this bundle until they've reconciled.

The whole bundle is wrapped in a `SignedKvEntry` whose name is `<room_fingerprint>/bundle/<epoch_id>/<rotator_id>` and whose `signature` is the rotator's Ed25519 signature over the canonical entry encoding (per `sunset-core` Plan 6's `signing_payload`).

Recipients iterate `bundle.recipients`, find the entry whose `recipient_identity` matches their own, attempt X25519 + HKDF + ChaCha20Poly1305 decryption. On success, they verify the entry's outer signature and `member_set_hash`, accept the bundle, and **immediately wipe their copy of `K_epoch_n` from local memory and any persistent backend** (see §"Key wipe contract").

## Who rotates (epoch transitions)

For v1, **single-rotator semantics**:

- **Invite-only rooms.** The room admin (room creator in v1; admin set extends in a later spec) is the sole rotator. Membership ops (`member-add`, `member-remove`) are signed by the admin. Every membership op is paired with the next epoch's key bundle, written by the same admin.
- **Open rooms.** Any current member may rotate; in practice the rotator is whichever member fulfilled the most recent join request. Concurrent rotations are possible but rare given v1 expectations of room sizes; if they occur, the canonical winner is determined by a deterministic tiebreak rule: smaller `blake3(SignedKvEntry encoding)` wins. Losing rotators discard their proposal and re-acquire the winning one as a normal recipient. (This is the only place v1 admits ambiguity in epoch ordering. In v2, multi-admin invite-only rooms inherit the same tiebreak rule.)

Triggers for rotation:

| Trigger | Required for v1? |
|---|---|
| Member added | Required |
| Member removed | Required |
| Time-based (e.g., daily) | Not in v1. PFS is bounded by "time since last membership change" for stable rooms. |
| Compromise-recovery (admin requests "force rotate") | Not in v1. Admin can synthesize one by removing-and-re-adding a member. |

**On every rotation, the rotator MUST:**

1. Generate `K_epoch_{n+1}` from a CSPRNG.
2. Write the membership-change op (if any) to the store.
3. Write the key bundle to the store, addressed to every member of the new member set.
4. Wipe `K_epoch_n` from memory after successfully writing both.

## Wire envelope: a complete message

Putting layers 1-3 together, this is what the bytes-on-the-wire look like for a single chat message:

```
SignedKvEntry {                                           // sunset-store layer
    verifying_key: <sender's Ed25519 pubkey>,             // PLAINTEXT (known leak)
    name:          <room_fingerprint> || "/msg/" ||       // PLAINTEXT (room linkable to fingerprint-knowers)
                   hex(value_hash),
    value_hash:    <hash of ContentBlock below>,          // PLAINTEXT (opaque)
    priority:      sent_at_ms,                            // PLAINTEXT (timing leak)
    expires_at:    None | u64,                            // PLAINTEXT
    signature:     <Ed25519 sig over canonical entry>,    // PLAINTEXT (per Plan 6)
}

ContentBlock {                                            // sunset-store content layer
    data:       postcard(EncryptedMessage { ... }),       // ciphertext below
    references: [],                                       // empty for v1 (attachments deferred)
}

// Plaintext layout of the encrypted message envelope (the AEAD plaintext):
SignedMessage {                                           // signed inside, then encrypted
    inner_signature: [u8; 64],                            // Ed25519 over (room_fp || epoch_id || sent_at_ms || body_bytes)
    sent_at_ms:      u64,
    body:            Bytes,                               // utf-8 message text in v1; future: tagged op type
}

// Final on-the-wire ciphertext (postcard-encoded into ContentBlock.data):
EncryptedMessage {
    epoch_id:        u64,
    nonce:           [u8; 24],                            // XChaCha20Poly1305 nonce
    ciphertext:      Bytes,                               // = XChaCha20Poly1305(
                                                          //       key   = HKDF(K_epoch_n, ..., msg_value_hash),
                                                          //       nonce = nonce,
                                                          //       ad    = room_fp || epoch_id || sender_id || sent_at_ms,
                                                          //       pt    = postcard(SignedMessage),
                                                          //   )
}
```

Two signatures, two purposes, both Ed25519:

- The **outer signature** (on `SignedKvEntry`) is verified by every store backend on insert — gates store-level acceptance, prevents corrupted-entry storage, established in Plan 6.
- The **inner signature** (`SignedMessage.inner_signature`) is verified by every receiving *member* after AEAD-decrypt — provides the message-attribution authentication property (your third non-negotiable). It binds the body to the room and to the epoch, preventing cross-room and cross-epoch replay.

Both sign the same identity (the sender's Ed25519 key). The inner sig is what lets the recipient prove to themselves "this message body was authored by alice, and I have decrypted it correctly under the right epoch's key" without trusting the outer-sig path.

## Authentication invariant

Every message-decryption code path MUST execute, in order:

1. Outer entry signature verifies under `entry.verifying_key`. (Done by the store on insert.)
2. The entry's `value_hash` matches `block.hash()`. (Already an invariant of `sunset-store::Store::insert`.)
3. The `EncryptedMessage.epoch_id` corresponds to a known, accepted epoch root key.
4. AEAD decryption with the derived `K_msg` and `additional_data = room_fp || epoch_id || sender_id || sent_at_ms` succeeds.
5. The resulting `SignedMessage.inner_signature` verifies under `entry.verifying_key` over `(room_fp || epoch_id || sent_at_ms || body)`.

A message is delivered to the application layer **only if all five steps pass**. Any failure is treated as a forgery attempt, the entry is rejected from local view (it remains in the store as a signed CRDT event, but is not surfaced to the user), and is logged for diagnostics.

## Membership and join

### Open rooms

- A peer with the room name derives `K_room`, computes `room_fingerprint`, and subscribes to `Filter::NamePrefix(room_fingerprint || "/")` via sunset-sync.
- They publish a presence entry: a `SignedKvEntry` with name `<room_fp>/presence/<sender_id>` whose `ContentBlock.data` is `K_room`-AEAD-encrypted plaintext containing their current epoch-ephemeral X25519 pubkey, their display name, and a TTL.
- An existing member sees the new presence entry, decides to grant the joiner access, and writes a key bundle for the next epoch addressed to all current members + the joiner. The new member set is `current_members ∪ {joiner}`.
- The joiner fetches the bundle, decrypts, and is now in epoch n+1.
- For epoch 0 specifically, no bundle is needed — the joiner re-derives `K_epoch_0` from `K_room` directly. They can read messages back to epoch 0 if and only if those messages were ever sent in epoch 0 of an open room.

### Invite-only rooms

- A member is added by the admin, who writes a `member-add` op (a `SignedKvEntry` at `<room_fp>/member/<added_id>`, encrypted under `K_room`, with `ContentBlock.data` containing `{ added_identity, added_at_ms, role }` and signed by the admin).
- Pairs with a key bundle for epoch n+1 addressed to the new member set.
- Removed members lose access to all future epochs because they're not in the new bundle's recipient list.
- The current member set is reconstructed by a recipient from the chain of `member-add` / `member-remove` ops, ordered by their store priority and identified by the canonical `member_set_hash`.

## Key wipe contract

This is what makes PFS real and is therefore a **load-bearing invariant**:

> When a peer accepts a new epoch's root key (via successful decryption of a key bundle), the peer MUST erase `K_epoch_{n}` and every derived per-message key from:
> - process memory (zeroize the buffer),
> - any persistent backend (`sunset-store-fs`'s SQLite store, `sunset-store-indexeddb`'s object store, `sunset-store-memory`'s map),
> - any in-memory cache used for message decryption,
>
> **before** acknowledging the new epoch as current. There is no "grace period" for late-arriving messages from prior epochs. Late-arriving messages from epoch n that arrive after epoch n+1 has been accepted are unreadable. This is by design; PFS is incompatible with key retention.

The trait surface (§"Trait surface" below) makes this an explicit method on the host-side key store, not an "expected behavior" of an opaque key cache.

`zeroize` (the crate) is the standard way to erase secret bytes in Rust without optimizer interference. It is wasm-compatible.

## PFS scope and v1 limits

Per the brainstorm, the v1 PFS guarantee is:

> **If a peer's identity key (Ed25519) is compromised today, message bodies sent in any prior epoch are unrecoverable from that compromise alone. Message bodies sent in the *current* epoch (since the most recent membership change) remain exposed.**

This is a meaningful, honest property. It is weaker than Signal's per-message PFS and stronger than any scheme without rotation. The wire envelope is structured so that when sub-epoch ratcheting is added later, only the per-message key derivation changes (HKDF info string), not the bundle wire shape or the layered structure.

## Architecture-spec amendment

The architecture spec §"Wire envelope and encryption" currently describes a single AEAD layer with a static room key derived from `Argon2id(room_name)`, plus a Noise handshake for forward secrecy whose key schedule is left open. This subsystem spec **supersedes** that section in three specific ways:

1. **Rooms-as-shared-passwords semantics (high-entropy room name → historical read for new joiners) is dropped.** New joiners only see message bodies from the epoch they joined onward. Open rooms' epoch 0 retains a name-derivable key for compatibility with the existing room-fingerprint mechanism, but a room that ever rotates past epoch 0 cannot have its history retroactively read by name-knowers.
2. **The Noise handshake is replaced by direct X25519 + HKDF for one-shot key delivery.** The CRDT store is not an interactive channel; Noise's interactivity is unhelpful here. We pay no security cost: X25519 + HKDF + AEAD-with-known-recipient-pubkey *is* the inner primitive every Noise handshake uses; we just skip the handshake state machine.
3. **The "group session key" is given a concrete shape** — `K_epoch_n`, rotated on every membership change, distributed as a key bundle, with named primitives (X25519, HKDF-SHA256, ChaCha20-Poly1305 / XChaCha20-Poly1305).

A revision marker is added to the architecture spec pointing at this document.

## Trait surface

`sunset-core` introduces three new traits, each with a default implementation. Hosts can override for testing or for HSM-backed key storage.

```rust
pub trait RoomKeyDerivation {
    /// Argon2id(room_name) → K_room. Tunable via params for testing (faster
    /// in tests, OWASP-2023 in production).
    fn derive_room_key(&self, room_name: &str) -> [u8; 32];

    /// blake3-keyed hash of K_room → room_fingerprint.
    fn room_fingerprint(&self, k_room: &[u8; 32]) -> [u8; 32];
}

pub trait EpochKeyStore {
    /// Look up the current epoch's root key for a room.
    fn current_epoch(&self, room: &RoomFingerprint) -> Option<EpochId>;
    fn epoch_root(&self, room: &RoomFingerprint, epoch: EpochId) -> Option<&[u8; 32]>;

    /// Insert a fresh epoch root, atomically wiping every prior epoch root
    /// for this room. This is the load-bearing PFS contract.
    fn install_epoch_root(
        &mut self,
        room: &RoomFingerprint,
        epoch: EpochId,
        root: [u8; 32],
    );

    /// Out-of-band wipe (e.g., on app shutdown, or "sign out").
    fn wipe_room(&mut self, room: &RoomFingerprint);
}

pub trait MessageCrypto {
    fn encrypt_message(
        &self,
        identity:    &Identity,                // signs the inner sig
        room_fp:     &RoomFingerprint,
        epoch_id:    EpochId,
        epoch_root:  &[u8; 32],
        sent_at_ms:  u64,
        body:        &[u8],
    ) -> EncryptedMessage;

    fn decrypt_message(
        &self,
        sender:      &IdentityKey,             // verifies the inner sig
        room_fp:     &RoomFingerprint,
        epoch_root:  &[u8; 32],
        encrypted:   &EncryptedMessage,
    ) -> Result<DecryptedMessage>;
}
```

Default implementations live in `sunset-core/src/crypto/`. The `EpochKeyStore` default uses a `HashMap<RoomFingerprint, BTreeMap<EpochId, Zeroizing<[u8; 32]>>>` and *enforces* the wipe contract by overwriting prior entries with `Zeroizing` — the trait method's signature commits to atomic install + wipe.

## Cryptographic primitives — chosen versions

All available as well-maintained Rust crates that compile to `wasm32-unknown-unknown`:

| Primitive | Crate | Version (workspace dep) | Notes |
|---|---|---|---|
| Ed25519 sign/verify | `ed25519-dalek` | 2.x | Already added in Plan 6. |
| X25519 ECDH | `x25519-dalek` | 2.x | `serde` feature for ephemeral pubkey serialization. |
| ChaCha20-Poly1305 / XChaCha20-Poly1305 | `chacha20poly1305` | 0.10.x | Both AEADs from one crate. |
| HKDF-SHA256 | `hkdf` + `sha2` | hkdf 0.12, sha2 0.10 | |
| Argon2id | `argon2` | 0.5.x | OWASP 2023 params; tunable for tests via a `Params` struct. |
| Constant-time compare | `subtle` | 2.x | For verifying tags in custom code paths if any. |
| Secret wiping | `zeroize` | 1.x | Wraps `[u8; 32]` with `Zeroizing<...>` for guaranteed erasure. |
| RNG | `rand_core` | 0.6 | Already added in Plan 6. Hosts inject a `CryptoRngCore`. |

Wire-format constants (HKDF info strings, Argon2id salt, fingerprint domain separator) live in `sunset-core/src/crypto/constants.rs` as `pub const` byte literals, with frozen test vectors next to them — same discipline as the canonical signing payload in Plan 6.

## Test discipline (this spec mandates)

Each primitive has at least:

- **Round-trip tests** (encrypt → decrypt → original).
- **Tamper-detection tests** (flip a bit in nonce / ciphertext / additional-data → decryption fails; flip a bit in inner signature payload → inner verify fails).
- **Cross-key tests** (alice's bundle ciphertext under bob's `K_wrap` does not decrypt).
- **Frozen wire-format vectors** — for `EncryptedMessage`, `EpochKeyBundle`, the constant strings, and the Argon2id of a fixed test room name. If any of these change, signatures and ciphertexts ever produced under v1 become invalid, so the constants are bumped behind a wire-format version.
- **PFS-wipe test** — install epoch n+1, attempt decryption of an epoch-n ciphertext, observe that the prior key is no longer accessible from `EpochKeyStore::epoch_root`.
- **Key-bundle replay test** — replaying a stale bundle to an old recipient does not change the current epoch.

Two-peer integration tests (extending Plan 6's `crates/sunset-core/tests/two_peer_message.rs`) demonstrate:

- Open room: alice posts in epoch 0, bob (who joined later) decrypts.
- Open room: alice rotates to epoch 1, bob participates, charlie (who has the room name but never showed up) cannot decrypt epoch-1 messages.
- Invite-only room: admin posts, member decrypts; non-member with the room name cannot decrypt.
- Authentication: a forged inner-signature attempt is rejected at step 5 of the §"Authentication invariant".
- PFS: after rotation, decryption attempts against epoch-n ciphertexts using the new EpochKeyStore state fail.

## Items deferred to follow-up subsystem specs

These are explicitly **out of v1** to keep the spec implementable as one focused plan, but they are real and tracked:

1. **Sender-identity hiding inside a room** (per-room ephemeral signing keys with "endorsed by identity X" in the ciphertext).
2. **Sub-epoch PFS** (per-message ratcheting, sender-key chains, or MLS-shaped TreeKEM).
3. **Hybrid PQC** for both signing (ML-DSA + Ed25519) and KEM (ML-KEM + X25519).
4. **Multi-admin invite-only rooms** with concurrent-rotation conflict resolution beyond the simple tiebreak rule.
5. **Time-based rotation** (e.g., rotate every 24 hours regardless of membership).
6. **Cover traffic / size padding / timing decoys.**
7. **Federated identity layer** (handles, delegation chains, `sunset-trust`) — already a planned subsystem.
8. **Voice-channel crypto** — separate subsystem; uses a session key derived from a Noise handshake at call setup, not the chat epoch key.
9. **Multi-device** identity and per-device session keys.

## Implementation sequencing (informational — actual plans live in plans/)

Suggested order:

- **Phase A — primitives + traits** (extends Plan 6's sunset-core scope or adds a Plan 7).
  - Add the crypto primitives, the constants module with frozen vectors, the `RoomKeyDerivation` and `MessageCrypto` traits + default impls, and the `EncryptedMessage` wire shape.
  - Replace Plan 6's plaintext message body with `EncryptedMessage`-wrapped bodies; introduce `K_epoch_0`.
- **Phase B — epoch rotation and key bundles** (Plan 8).
  - Add `EpochKeyStore` with the PFS wipe contract.
  - Add `EpochKeyBundle` wire shape, the X25519+HKDF wrap/unwrap path, the rotator code path.
  - Add presence entries with ephemeral pubkeys.
  - Two-peer integration test: alice rotates, bob receives the new bundle, decrypts a post-rotation message.
- **Phase C — membership ops** (Plan 9).
  - `member-add` / `member-remove` ops, signed by admin.
  - Member-set reconstruction from the op chain.
  - Invite-only-room admission enforcement.

Each phase is independently testable and produces a meaningful slice of the system.

## Self-review checklist

- [x] All three non-negotiables (E2EE, PFS, authentication) are met by an explicit mechanism in this spec.
- [x] The PFS limit (per-epoch, not per-message) is explicitly stated, not hidden.
- [x] The known plaintext leaks (sender vk, room fingerprint, priority) are explicitly stated.
- [x] Open and invite-only rooms share the message-decryption code path; they differ only in epoch-0 init and rotation authorization.
- [x] The architecture-spec amendment is explicit and identifies the three superseded points.
- [x] Every named primitive has a concrete crate and version in §"Cryptographic primitives".
- [x] The PFS wipe contract is given a trait method, not a comment.
- [x] Test discipline mandates tamper, replay, and PFS-wipe tests, not just round-trip tests.
- [x] Deferred items are listed; nothing is left as "TBD" in the active scope.
