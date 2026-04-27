# sunset-sync native WebSocket transport (Plan C) — Subsystem design

- **Date:** 2026-04-27
- **Status:** Draft (subsystem-level)
- **Scope:** First real `Transport` implementation for `sunset-sync`. Native (`tokio-tungstenite`) WebSocket transport, decorated by a generic Noise wrapper, plus the sync-internal Ed25519 signing path that's currently stubbed. Plan C in the web roadmap (A → C → D → E).

## Non-negotiable goals

1. **Real wire transport.** Two `SyncEngine` instances on different processes (or different tokio tasks) can exchange `SyncMessage`s over a real localhost WebSocket connection.
2. **Per-connection mutual authentication + encryption.** Every connection is wrapped in a Noise tunnel. Each side cryptographically proves its identity via its Ed25519 keypair (converted to X25519 for Noise ECDH). A `TransportConnection`'s `peer_id()` comes out of the Noise handshake — there's no way to talk over an unauthenticated connection.
3. **Sync-internal signing.** `SyncEngine` writes signed `_sunset-sync/subscribe` (and future presence/key-bundle) entries with real Ed25519 signatures. The `Ed25519Verifier`-on-receiver workaround that Plan 6 worked around (`MemoryStore::with_accept_all()` in the integration test) is no longer needed.
4. **Layering: transport crates have ZERO crypto knowledge.** A `RawTransport` trait carries plain bytes. `sunset-noise` is the only crate that knows about Noise / Ed25519. New transports (browser WebSocket, WebRTC, WebTransport) implement only `RawTransport` and slot into the same `NoiseTransport<R>` decorator.

## Non-goals (deferred)

- **Browser WebSocket transport.** Separate plan — implementation is different enough (web-sys `WebSocket` is event-callback shaped, no inbound) that splitting is cleaner. Plan E.transport.
- **WebRTC, WebTransport** — separate plans (Plan W, Plan WT).
- **`sunset-relay` binary, multi-relay integration tests.** Separate plan (Plan D). This plan validates the transport + signing with a two-peer WebSocket test; D builds the relay process and tests multi-relay topologies.
- **Hybrid post-quantum cryptography** — for v0, classical `Noise_IK_25519_XChaChaPoly_BLAKE2b`. A unified hybrid-PQC subsystem spec will later cover Noise + Plan 7 key bundles + Plan 9 signatures together; doing PQ on Noise alone is a half-measure since the inner key bundles are still classical X25519+HKDF.
- **Multi-relay support per client, relay discovery.** Architecture-spec features that ride on top of this transport.
- **Connection-level QoS** (rate limiting, congestion control). Operational concerns; can layer above the transport.

## Threat model

The Noise layer specifically defends against:

| Adversary | Sees | Cannot |
|---|---|---|
| Passive on-wire observer | Connection metadata (timing, sizes), Noise handshake messages | Read post-handshake payloads. Identify either peer's static pubkey from on-wire bytes (IK encrypts initiator's static; responder's static is known by initiator out-of-band so isn't transmitted). |
| Active MITM / TLS-terminating proxy | Same as above, plus full TLS visibility if `wss://` | Forge or read the Noise tunnel inside (Noise is independent of TLS). Replays detected by Noise's nonce counters. |
| Compromised relay | Everything that arrives via the connection (which is the same as any sunset-sync peer would see). All entries already have outer Ed25519 sigs and ciphertext bodies — relay sees what the protocol allows. | Forge entries (signature verification at every receiver). Decrypt epoch ciphertext bodies (relay isn't a member of the epoch). |
| Future quantum adversary recording today's traffic | Recorded ciphertext + Noise handshake bytes. Could break X25519 retroactively (Shor's). | Decrypt past Noise sessions IF the per-message AEAD inside survives separately — but in v0 it doesn't, since the wrapped Plan 7 epoch-bundle deliveries also use classical X25519. **A unified hybrid-PQC subsystem spec will close this gap.** |

## Architecture

### New trait split: RawTransport vs Transport

