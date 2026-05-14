# sunset-store and sunset-sync — Subsystem Design

- **Date:** 2026-04-25
- **Status:** Approved
- **Scope:** Combined subsystem design for `sunset-store` (data plane) and `sunset-sync` (replication plane). The two are designed together because their decisions are deeply intertwined — the sync layer reuses the store's CRDT primitives to replicate its own configuration, and the store's filter expression doubles as the sync layer's interest-set encoding.
- **Parent:** [`2026-04-25-sunset-chat-architecture-design.md`](2026-04-25-sunset-chat-architecture-design.md). When this spec contradicts the architecture spec, the architecture spec wins; this document refines but never overrides it.

## Refinements to the architecture spec

This document refines two architecture-spec choices in small ways:

1. **`sunset-sync` becomes a separate crate**, parallel to `sunset-store`. The architecture spec described sync as a layer "inside `sunset-core`"; in practice the sync code is non-trivial enough (peer protocol, digest exchange, interest-set matching) to deserve its own crate, and that crate becomes reusable for non-chat consumers of the store. `sunset-core` depends on `sunset-sync` rather than embedding it.
2. **The `Transport` trait moves from `sunset-core` to `sunset-sync`**, since `sunset-sync` is the trait's only consumer. `sunset-core` reaches the trait through `sunset-sync`. Hosts still inject the concrete `Transport` impl.

Neither refinement changes any architectural commitment in the parent spec.

## sunset-store

### Purpose and shape

`sunset-store` is a Rust crate that compiles to native and to WebAssembly. It provides:

- A signed CRDT key-value store with priority-based last-write-wins, TTL pruning, and no tombstones.
- A content-addressed blob store keyed by blake3.
- Subset queries by keyspace, namespace, and name prefix.
- Async-stream subscriptions with historical replay and cursor-based resume.
- A pluggable storage backend trait, implemented separately for memory, IndexedDB, and SQLite-plus-filesystem.

The store is **message-agnostic** — it knows nothing about chat semantics, identity, or rooms. Sunset.chat is one consumer; other applications could use the store independently for any signed-CRDT-with-content-addressing workload.

### Data model

Two stored types, both serialized canonically with **postcard** (frozen schema; the canonical bytes are part of the wire format and the input to ContentBlock hashing):

```rust
pub struct SignedKvEntry {
    pub verifying_key: VerifyingKey,
    pub name:          Bytes,            // application-opaque
    pub value_hash:    Hash,             // blake3 of a ContentBlock; the actual value lives in the content store
    pub priority:      u64,              // monotonic ordering for LWW; default = wall-clock at write, set by caller
    pub expires_at:    Option<u64>,      // optional TTL; entries past expiry get pruned
    pub signature:     Bytes,            // covers the canonical encoding of all fields above
}

pub struct ContentBlock {
    pub data:       Bytes,                // payload bytes (typically AEAD ciphertext at the application layer)
    pub references: Vec<Hash>,            // explicit references to other ContentBlocks; covered by the block's hash
}
```

`hash(content_block) = blake3(postcard::to_stdvec(&content_block))`. Two ContentBlocks with the same canonical bytes have the same hash and are deduplicated automatically.

KV entries are tiny and uniform — a few hundred bytes regardless of value size. All actual data lives in the content store. The KV layer is a typed pointer index over content; the content store is the data.

#### Why this shape

- **Pure content-addressed data.** Garbage collection is uniform — anything reachable from a live KV pointer is alive; everything else is dead.
- **Deduplication.** A blob shared by many KV entries is stored once. During replication, peers re-sending KV entries don't have to re-send the underlying blobs.
- **Tamper-evidence.** The signature on a KV entry covers `value_hash`. A malicious relay can't substitute a different ContentBlock without invalidating the signature.
- **Replication-friendly.** A peer can ship `(SignedKvEntry, ContentBlock)` in one message, or just the entry if it has reason to believe the receiver already has the blob.

#### Note on content-DAG topology

`ContentBlock.references` form an arbitrary DAG. The store stores blocks and references but does not police what topology consumers build. **Avoiding the leak of message/content structure through the reference graph is an application-layer concern** — sunset-core's chat layer is responsible for choosing a topology (Merkle-style fan-out, padding, batching multiple operations per block) that doesn't expose meaningful structure to relays. The store will faithfully store whatever shape consumers give it.