```
SyncEngine
   ↑ consumes
Transport         (existing — authenticated; TransportConnection has peer_id())
   ↑ implemented by
NoiseTransport<R: RawTransport>     (in sunset-noise — the only crate that knows Noise)
   ↑ decorates
RawTransport      (NEW — plain bytes pipe; RawConnection has NO peer_id())
   ↑ implemented by
WebSocketRawTransport                 (in sunset-sync-ws-native — zero crypto deps)
WebRtcRawTransport                    (future)
WebTransportRawTransport              (future)
```

`RawTransport` lives in `crates/sunset-sync/src/transport.rs` alongside the existing `Transport`. Same module, two trait pairs, clear separation:

```rust
// EXISTING — unchanged
#[async_trait(?Send)]
pub trait Transport {
    type Connection: TransportConnection;
    async fn connect(&self, addr: PeerAddr) -> Result<Self::Connection>;
    async fn accept(&self) -> Result<Self::Connection>;
}

#[async_trait(?Send)]
pub trait TransportConnection {
    async fn send_reliable(&self, bytes: Bytes) -> Result<()>;
    async fn recv_reliable(&self) -> Result<Bytes>;
    async fn send_unreliable(&self, bytes: Bytes) -> Result<()>;
    async fn recv_unreliable(&self) -> Result<Bytes>;
    fn peer_id(&self) -> PeerId;
    async fn close(&self) -> Result<()>;
}

// NEW
#[async_trait(?Send)]
pub trait RawTransport {
    type Connection: RawConnection;
    async fn connect(&self, addr: PeerAddr) -> Result<Self::Connection>;
    async fn accept(&self) -> Result<Self::Connection>;
}

#[async_trait(?Send)]
pub trait RawConnection {
    async fn send_reliable(&self, bytes: Bytes) -> Result<()>;
    async fn recv_reliable(&self) -> Result<Bytes>;
    async fn send_unreliable(&self, bytes: Bytes) -> Result<()>;
    async fn recv_unreliable(&self) -> Result<Bytes>;
    async fn close(&self) -> Result<()>;
    // No peer_id — raw connections haven't authenticated anyone.
}
```

The existing `TestTransport` (under the `test-helpers` feature) keeps implementing `Transport` directly — useful for tests that want to skip the Noise layer.

### Noise wrapper crate

`crates/sunset-noise/`:

```
sunset-noise/
├── Cargo.toml
└── src/
    ├── lib.rs
    ├── handshake.rs        # NoiseTransport, NoiseConnection
    ├── identity.rs         # Ed25519 → X25519 conversion helpers
    └── pattern.rs          # NOISE_PATTERN constant + version
```

**Pattern**: `Noise_IK_25519_XChaChaPoly_BLAKE2b`. Pinned exactly. Decoded:

- `IK` — initiator knows responder's static pubkey out-of-band (from PeerAddr); initiator's static pubkey is sent encrypted in the first message. 1 RTT total. Mutual authentication.
- `25519` — X25519 ECDH. Identity keys are Ed25519; we derive a per-handshake X25519 key from the Ed25519 secret seed using the standard curve conversion.
- `XChaChaPoly` — XChaCha20-Poly1305 (24-byte nonces, larger than ChaChaPoly's 12 bytes). Matches sunset-core's per-message AEAD choice — consistent stack.
- `BLAKE2b` — 64-byte hash output. Faster than BLAKE2s on 64-bit (relays); fine in wasm.

**Underlying impl**: `snow` crate (the de facto Rust Noise impl). The pattern string is a one-liner snow accepts.

**Identity → X25519 derivation**: `ed25519-dalek` exposes `SigningKey::to_scalar_bytes()`; `curve25519-dalek` provides the Edwards-to-Montgomery point conversion. The standard pair: hash the Ed25519 secret seed with SHA-512, take the first 32 bytes (clamped) as the X25519 secret. This is the well-documented "convert Ed25519 to X25519" path used by Signal / WireGuard / etc. Same identity bytes, two valid uses.

### `Signer` trait

In `crates/sunset-sync/src/signer.rs` (new module):

```rust
use bytes::Bytes;
use sunset_store::VerifyingKey;

/// Per-peer signing capability. The host injects this into `SyncEngine`
/// at construction; the engine uses it when writing its own internal entries
/// (subscriptions, presence, key bundles).
///
/// `sunset-core::Identity` implements this in the application layer; tests
/// can implement a stub.
pub trait Signer: Send + Sync {
    /// The verifying-key bytes that match this signer's signatures.
    fn verifying_key(&self) -> VerifyingKey;

    /// Produce an Ed25519 signature over `payload`. Returns 64 bytes.
    fn sign(&self, payload: &[u8]) -> Bytes;
}
```

The trait is `Send + Sync` so a single signer instance can be shared across tasks. (Compatible with the existing `SignatureVerifier` discipline.)

### `signing_payload` moves to sunset-store

The canonical signing payload encoding (currently in `crates/sunset-core/src/canonical.rs`) is a property of the `SignedKvEntry` wire format — it belongs to the store crate, not the application crate. Move:

- `crates/sunset-core/src/canonical.rs` → `crates/sunset-store/src/canonical.rs`
- `crates/sunset-core/src/lib.rs`: re-export `pub use sunset_store::canonical::signing_payload;` (and the `Error` variant if applicable) so existing sunset-core consumers don't break.
- `crates/sunset-core/src/verifier.rs`: import from `sunset_store::canonical` instead of `crate::canonical`.
- The frozen-vector test moves with the function.

This unblocks `sunset-sync` from depending on sunset-core just to access the canonical payload encoding. Alternative — duplicating the encoding in sunset-sync — would risk drift; centralizing in the store is the right home.

### SyncEngine constructor change

```rust
impl<S, T> SyncEngine<S, T>
where
    S: Store + 'static,
    T: Transport + 'static,
{
    pub fn new(
        store: Arc<S>,
        transport: T,
        config: SyncConfig,
        local_peer: PeerId,
        signer: Arc<dyn Signer>,           // NEW required parameter
    ) -> Self { ... }
}
```

`do_publish_subscription` (and any future `publish_presence`, `publish_key_bundle`):

```rust
async fn do_publish_subscription(&self, filter: Filter, ttl: Duration) -> Result<()> {
    use sunset_store::{ContentBlock, SignedKvEntry, canonical::signing_payload};

    let value = postcard::to_stdvec(&filter).map_err(...)?;
    let block = ContentBlock {
        data: Bytes::from(value),
        references: vec![],
    };
    let now_secs = ...;

    let mut entry = SignedKvEntry {
        verifying_key: self.local_peer.0.clone(),
        name: Bytes::from_static(reserved::SUBSCRIBE_NAME),
        value_hash: block.hash(),
        priority: now_secs,
        expires_at: Some(now_secs.saturating_add(ttl.as_secs())),
        signature: Bytes::new(),
    };
    let payload = signing_payload(&entry);
    entry.signature = self.signer.sign(&payload);   // REAL signature now

    self.store.insert(entry, Some(block)).await?;
    Ok(())
}
```

### WebSocket address scheme

`PeerAddr` is opaque `Bytes`; each transport interprets it. For the WebSocket transport, the bytes are a URL with a required fragment carrying the responder's expected static pubkey:

```
wss://relay.example.com:443#x25519=<64-hex-chars>
ws://localhost:8080#x25519=<64-hex-chars>
```

The `x25519` fragment carries the **post-conversion** X25519 pubkey (32 bytes, hex-encoded) so the initiator doesn't need to know about Ed25519 → X25519 conversion to dial. This is the responder's "static identity" from Noise's perspective.

**Why fragment, not query**: fragments are intentionally not sent to HTTP servers, so this stays purely client-side. The actual WebSocket handshake URL (without fragment) is what hits the server.

**Why X25519 not Ed25519 in the address**: the dialer would have to convert before opening the Noise handshake anyway; doing it once at config time and using the converted form on the wire is simpler. (The Ed25519 → X25519 conversion is one-way deterministic, so config tools can derive the address from a known Ed25519 pubkey.)

For the relay's accept side, the relay only needs its own Ed25519 secret + the listen URL — it doesn't need to know the connecting client's pubkey ahead of time (IK responder learns the initiator's static during the handshake).

## Components

### `sunset-noise` crate