### LWW semantics

For a KV insert with key `(verifying_key, name)`:

| Existing entry         | Action                                              |
| ---------------------- | --------------------------------------------------- |
| None                   | Insert.                                             |
| `priority >= new`      | Reject as `Stale` (idempotent re-send is a no-op).  |
| `priority < new`       | Replace: delete the old row, insert the new.        |

TTL pruning runs independently of LWW: a background sweep deletes entries past `expires_at`.

### Garbage collection

Mark-and-sweep over content blobs:

1. Walk every live `SignedKvEntry`; accumulate `value_hash` into the live root set.
2. For each ContentBlock in the live root set, walk its `references` transitively, marking everything reachable.
3. Any ContentBlock not in the marked set is unreferenced.
4. Sweep deletes unreferenced blobs.

Cadence and incremental design are backend-specific. Native backends run periodic background sweeps; the IndexedDB backend likely runs sweeps on demand or on a longer cadence under browser quotas. Concurrent-insert safety during a sweep — handled by tracking inserts started before the sweep cutoff and not deleting their referenced blobs — is implementation detail of each backend.

Per-event atomic insert (see §Atomicity below) means that no orphan-blob cleanup is needed for crashes — partial writes are never committed.

### Trust boundary at the store

The store does **structural signature verification only**. On every insert:

1. `SignatureVerifier::verify(entry)` is called.
2. The verifier checks the math: `signature` is a valid signature over the canonical encoding of the entry's other fields, made with `verifying_key`.
3. If verification fails, insert returns `Error::SignatureInvalid` and nothing is stored.

The store does **not** know about identity delegations, room admission, or any application-specific notion of "is this verifying_key allowed to write this name." Those are concerns of the layer above. The store just guarantees that every stored entry was signed by the key it claims.

### API surface

Single trait, async, `?Send` futures so non-`Send` WASM backends are accepted:

```rust
#[async_trait(?Send)]
pub trait Store {
    async fn insert(&self, entry: SignedKvEntry, blob: Option<ContentBlock>) -> Result<()>;
    async fn put_content(&self, block: ContentBlock) -> Result<Hash>;
    async fn get_content(&self, hash: &Hash) -> Result<Option<ContentBlock>>;
    async fn get_entry(&self, vk: &VerifyingKey, name: &[u8]) -> Result<Option<SignedKvEntry>>;
    async fn iter(&self, filter: Filter) -> Result<BoxStream<'_, Result<SignedKvEntry>>>;
    async fn subscribe(&self, filter: Filter, replay: Replay) -> Result<BoxStream<'_, Result<Event>>>;
    async fn delete_expired(&self, now: u64) -> Result<usize>;
    async fn gc_blobs(&self) -> Result<usize>;
    async fn current_cursor(&self) -> Result<Cursor>;
}
```

Filter expression:

```rust
pub enum Filter {
    Specific(VerifyingKey, Bytes),     // exact entry
    Keyspace(VerifyingKey),             // all entries by this writer
    Namespace(Bytes),                    // all entries with this exact name
    NamePrefix(Bytes),                   // all entries with name starting with prefix
    Union(Vec<Filter>),                  // OR composition
}
```

Subscription replay mode:

```rust
pub enum Replay {
    None,                  // only future events
    All,                   // all historical matching, then live
    Since(Cursor),         // events with sequence > cursor, then live
}
```

Cursor:

```rust
pub struct Cursor(u64);    // opaque to consumers; backends maintain a monotonic per-store sequence
```

Event delivered on a subscription stream:

```rust
pub enum Event {
    Inserted(SignedKvEntry),
    Replaced { old: SignedKvEntry, new: SignedKvEntry },
    Expired(SignedKvEntry),
    BlobAdded(Hash),
    BlobRemoved(Hash),
}
```

### Concurrency contract

- All trait methods take `&self`; backends manage their own internal locking.
- Multiple concurrent readers are always allowed.
- Writes are serialized per backend (one transaction at a time).
- Read-after-write consistency: a write that returned `Ok` is visible to subsequent reads on the same handle.
- Subscription delivery preserves serialization order: subscribers see events in the order writes committed.

### Atomicity