```toml
[dependencies]
async-trait.workspace = true
bytes.workspace = true
snow = { version = "0.10", default-features = false, features = ["default-resolver", "use-blake2", "use-chacha20poly1305"] }
ed25519-dalek.workspace = true
curve25519-dalek = { version = "4", default-features = false, features = ["alloc"] }
sha2.workspace = true
sunset-store.workspace = true
sunset-sync.workspace = true
thiserror.workspace = true
zeroize.workspace = true
```

(`use-blake2` and `use-chacha20poly1305` are snow's feature flags for the cipher/hash choices in our pattern.)

Public surface:

```rust
pub const NOISE_PATTERN: &str = "Noise_IK_25519_XChaChaPoly_BLAKE2b";

pub struct NoiseTransport<R: RawTransport> { ... }

impl<R: RawTransport> NoiseTransport<R> {
    /// Initiator side. The local identity's secret and the responder's
    /// expected static pubkey are folded into each `connect()` call via the
    /// PeerAddr, so a single NoiseTransport instance can dial multiple peers.
    pub fn new(raw: R, local_identity: Arc<dyn NoiseIdentity>) -> Self;
}

impl<R: RawTransport> Transport for NoiseTransport<R> {
    type Connection = NoiseConnection<R::Connection>;
    async fn connect(&self, addr: PeerAddr) -> Result<Self::Connection> { ... }
    async fn accept(&self) -> Result<Self::Connection> { ... }
}

pub struct NoiseConnection<C: RawConnection> { ... }

impl<C: RawConnection> TransportConnection for NoiseConnection<C> {
    fn peer_id(&self) -> PeerId { /* from Noise handshake */ }
    async fn send_reliable(&self, bytes: Bytes) -> Result<()> { /* encrypt then raw send */ }
    async fn recv_reliable(&self) -> Result<Bytes> { /* raw recv then decrypt */ }
    // Unreliable channel: passes through to RawConnection without Noise framing
    // for v0; voice (which uses unreliable) gets its own session keys later.
    async fn send_unreliable(&self, bytes: Bytes) -> Result<()> { ... }
    async fn recv_unreliable(&self) -> Result<Bytes> { ... }
    async fn close(&self) -> Result<()> { ... }
}

pub trait NoiseIdentity: Send + Sync {
    /// The Ed25519 verifying key — published as the peer's identity.
    fn ed25519_public(&self) -> [u8; 32];

    /// Provide the X25519 static secret used during the Noise handshake.
    /// Implementations should derive this from the Ed25519 secret seed via
    /// the standard SHA-512-clamp conversion. The returned secret MUST be
    /// zeroized when dropped.
    fn x25519_static_secret(&self) -> Zeroizing<[u8; 32]>;
}
```

`sunset-core::Identity` implements `NoiseIdentity` (and `Signer`) — that lives in sunset-core, not sunset-noise, to avoid a dep inversion.

### `sunset-sync-ws-native` crate

```toml
[dependencies]
async-trait.workspace = true
bytes.workspace = true
sunset-sync.workspace = true
thiserror.workspace = true
tokio = { workspace = true, features = ["sync", "rt", "macros", "net"] }
tokio-tungstenite = "0.24"
url = "2"
futures-util = { workspace = true, default-features = false, features = ["sink"] }
```

Zero crypto deps. The crate's responsibilities:

- Parse a `PeerAddr`'s URL (ignore the fragment — that's for the Noise wrapper).
- Open a `tokio_tungstenite::connect_async()` to the URL.
- Listen on a configured `TcpListener`, upgrade incoming connections to WebSocket via `tokio_tungstenite::accept_async`.
- Wrap each WebSocket into a `WebSocketRawConnection` whose `send_reliable` sends a binary frame and `recv_reliable` reads the next binary frame as `Bytes`.
- `send_unreliable` / `recv_unreliable`: return `Error::Unsupported` (WebSocket is reliable-only). The caller (Noise wrapper) handles this gracefully — voice doesn't use this transport in v0.

Public surface:

```rust
pub struct WebSocketRawTransport {
    listener: Option<TcpListener>,    // None = dial-only
    accept_buffer: ...,
}

impl WebSocketRawTransport {
    pub fn dial_only() -> Self;
    pub async fn listening_on(bind_addr: SocketAddr) -> Result<Self>;
}

impl RawTransport for WebSocketRawTransport {
    type Connection = WebSocketRawConnection;
    async fn connect(&self, addr: PeerAddr) -> Result<Self::Connection>;
    async fn accept(&self) -> Result<Self::Connection>;
}

pub struct WebSocketRawConnection { ... }

impl RawConnection for WebSocketRawConnection { ... }
```

### Two-peer integration test

`crates/sunset-sync-ws-native/tests/two_peer_ws_noise.rs`:

```rust
//! End-to-end: alice (dialer) and bob (listener) exchange a real
//! sunset-core encrypted+signed message over a real localhost WebSocket
//! wrapped in Noise. Both stores use Ed25519Verifier — proves the
//! sync-internal signing path is real (no AcceptAllVerifier workaround).
```

Steps the test exercises:

1. Bob binds a TCP listener; constructs `NoiseTransport::new(WebSocketRawTransport::listening_on(...), bob_identity)`.
2. Alice constructs `NoiseTransport::new(WebSocketRawTransport::dial_only(), alice_identity)`.
3. Both stores: `MemoryStore::new(Arc::new(Ed25519Verifier))`. NOT `with_accept_all`.
4. Both `SyncEngine::new(..., signer = Arc::new(<id>.clone()))`.
5. Bob publishes interest in `room_messages_filter("general-test")`.
6. Alice dials bob's address (URL with `#x25519=<bob's converted pubkey>`).
7. Wait for alice to learn bob's subscription.
8. Alice composes via sunset-core, inserts into her store.
9. Bob receives entry + block via sync.
10. Bob decodes via sunset-core, asserts author key + body match.

Same shape as the existing `crates/sunset-sync/tests/two_peer_sync.rs` (TestNetwork) and `crates/sunset-core/tests/two_peer_message.rs` (also TestNetwork) but over real WebSocket + Noise + Ed25519Verifier.

## Tests + verification

- **Native unit tests in sunset-noise**: handshake roundtrip with two locally-instantiated NoiseTransports talking over an in-memory pipe (no WebSocket); X25519-from-Ed25519 derivation matches a frozen test vector.
- **Native unit tests in sunset-sync-ws-native**: listen + dial + send/recv binary frame; connection-close behavior.
- **Native unit tests in sunset-sync** (Signer plumbing): SyncEngine::do_publish_subscription writes a real-signed entry; an `AcceptAllVerifier` test checks the entry has a non-empty signature; an `Ed25519Verifier` test checks the signature actually verifies.
- **Integration test (the headline)**: the two-peer-over-WS-and-Noise test described above.
- **No regressions**: full workspace `cargo test --workspace --all-features` and `cargo clippy ... -D warnings` clean.
- **WASM compatibility**: sunset-noise must build cleanly for `wasm32-unknown-unknown` (the future browser transport will use it). sunset-sync-ws-native is native-only and doesn't need to.

## Items deferred

- Browser WebSocket transport — separate plan.
- WebRTC, WebTransport — separate plans.
- Hybrid post-quantum cryptography (Noise HFS + Plan 7 PQ key bundles + Plan 9 PQ signatures) — single unified PQC subsystem spec when the v0 stack is shipped.
- `sunset-relay` binary, multi-relay integration tests — Plan D.
- Connection retry / backoff / multiplexing — operational concerns.
- Multi-relay client config — operational concern that rides on top of this transport.

## Self-review checklist

- [x] Three non-negotiables (real wire transport, mutual auth+encryption, sync-internal signing) are met by named mechanisms.
- [x] Noise pattern is fully specified (`Noise_IK_25519_XChaChaPoly_BLAKE2b`) — no ambiguity.
- [x] Ed25519 → X25519 derivation method is explicit (SHA-512 clamp, the standard conversion).
- [x] PeerAddr scheme is concrete (`wss://host:port#x25519=<hex>`).
- [x] Crate split rationale (zero-crypto-deps in transports) is explicit.
- [x] Threat model honestly notes that PQ on Noise alone is incomplete without inner-layer PQ.
- [x] Two-peer test description is concrete enough to plan against.
- [x] Deferred items prevent scope creep.