- **Per-event atomic insert.** A single `store.insert(entry, blob)` is a transaction: the blob (if provided) and the KV row succeed together or both fail. No orphan-blob cleanup is needed because no partial writes are committed. Backends: SQLite trivially supports; IndexedDB supports cross-object-store transactions; memory is naturally atomic.
- **No batch atomicity.** There is no `insert_many` transaction. The KV entry is the atomic unit. Application-layer atomicity (e.g., "these multiple operations go together") is encoded by packaging multiple operations into one ContentBlock, signed once, referenced by one KV entry. The store never tracks batches.
- **Lazy dangling references.** A KV entry whose `value_hash` points to a not-yet-stored ContentBlock is accepted. Reads via `store.get_content(hash)` return `Ok(None)` until the blob arrives. This avoids head-of-line blocking during sync — replication is naturally async and parallel, and missing-blob windows self-heal as soon as any peer ships the referenced content.

### Error type

```rust
pub type Result<T> = std::result::Result<T, Error>;

pub enum Error {
    Backend(String),       // wrapped backend-specific failures (rusqlite errors, IndexedDB DOM exceptions)
    SignatureInvalid,       // SignatureVerifier rejected the entry
    Stale,                  // write rejected because an existing entry has equal or higher priority
    NotFound,               // read returned no result
    Corrupt,                // internal invariant violated (signature unexpectedly fails on read; malformed ContentBlock)
    Closed,                 // operation on a closed store handle
}
```

### Backends

Three crates implement the `Store` trait:

#### `sunset-store-memory`

In-memory `BTreeMap<(VerifyingKey, Name), SignedKvEntry>` for the KV index, `HashMap<Hash, ContentBlock>` for content. Wrapped in `tokio::sync::Mutex` (or a single-threaded `RefCell` on WASM where appropriate). Used for tests, ephemeral sessions, and "remember me unchecked" web sessions.

#### `sunset-store-indexeddb` (WASM-only)

Two object stores in a single IndexedDB database:

- `entries` — keyed by `(verifying_key, name)` (postcard-encoded composite key). Indexed by `name` for namespace queries.
- `blobs` — keyed by `hash` (blake3 bytes).

Cross-object-store transactions for atomic insert. `wasm-bindgen-futures` adapters bridge IndexedDB's callback-based API to async Rust. Quota handling and storage estimation are implementation concerns documented in this backend's own implementation notes.

#### `sunset-store-fs`

SQLite for the KV index (schema below), filesystem for content blobs. Glued with `tokio::task::spawn_blocking` since `rusqlite` is sync.

KV index schema (illustrative; the exact schema is an implementation detail of this backend, not a wire format):

```sql
CREATE TABLE entries (
    sequence       INTEGER PRIMARY KEY AUTOINCREMENT,
    verifying_key  BLOB NOT NULL,
    name           BLOB NOT NULL,
    value_hash     BLOB NOT NULL,
    priority       INTEGER NOT NULL,
    expires_at     INTEGER,
    signature      BLOB NOT NULL,
    UNIQUE(verifying_key, name)
);

CREATE INDEX idx_entries_name       ON entries(name);             -- supports both exact-match (Namespace) and prefix queries via LIKE / range
CREATE INDEX idx_entries_expires_at ON entries(expires_at) WHERE expires_at IS NOT NULL;
```

`sequence` provides the cursor monotonic ordering. Content blobs live on disk under a sharded directory (`content/ab/cdef...` style) for filesystem performance.

### Conformance testing

A shared integration-test suite, exposed as a public test helper from `sunset-store`, that any backend implementation can run against. Verifies LWW semantics, GC correctness, subscription ordering, atomicity, error mapping, and cursor-based resume. Backends that pass the suite are interchangeable from a callers' perspective.

### Items deferred to implementation work

- Concrete IndexedDB schema versioning and quota handling.
- Sweep cadence and incremental sweep design for `gc_blobs`.
- Filesystem blob layout: shard depth, fsync policy, optional compression.
- Migration story when the postcard schema needs to evolve (forklift; new ContentBlock format → new hashes → fresh content).
- Maximum-size limits for KV values, ContentBlock data, names.
- Concurrency-control optimizations for `sunset-store-fs` under high write contention (WAL settings, batch commit windows).

### Implementation note: `sunset-store-indexeddb` durability is best-effort

`sunset-store-indexeddb` (added 2026-05-05) maintains an in-memory mirror of the IndexedDB-backed object stores, and `insert` / `put_content` return `Ok(())` after the in-memory mirror is updated — the IDB transaction commit is **not awaited** before the call returns. Browsers drain pending IDB transactions during the page-unload that follows `location.reload()` (and during normal navigation), so persistence across a reload is reliable in practice and is verified by `web/e2e/indexeddb_persistence.spec.js`. A genuine browser/tab crash between an `insert` returning and the IDB commit can lose the entry; the CRDT contract still holds — sync re-fetches lost entries from peers — but a single-relay user could perceive a recently-typed message vanishing. This is an explicit performance-vs-strict-durability tradeoff: pinning a macrotask hop on every insert (presence heartbeats, voice presence, membership, message receipts) was wide enough to drop voice frames during peer-connection setup. Future work to recover stricter durability without the latency cost would either need a batched/coalesced commit pump or platform support for synchronous IDB writes (not currently available).

## sunset-sync

### Purpose and shape

`sunset-sync` is a Rust crate, parallel to `sunset-store`. It provides peer-to-peer replication of `sunset-store` data over a pluggable transport. It compiles to native and to WebAssembly. It is reusable independently of chat — any application built on `sunset-store` can use `sunset-sync` to share data between peers.

### Transport trait

`sunset-sync` defines the `Transport` trait that hosts implement (browser WebRTC, native `webrtc-rs`, future udpp). Trait surface:

```rust
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
    async fn send_unreliable(&self, bytes: Bytes) -> Result<()>;        // for future voice support
    async fn recv_unreliable(&self) -> Result<Bytes>;
    fn peer_id(&self) -> PeerId;
    async fn close(self) -> Result<()>;
}
```

`sunset-sync` itself only uses the reliable channel. The unreliable channel is exposed in the trait so that voice (in `sunset-core`) can share the same Transport implementation.

### Top-level type

```rust
pub struct SyncEngine<S: Store, T: Transport> {
    store:     Arc<S>,
    transport: T,
    config:    SyncConfig,
    /* internal: peer table, in-memory subscription index, trust set */
}

impl<S: Store, T: Transport> SyncEngine<S, T> {
    pub fn new(store: Arc<S>, transport: T, config: SyncConfig) -> Self;
    pub async fn add_peer(&self, addr: PeerAddr) -> Result<()>;
    pub async fn publish_subscription(&self, filter: Filter, ttl: Duration) -> Result<()>;
    pub async fn set_trust(&self, trust: TrustSet) -> Result<()>;
    pub async fn run(&self) -> Result<()>;
}
```

`publish_subscription` is the user-facing API to declare what events you want from peers. Internally it writes a KV entry under `(local_pubkey, "_sunset-sync/subscribe")` whose value is a postcard-encoded `Filter`. The entry is signed by the local key, so it propagates with integrity through the same replication path as everything else.

### Reserved names

`sunset-sync` reserves a small set of `name` prefixes for its protocol metadata:

- `_sunset-sync/subscribe` — subscription filter entries.
- `_sunset-sync/peer-health` — optional liveness/health summaries (not required for v1).
- (Additional reserved names defined as the implementation evolves; documented in this section as they are added.)

The `_sunset-sync/` prefix is reserved by **convention**: application-layer code (sunset-core, downstream consumers) does not write under names with this prefix, so that sunset-sync's interpretation of those entries isn't ambiguous. The convention isn't enforced by the store — the store just verifies signatures, and any peer with a valid signing key could in principle sign an entry under any name. A peer that publishes a malformed or malicious entry under a reserved name is treated like any other peer that publishes garbage: structural verification still passes if the signature is valid, but sunset-sync's parsing of the entry's value either succeeds (interpreting the value as a `Filter`, etc.) or fails gracefully. Defense against deliberately hostile values (e.g., a `Filter::All` "I want everything" subscription) is a separate concern handled by the trust filter and by per-peer rate-limiting policy.

### Subscription via store

A peer's subscription filter is data, not protocol state. To express interest in a set of events, a peer publishes a signed entry:

```text
KV entry:
  verifying_key = peer's local verifying key
  name          = "_sunset-sync/subscribe"
  value_hash    = blake3 of ContentBlock containing postcard(Filter)
  priority      = unix timestamp of declaration
  expires_at    = priority + ttl
  signature     = signed by local key
```

The entry replicates to other peers via the normal sync path. When a peer's subscription is updated (new filter, refreshed TTL), it writes a new entry with a higher priority — LWW automatically supersedes the old one.

Dead peers' subscription entries simply expire. There is no need for explicit unsubscribe or "cleanup" logic — TTL handles it.

### Wire-protocol message types

All messages serialized with postcard, sent over the Transport reliable channel:

```rust
pub enum SyncMessage {
    Hello          { protocol_version: u32, peer_id: PeerId },
    EventDelivery  { entries: Vec<SignedKvEntry>, blobs: Vec<ContentBlock> },
    BlobRequest    { hash: Hash },
    BlobResponse   { block: ContentBlock },
    DigestExchange { filter: Filter, range: DigestRange, bloom: Bytes },
    Fetch          { entries: Vec<(VerifyingKey, Bytes)> },
    Goodbye        {},
}
```

Notably absent: there is no `SubscribeRequest`. Subscriptions are KV entries that propagate via `EventDelivery` like any other event.

### Bootstrap

When a Transport connection between two peers is established:

1. Both sides exchange `Hello { protocol_version, peer_id }` and verify protocol-version compatibility.
2. Both sides initiate a `DigestExchange` with `filter = Filter::Namespace("_sunset-sync/subscribe")`. Each side sends a bloom over `(verifying_key, name, priority)` triples in their store matching that filter.
3. Each side compares the received bloom to its own data; for tuples present locally but absent in the remote bloom, that means the remote is missing those entries → schedule them for `EventDelivery`. For tuples absent locally but present in the remote bloom (false-negative-free side of the bloom is what matters), we ignore — the remote has data we lack and they will push it on their side of the exchange, or we explicitly `Fetch`.
4. After the digest exchange settles, each side knows the other's subscription filters. Push flow takes over for new events.
5. For catch-up on application data (chat), the peer issues additional `DigestExchange` requests over its application-level subscription filters and `Fetch`es gaps as needed.

A fresh peer with an empty store performs the same handshake; the bloom on its side is empty, so it receives every entry the remote has matching the bootstrap filter, and similarly for its application filters.

### Push flow

`SyncEngine` opens a long-lived local subscription on the store with `Filter` covering everything its peers are interested in (the union of all known peer subscriptions). On every event from this stream:

1. Look up which currently-connected peers have a matching subscription in the in-memory peer-subscription index.
2. For each match, send `EventDelivery { entries: [event], blobs: [referenced ContentBlock if available] }` over that peer's connection.
3. If the referenced blob isn't available locally, the entry is shipped without it; the peer will `BlobRequest` if it doesn't already have it.

The receiver, on receiving `EventDelivery`:

1. For each entry, applies the trust filter (drop if `verifying_key` not on the trust list).
2. Calls `store.insert(entry, blob_if_present)`. The store's `SignatureVerifier` validates the signature; LWW and atomicity rules apply normally.
3. The store emits the inserted event on local subscriptions, including the engine's own pump-loop (which then forwards to other connected peers whose subscriptions match), achieving transitive delivery.

### Pull / catch-up flow

Catch-up is a sequence of `DigestExchange` + `Fetch` rounds:

1. A issues `DigestExchange { filter, range, bloom }` to B.
2. B compares the bloom to its own data within `range` matching `filter`. For any (vk, name) tuple B has locally but the bloom doesn't contain (with the bloom's known FPR), B prepares to push it.
3. B sends `EventDelivery` for each missing entry plus any blobs it has and is willing to ship.
4. A inserts received entries; for any whose `value_hash` doesn't have a local blob, A issues `BlobRequest` to whichever connected peer is most likely to have it.
5. Repeat for additional ranges if the bloom-exchange is partitioned.

### Anti-entropy

Periodically, every connected peer pair runs `DigestExchange` over each active subscription filter. The cadence, bloom parameters, and range partitioning are subsystem-implementation details. The mechanism is the same as in catch-up.

### Trust filter

`SyncEngine` holds an in-memory `TrustSet` describing which `verifying_key` values it accepts events from. The set is supplied by the host through `set_trust(...)` and may be updated at any time. How the host obtains and stores the trust set is **not** a sunset-sync concern — typical patterns include "load it from a KV entry signed by the user's master key" (the identity subsystem will define the schema for that entry) or "compute it from local config." Sunset-sync simply consumes whatever set is currently in effect.

Hosts that want trust changes to flow automatically will typically open their own local store subscription on the trust-set entry and call `set_trust` whenever it changes. Sunset-sync does not do this on its own because it doesn't know the schema of the trust entry; that's identity-subsystem territory.

On every event arriving from a peer (before `store.insert` is called), `SyncEngine` checks `event.verifying_key ∈ trust_set`. If not present, the event is silently dropped. The trust set is **never serialized to peers** — it remains a private local construct, so a peer never has to advertise to a relay whom it trusts.

For peers whose trust policy is "open" (accept events from anyone — typical for a chat client in an open room), `TrustSet` is `TrustSet::All` and the check is a no-op.

### Multi-relay support

The `SyncEngine` supports multiple simultaneous `Transport` connections (multiple relays plus direct peer connections). On insert, the engine pushes the event to every connected peer whose subscription matches; receivers dedupe naturally because LWW yields `Stale` for re-deliveries of the same entry. On read / catch-up, the engine prefers the lowest-latency available source.

Failover when a relay disconnects: existing pushes to other connections continue uninterrupted; reconnect logic re-establishes the lost connection in the background.

### Errors

`sunset-sync` has its own error type `sunset_sync::Error` that wraps `sunset_store::Error`, transport errors, decode errors, and protocol errors. A `pub type Result<T>` alias accompanies it.

### Items deferred to subsystem-implementation work

- Specific bloom-filter parameters (size per entry, false-positive rate target).
- `DigestRange` partitioning strategy (hash-prefix buckets vs sequence-number ranges vs hybrid).
- Anti-entropy cadence and adaptive triggering.
- Backpressure: per-connection windows, message rate-limiting, store-side write throttling when the inbound queue is full.
- Reconnect / retry policy for dropped Transport connections.
- Connection-establishment ordering when many `add_peer` calls happen in quick succession.
- Reserved-name registry: full final list, naming convention rules, enforcement mechanism in the store.
- Catch-up pagination size for huge stores.
- Concrete schema for the trust-set KV entry (defined jointly with the identity subsystem).
- Protocol versioning policy: when does `protocol_version` change, how is incompatibility handled.

## Cross-crate concerns

### Workspace placement

```
crates/
├── sunset-store/                  # this spec, store half
├── sunset-store-memory/           # backend
├── sunset-store-indexeddb/        # backend (wasm-only)
├── sunset-store-fs/               # backend (native-only)
├── sunset-sync/                   # this spec, sync half (defines Transport trait)
└── sunset-core/                   # depends on sunset-store + sunset-sync; adds chat semantics
```

The architecture spec's repo layout shifts `sunset-sync` into the workspace alongside the existing crates. This is the only repo-layout change.

### Dependency graph

- `sunset-store` depends on: `blake3`, `postcard`, `serde`, `futures`, `async-trait`.
- Backend crates depend on `sunset-store` plus their platform-specific primitives.
- `sunset-sync` depends on: `sunset-store`, `postcard`, `serde`, `futures`, `async-trait`, `bloomfilter` (or similar).
- `sunset-core` depends on: `sunset-store`, `sunset-sync`, the chosen backend(s) for whatever host it's compiling for, plus chat-specific deps.

### Conformance testing

Both crates expose shared test helpers (under a `test-helpers` feature flag) that any consumer can use to drive end-to-end scenarios:

- `sunset-store::test_helpers::run_conformance_suite(store)` — runs the full conformance suite against any `Store` impl.
- `sunset-sync::test_helpers::two_peer_scenario(...)` — runs a multi-peer scenario with a configurable transport (typically an in-memory loopback transport) to verify replication invariants.

These helpers are also what the Playwright integration tests in the architecture-spec testing section ultimately exercise indirectly — Playwright drives real browsers running the web client, which in turn uses these crates against IndexedDB and a real WebRTC transport.

## References

- Parent: [`2026-04-25-sunset-chat-architecture-design.md`](2026-04-25-sunset-chat-architecture-design.md).
- Inspiration: [`baybridge`](https://github.com/nicolaschan/baybridge) (CRDT KV + content-addressed model), [`insanity`](https://github.com/nicolaschan/insanity) (store-mediated peer rendezvous, room-name-as-shared-secret).
- [Postcard](https://docs.rs/postcard) — the chosen canonical serialization format.
- [BLAKE3](https://github.com/BLAKE3-team/BLAKE3) — the hash function for content addressing.
