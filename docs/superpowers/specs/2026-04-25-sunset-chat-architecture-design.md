# sunset.chat — System Architecture Design

- **Date:** 2026-04-25
- **Status:** Approved (architecture-level)
- **Scope:** This is the *architecture spec* — the north-star document that subsystem specs reference. It defines component boundaries, cross-cutting concerns, and the things every subsystem must agree on. It deliberately does **not** specify implementation details for any subsystem; those live in their own follow-up specs.

## Purpose and audience

This document is the canonical architectural reference for sunset.chat. Subsystem specs (identity, crypto, sync, storage, voice, etc.) reference and refine the decisions here without contradicting them. Implementation work follows subsystem specs, not this one.

When this spec contradicts a subsystem spec, this spec wins; subsystem specs are expected to either match it or to amend it via an explicit revision marked at the top of this file.

## Overview

sunset.chat is a peer-to-peer end-to-end encrypted chat application with first-class voice, multiple client surfaces (web, native TUI, Minecraft mod), an optional federated identity layer, and optional always-on relays for offline message caching and connection signaling. All clients and relays run the same Rust core compiled to either WebAssembly or a native binary.

Reliable communication (chat ops, member-list ops, identity delegations, handshakes, presence) routes through a content-addressed CRDT store. Real-time voice rides a separate unreliable transport, direct peer-to-peer when possible and relay-forwarded when not. Both channels are end-to-end encrypted; nothing is ever sent in plaintext.

The store is inspired by [`baybridge`](https://github.com/nicolaschan/baybridge); the room-name-as-shared-secret model is inspired by [`insanity`](https://github.com/nicolaschan/insanity); the future custom transport draws inspiration from [`udpp`](https://github.com/nicolaschan/udpp). All three are reimplemented from scratch within sunset.chat with cleaner abstractions; none are pulled in as dependencies.

## Goals and non-goals

### Goals (in scope for v1)

- E2E encrypted multi-party chat, with rooms supporting either an open or invite-only admission policy.
- Federated identity layer: handles in the form `alice@example.com`, ACME-style leases on delegated identity keys, self-hostable trust servers.
- Ephemeral identities by default; opt-in to a federated handle.
- Multiple client surfaces sharing one Rust core: web (Gleam, runs from static files on GitHub Pages), native TUI, Minecraft mod (Java loading the Rust core compiled to WebAssembly).
- Optional relay servers (Docker, same Rust core) for offline message caching and connection signaling, including voice forwarding for peers behind restrictive NAT.
- A baybridge-style store as a separate, message-agnostic component: signed CRDT KV with priority-based last-write-wins; content-addressed blob store with reference-walked GC; subset-aware replication; clean pruning without tombstones.
- Subset replication: peers replicate only the data they care about.
- Voice / real-time as a first-class transport concern (architecturally bound; codec/jitter/mixing detail deferred to a voice subsystem spec).
- Multi-relay support: clients accept several relay URLs and use them in parallel for redundancy.
- Sync dashboard exposed by every host that bears a store, surfacing peer connections, replication progress, GC state, and similar.
- End-to-end Playwright integration testing across multiple browser instances and a real relay.
- Cryptographic ground-truth principle: record what actually happened (signed events from each actor); UI defaults are ergonomic, but full provenance is always inspectable.

### Non-goals (explicitly out of scope for v1)

- Metadata privacy from relays. Relays see who is in which room, when peers are online, message timing and size. Standard E2E messenger threat model.
- Sender anonymity / Tor-grade unlinkability.
- Deniable authentication. Messages are non-repudiable by design.
- Federation with non-sunset chat protocols (Matrix, XMPP, IRC).
- Built-in spam/abuse moderation tooling beyond per-room admin controls.
- Cryptographic transparency log for the trust server (the baybridge-style store provides a built-in audit trail with built-in pruning, but no Merkle/append-only proof).

### Architectural constraints

- The web client must run from static files on GitHub Pages with no backend requirement.
- The protocol engine is Rust and compiles cleanly to both WebAssembly and native binaries.
- All artifacts (Rust crates, Gleam web app, Java mod, Docker relay image) are built via a single nix flake.
- The same business logic runs identically across web, MC mod, TUI, and relay. Glue layers in JS and Java are absolutely minimal — only what is needed to plug WebAssembly into platform host services.

## System architecture

### Compilation paths

There are two compilation paths from the Rust core, and both serve the same protocol logic.

```
                          sunset-core
                         (Rust crate)
                              │
                ┌─────────────┴─────────────┐
                │                           │
              WASM                        native
        (.wasm artifact)            (.rlib / binary)
                │                           │
         ┌──────┴──────┐              ┌─────┴─────┐
         │             │              │           │
        web         MC mod           TUI        relay
       Gleam         Java          (binary)    (Docker)
```

WASM hosts (web client, MC mod) load a single `.wasm` artifact and provide host services through wasm-bindgen-style imports. Native hosts (TUI client, relay) link `sunset-core` as a Rust crate directly and provide host services through native Rust APIs.

### Layering inside `sunset-core`

```
┌──────────────────────────────────────────────────────────┐
│ Application layer                                        │
│   chat semantics · room state · voice signaling ·        │
│   identity (federated handle resolution + delegation)    │
├──────────────────────────────────────────────────────────┤
│ Sync layer                                               │
│   subset replication · interest-set gossip ·             │
│   peer-to-peer + relay-cached delivery for offline peers │
├──────────────────────────────────────────────────────────┤
│ Store layer (`sunset-store`, separate crate)             │
│   CRDT KV (signed events, last-write-wins by priority,   │
│   TTL pruning, no tombstones) +                          │
│   content-addressed blob store (blake3, DAG references)  │
│   over a pluggable storage-backend trait                 │
├──────────────────────────────────────────────────────────┤
│ Crypto layer                                             │
│   Noise session establishment with hybrid PQC handshake; │
│   hybrid (classical + post-quantum) signing for          │
│   delegations and message ops; blake3 for content        │
│   addressing                                             │
├──────────────────────────────────────────────────────────┤
│ Transport layer (trait surface)                          │
│   WebRTC (v1, all hosts) → udpp (future)                 │
│   reliable + unreliable channels                         │
└──────────────────────────────────────────────────────────┘
```

### Host-services trait

All hosts implement a `HostServices` trait that the core depends on at construction time. Trait surface includes transport, storage backend, time, and RNG. The actual cryptography (Noise, signing, hashing) is performed by `sunset-core` itself in pure Rust — the host only needs to provide platform-appropriate primitives like a secure RNG and current-time accessor. Native hosts implement the trait using tokio, `sunset-store-fs`, system time, and `getrandom`. WASM hosts implement it via WASM imports: in the browser, the imports route to WebRTC, IndexedDB, `crypto.getRandomValues()`, and `performance.now()`; in the JVM, they route to Java NIO networking, the JVM filesystem, `SecureRandom`, and `System.currentTimeMillis()`.

## Components

### v1 deliverables

1. **`sunset-store`** — Rust crate. Defines the storage trait surface and CRDT KV semantics. Independently usable; not chat-specific.
2. **`sunset-store-memory`** — Rust crate. In-memory backend implementation of `sunset-store`. Used for ephemeral sessions and tests.
3. **`sunset-store-indexeddb`** — Rust crate (WebAssembly-only). IndexedDB backend implementation. Used by the web client when "remember me" is enabled.
4. **`sunset-store-fs`** — Rust crate. Filesystem + SQLite backend. SQLite serves as the indexed KV table; the filesystem stores content blobs at high throughput. Used by the relay and TUI defaults.
5. **`sunset-core`** — Rust crate. The protocol engine. Depends on `sunset-store` traits but not on any specific backend; depends on the transport trait but not on any specific transport. Compiles to native and to WebAssembly.
6. **`sunset-tui`** — Rust binary. Native TUI client. Links `sunset-core` directly. Provides native host services using tokio and `sunset-store-fs`. UI built with `ratatui` (non-architectural).
7. **`sunset-relay`** — Rust binary, distributed as a Docker image. Links `sunset-core` directly. Provides native host services using tokio and `sunset-store-fs`. Operates with a relay participation policy (see §Transport, sync, relay role).
8. **`sunset-web`** — Gleam project. Web client. Imports the same `.wasm` artifact the MC mod loads. Provides browser host services (WebRTC, IndexedDB via `sunset-store-indexeddb`, WebCrypto, `performance.now()`). UI is Gleam. JS glue exists only where Gleam cannot bind a browser API directly.
9. **`sunset-mod`** — Java/Gradle project. Minecraft mod. Bundles the same `.wasm` artifact the web client uses, loaded via a WebAssembly runtime in the JVM. Java code is minimal: WASM runtime integration, Minecraft chat-event hooks (intercept in-game chat → forward to room; render incoming messages back to the player), and Java host-services (Java NIO net, filesystem).

### Adjacent components (not v1 deliverables)

- **`sunset-trust`** — federated trust server. Publishes a master key at a well-known URL, signs delegations for `alice@example.com` handles, runs the ACME-style renewal flow. Self-hostable; could itself be a small Rust binary using `sunset-core`'s identity primitives. Has its own subsystem spec; separate from chat-protocol implementation.

### Component dependencies

- Every client (web, TUI, mod) and the relay depend on `sunset-core` plus one or more storage-backend crates. Hosts typically link a single backend at build time, but a host that wants runtime-selectable backends (e.g., a TUI that supports both in-memory and filesystem modes) is free to depend on multiple.
- `sunset-core` depends on `sunset-store` interfaces only — never on a specific backend.
- Backend implementations depend on `sunset-store` and on platform-appropriate primitives (web-sys for IndexedDB, rusqlite + tokio for filesystem, etc.).
- `sunset-trust` consumes a subset of `sunset-core`'s identity primitives; it does not depend on the chat-protocol layers.

### Glue minimality (operating principle)

JS in the web client and Java in the MC mod are absolutely minimal. Their job is exclusively to plug the WebAssembly module into the host's platform services. Chat logic, CRDT logic, crypto, sync — always Rust. Rust and Gleam are the core languages of the project; everything else is glue.

## Identity model

### Two-tier hybrid identity

- **Ephemeral identity (default).** When a peer starts a session with no prior state, it generates a fresh keypair locally. Display name is user-supplied. Closing the app loses the identity. Opt-in "remember me" persists the keypair in local storage on that device.
- **Federated identity (opt-in).** Handles are formatted as `alice@example.com`. Identity resolves through a three-tier signing chain anchored at a trust server.

### Federated signing chain

1. **Master key** held by the trust server (e.g., `example.com`). Published at a well-known URL, fetched over TLS, and cached locally on first contact. Master keys do not have a built-in expiry; rotation is a trust-server subsystem concern.
2. **Delegated identity key** owned by Alice. Lives in the store as a signed KV entry: `(master_key, "alice") -> {delegation_pubkey, expires_at, lease_metadata, ...}`. ACME-style — Alice must re-prove control to her trust server before `expires_at` to renew. Renewal = a new event with higher priority and pushed-out `expires_at`; the store auto-prunes the previous entry.
3. **Ephemeral session keys** owned by Alice's device, signed by Alice's delegated key. Used for Noise handshakes and per-message signing.

### Verification path for an incoming op claiming an identity

1. Verify the op's signature against the op's signing key.
2. Resolve the signing key → its delegation chain (lookup of `(master_key, "alice")` in the store).
3. Validate the delegation has not expired.
4. Validate the delegation is signed by the master key.
5. Validate the master key matches the one published at the trust server's well-known URL (cached locally).

### Items deferred to the identity subsystem spec

- Multi-device model (per-device delegation under one handle, vs. one delegation transferred between devices, vs. a sub-identity hierarchy).
- Master-key bootstrap trust anchor (TLS-only / TOFU on key fingerprint / DNSSEC / opt-in key transparency).
- Alice's authentication flow to her trust server when requesting a delegation.
- Default lease duration and renewal cadence.
- Concrete protocol for the `sunset-trust` server.

## Room model

### Room ID

A room ID is the hash of an initial signed config block. Content-addressing the room ID makes the ID itself a commitment to admin keys, name, and admission policy. There is no central room registry.

### Admission policy

Every room declares its admission policy at creation, baked into the initial config. There are two policies:

- **Open.** Any identity (federated or ephemeral) is admitted. Identity is purely a display label; nothing cryptographic gates participation. Open rooms remain useful with federated identities for handle display and accountability.
- **Invite-only.** A cryptographic member list is maintained as signed CRDT ops in the store. Membership is enforced *at every peer locally* — every peer validates each incoming op against the current member list before applying. There is no central join point. Member entries are federated identities only (`alice@example.com`); raw-pubkey-fingerprint admission is intentionally not supported because federated handles bake in lease-based key rotation.

Admin authority for managing the member list is signed by one or more keys baked into the initial config. The exact admin model — single owner, multi-admin, or a richer role hierarchy — is deferred to a room subsystem spec.

### Joining a room

Joining is decentralized. To join a room, a peer:

1. Obtains the room name (low-entropy "general" or high-entropy near-UUID, both are valid; see §Wire envelope).
2. Computes the room fingerprint from the room name via Argon2id.
3. Subscribes its replication interest to the room fingerprint.
4. Pulls existing room state and presence entries from any peer or relay willing to serve them.
5. Validates the chain (admission policy, member list if invite-only, identity delegations).
6. Begins participating: publishes its own presence entry, sends Noise handshake events, replicates room ops as they arrive.

### Items deferred to the room subsystem spec

- Admin model: single owner, multi-admin, or role hierarchy.
- Member-list op semantics, including conflict resolution when two admins concurrently add/remove the same member.
- Ownership transfer protocol.
- Room-name change and its interaction with the PKDF (typically prohibited; effectively a new room).

## Store layer (`sunset-store`)

The store is a separate, message-agnostic crate inspired by baybridge but reimplemented with cleaner abstractions and a pluggable storage backend trait. Chat is a client of the store, not the other way around. The store can be used for any signed-CRDT-with-content-addressing workload.

### CRDT KV store

- Entry: `(verifying_key, name) -> {value, priority, expires_at, signature}`.
- `priority` defaults to the unix timestamp at write time but is set by the caller.
- Last-write-wins on `(verifying_key, name)` by priority.
- Optional `expires_at` for TTL-based pruning.
- A new event with higher priority for the same key **replaces and deletes** the old entry. There are no tombstones.
- Indexed for two query patterns: by **keyspace** (all names a writer publishes) and by **namespace** (all writers using a name). Plus prefix queries on names (e.g., `room_R/*`).

### Content-addressed blob store

- `ContentBlock { data: bytes, references: [hash] }`, keyed by `blake3(serialized)`.
- References form a DAG between blobs.
- Naturally deduplicated: identical blobs hash to the same key.
- GC walks the reachable set from KV pointers; unreferenced blobs are reclaimed.

### Storage backend trait

`sunset-store` defines a small `StorageBackend` trait surface:

- KV: get / put / delete / range-query (by keyspace, by namespace, by name prefix).
- Content: get / put / has / iterate-references.

Backend implementations live in their own crates so that hosts only depend on what they need:

- `sunset-store-memory` — ephemeral, useful for tests and "remember me unchecked" web sessions.
- `sunset-store-indexeddb` — WebAssembly-only; used by the web client when persistence is enabled.
- `sunset-store-fs` — SQLite for the KV index, filesystem for content blobs. Used by the relay and TUI default.

The store layer's job is *protocol semantics*: signatures, priorities, GC, subset queries. The backend layer's job is *bytes on/off the disk* with platform-appropriate primitives. The split is enforced by the trait boundary.

### Subset replication

Peers express their interests as filter sets:

- All entries in keyspace `K` (everything a particular writer publishes).
- All entries in namespace `N` or with name prefix `P` (everything for a room, everything for a handle).
- Specific content hashes and their transitive references.

Peers gossip their interest sets to neighbors and relays; the replication protocol pushes only matching events. Bloom filters or compressed range encodings keep announcements small. Specific encoding is a sync subsystem-spec choice.

### Garbage collection

- **KV supersession** is built in: higher-priority writes purge lower-priority entries on insert. This is what gives the store its bounded growth — old data physically disappears as it is overwritten.
- **TTL expiry**: a background sweep deletes events past `expires_at`.
- **Content blob GC**: mark-and-sweep walks all KV-reachable hashes through `ContentBlock.references`; unreachable blobs are reclaimed. Sweep cadence and incremental design are storage-subsystem-spec concerns.

### Why the store deserves its own crate

- Chat, identity delegations, room state, voice signaling, presence, and read-receipts are all clients of the same KV + blob primitives. Decoupling makes those clients easier to reason about.
- Replication is subset-aware by default — no peer ever has to download everything.
- Storage does not grow unbounded. No tombstones. Expired or superseded data physically disappears.
- The same store API runs against memory, IndexedDB, sqlite+FS — the persistence policy is a host concern, not a protocol concern.
- The store can be reused for non-chat applications.

### Items deferred to storage subsystem specs

- Concrete SQLite schema for the KV index.
- IndexedDB transactional layout, quota handling.
- GC scheduling and incremental sweep design.
- Content-blob layout on disk (sharding, fsync policy, compression).

## Wire envelope and encryption

There are two distinct channels and two distinct envelopes.

- **Reliable channel** — chat ops, member-list ops, identity delegations, handshake messages, presence/routing info. Everything reliable routes through the store. The store is the universal rendezvous for reliable communication.
- **Unreliable channel** — voice frames. Direct peer-to-peer when possible, relay-forwarded when not. Always end-to-end encrypted; bypasses the store.

### Outer envelope: nothing in plaintext

Every store event for a room is AEAD-encrypted under a room-key derived from the room name.

- `room_name` → Argon2id (PKDF) → `(room_encryption_key, room_fingerprint)`.
- The store's `name` field for the event is the **`room_fingerprint`**, never the literal room name. An eavesdropper without the room name cannot identify which room any event belongs to.
- High-entropy room names function as shared passwords. A "private" room is just a room with a high-entropy name; sharing a link to it grants both access and historical read.

### Inner envelope: signed CRDT event

Inside the AEAD ciphertext, the operation payload conceptually looks like:

```text
SignedEvent {
  signing_key: bytes,           // sender's ephemeral session key
  signature:   bytes,           // sig over (payload || context) by signing_key
  context: {
    op_type:    enum,           // message | edit | delete | member-add |
                                //   member-remove | presence | handshake-init |
                                //   read-receipt | ...
    expires_at: option<u64>,
    ...
  },
  payload: bytes,               // op content
}
```

The sender's `signing_key` is their ephemeral session key, which chains via delegation to their handle (federated) or is self-signed (ephemeral). Verification chains as in §Identity model.

### Composition of access barriers

- Outer AEAD proves: "this event came from someone who knows the room name."
- Inner signature proves: "this event was authored by `<identity>`."
- **Open rooms:** outer AEAD is the only access barrier. Inner identity is for attribution and display.
- **Invite-only rooms:** outer AEAD plus a member-list check (sender identity must appear on the current member list, which is itself an encrypted, signed CRDT op).

### Forward secrecy

The room key derived from `Argon2id(room_name)` is static — it gives metadata privacy and a baseline access gate, but no forward secrecy on its own. Forward secrecy is layered: a group session key is established via the Noise handshake (whose handshake messages are themselves encrypted CRDT events through the store), rotates on membership change, and provides PFS for chat content. The exact key schedule (single AEAD with rotating subkeys, vs. nested AEADs, vs. an MLS-shaped tree) is a crypto subsystem-spec choice.

### Handshake and routing through the store

- Each peer publishes a **presence entry** at `(peer_key, room_fingerprint) -> AEAD{connection_info, display_name, ...}` with a short TTL, refreshed periodically while online. Insanity-style.
- Other peers in the room read the fingerprint namespace, find current presence entries, and initiate Noise handshakes. Handshake messages are themselves encrypted CRDT events through the store, so handshakes succeed even when peers are not simultaneously connected.
- Once a direct P2P connection is established, peers can replicate subsets of the store directly between each other (faster than via a relay) — but the protocol is the same; the relay path remains as fallback.
- **Bootstrap:** a peer connects to one or more known relay URLs configured per host, gains store access there, and discovers presence entries from the relay's view of the room.

### Voice envelope (unreliable)

After a Noise handshake completes, voice frames flow over the unreliable transport (WebRTC datachannel in v1; udpp later), AEAD-encrypted under a session key derived during the handshake. Voice frames carry a publisher signature so a relay can forward them but cannot forge or read them. Per-frame vs. per-chunk signing granularity is a voice subsystem-spec choice. Voice never touches the store.

When direct P2P is unavailable (NAT, firewall), a relay forwards voice frames between peers. The relay sees ciphertext and signatures; it cannot decrypt or modify.

## Transport, sync, and relay role

### Transport trait

`sunset-core` defines a `Transport` trait abstracting the P2P connection layer. The trait surface includes:

- Establish connection to a peer given connection info from a presence entry.
- Send reliable bytes (used by the sync layer).
- Send unreliable bytes (used by voice).
- Forward frames (used by the relay role).
- Connection lifecycle events (peer up / down / errored).

Hosts inject a concrete transport at construction time. The web host wires the Gleam-side `Transport` impl that calls into browser WebRTC; native hosts wire a Rust `Transport` impl using `webrtc-rs`.

### v1 transport: WebRTC everywhere

WebRTC is the v1 implementation across all hosts:

- Web client uses the browser's WebRTC API directly.
- Native hosts use `webrtc-rs`.
- Relays speak WebRTC and act as STUN/TURN-style signaling helpers in addition to their store-cache and voice-forwarding roles.

Uniform behavior across all hosts means a single transport stack to debug, well-understood NAT traversal via ICE/STUN/TURN, and battle-tested codepaths.

### Future transport: custom UDP-based protocol

The trait abstraction exists from day one because the transport stack is expected to evolve. The next planned transport is a custom UDP-based protocol, reimplemented from scratch with design inspiration from `udpp` (sessions, AEAD-on-UDP, congestion detection). It will drop in via the same trait surface with no upper-layer rewrite. Voice frames will likely move to it first because the unreliable channel is where the design is strongest.

### Relay role

A relay is a peer with a different participation policy. The protocol is identical to client peers; only configuration differs. Relays have no special trust.

- **Always-online bootstrap.** Clients configure one or more relay URLs. On startup, a client connects to a relay to gain initial store access.
- **Store cache.** A relay subscribes to configured subsets (e.g., "all rooms my users participate in"). It validates, stores, and serves matching CRDT events to peers requesting them. Honors the same TTL/GC semantics as any peer.
- **Connection signaling.** STUN/TURN-style help: address exchange via presence entries, hole-punching coordination, fallback data relay when direct P2P is impossible.
- **Voice forwarding.** When peers cannot establish direct P2P, the relay forwards their voice frames. Frames remain end-to-end encrypted (relay cannot read) and signed (relay cannot forge).
- **Self-hostable.** A user can run their own relay (`docker run sunset-relay`); a TUI client can also act as a relay for its operator's friends behind it.

### Multi-relay support in v1

Clients accept several relay URLs in their configuration and use them in parallel. Because store events are content-addressed, signed CRDT entries, duplicate fetches across relays converge cheaply: whichever relay responds first wins for any given event, and divergent responses are resolved by signature verification and priority comparison. Failover is transparent. Operators can run their own relay alongside community relays for redundancy.

### Sync layer

How CRDT events propagate:

- Each peer announces an **interest set** — a list of filters describing which (keyspace, namespace, prefix) tuples it wants to replicate. Bloom filters or compressed range encodings keep announcements small.
- Neighbors gossip interest sets and exchange events. The replication protocol pushes only events matching the receiver's interests.
- Joining a room is "subscribe to interest `(*, room_fingerprint, *)`" and start syncing from any neighbor or relay that has it.
- Relays' interest sets are explicit operational config (a community relay might serve specific fingerprints; a personal relay might serve everything its owner participates in).

### Discovery

- v1: configured relay URLs serve as the entry point. Once connected, a peer discovers other peers via the room's presence-entry namespace.
- Future: mDNS for LAN, DHT-based relay discovery for relay-less scenarios. Out of scope for v1.

### Items deferred to the sync subsystem spec

- Concrete encoding for interest sets (bloom filters? range encodings? interval trees?).
- Replication wire protocol framing.
- Anti-entropy strategy and gossip topology.
- Catch-up algorithm for newly joining peers with large historical state.

## Voice (architectural placeholder)

Voice is a first-class transport concern at the architecture level. The wire envelope and transport layer are designed to carry it without requiring redesign.

- Voice frames ride the unreliable transport channel.
- Each frame is AEAD-encrypted under a group session key shared by room members and carries a publisher signature.
- Direct P2P preferred; relay-forwarded fallback when the direct path fails.
- Signaling (call setup, join, leave) rides the reliable channel through the store, like any other op type.

The codec choice (likely Opus), jitter buffer design, group voice topology (full mesh vs. SFU vs. hybrid), frame size, and the granularity of signing (per-frame vs. per-chunk) are all deferred to the voice subsystem spec.

## Build, distribution, and repo layout

### Monorepo with a single nix flake

```
sunset/
├── flake.nix              # builds every artifact
├── flake.lock
├── .envrc                 # direnv: use flake
├── Cargo.toml             # cargo workspace root
├── crates/
│   ├── sunset-store/                  # store traits + CRDT semantics
│   ├── sunset-store-memory/           # backend: in-memory
│   ├── sunset-store-indexeddb/        # backend: IndexedDB (wasm-only)
│   ├── sunset-store-fs/               # backend: filesystem + sqlite
│   ├── sunset-core/                   # protocol engine; depends on sunset-store
│   ├── sunset-tui/                    # TUI binary
│   └── sunset-relay/                  # relay binary
├── web/                   # sunset-web (Gleam)
├── mod/                   # sunset-mod (Java + Gradle, bundles core .wasm)
└── docs/
    └── superpowers/specs/
```

### Flake build outputs

- `nix build .#sunset-tui` — native TUI binary (cross-compilable to Linux / macOS / Windows).
- `nix build .#sunset-relay` — native relay binary.
- `nix build .#sunset-relay-docker` — relay OCI image.
- `nix build .#sunset-core-wasm` — the `.wasm` artifact (consumed by web client and Minecraft mod).
- `nix build .#sunset-web` — Gleam build, static files for GitHub Pages deploy.
- `nix build .#sunset-mod` — Minecraft mod jar (bundles the `.wasm`).
- `nix flake check` — all unit and integration tests across Rust crates and the Gleam project.

### Distribution

- **Web client.** GitHub Pages, deployed from the `sunset-web` flake output.
- **TUI.** Prebuilt binaries on GitHub Releases; also `nix run github:user/sunset#sunset-tui`.
- **Relay.** Docker image on a registry (GHCR); also a static binary on GitHub Releases for non-container use.
- **MC mod.** Jar on GitHub Releases; also published to Modrinth/CurseForge if desired.

### CI

- On push: `nix flake check` on Linux and macOS runners. Lint, unit tests, integration tests.
- On tag: build all distribution artifacts and publish them.
- All Playwright integration tests run on every push (see §Observability and testing).

### Items deferred to the build/release subsystem spec

- Specific CI provider and workflow files.
- Signing and provenance for release artifacts.
- Release-cadence policy, semver discipline.
- Cross-compilation toolchains for Windows TUI builds.

## Observability and testing

### Status surface in the protocol layer

`sunset-store` and `sunset-core` expose a programmatic status query that any host can render. The surface includes:

- Connected peers (count, addresses, RTT).
- Per-relay sync state (pulling, caught up, errored).
- Per-room interest sets and replication progress.
- KV entry count, blob count, total bytes (per backend).
- GC state: last sweep, expired pruned, blobs reclaimed.
- Pending outbound events.

This is a non-optional architectural commitment: every host that bears a store renders this status as a dashboard appropriate to its medium.

### Dashboard renderers per host

- **Web client (Gleam):** dashboard view inside the same Gleam app.
- **TUI:** dashboard panel inside the TUI (`ratatui`).
- **Relay:** built-in HTTP admin endpoint serving an embedded HTML dashboard.
- **MC mod:** minimal `/sunset status` slash-command output in chat.

The same protocol-level status drives every host. Sync status is non-trivial in P2P + relay topologies; without visibility, debugging "why didn't my message arrive?" becomes painful.

### Playwright integration testing

End-to-end integration testing in v1 uses Playwright. The harness:

- Spins up multiple browser instances of the web client.
- Spins up a real relay (or several) via Docker.
- Drives multi-peer scenarios end-to-end:
  - Two browsers joining the same room and exchanging messages.
  - Three browsers, one going offline, coming back, catching up via relay.
  - Multi-relay fallback: kill one relay, traffic continues through another.
  - Federated identity flow: register at a trust server, get a delegation, post a message, verify chain on receipt.
  - Invite-only rooms: join attempt without invite fails; with invite succeeds.
  - High-entropy room name privacy: a third browser without the room name sees opaque blobs and cannot identify the room.

Because the web client is a complete exerciser of the WebAssembly core, this test suite also covers the core that the MC mod loads. The native TUI and relay get separate Rust-level integration tests.

### Items deferred to the testing subsystem spec

- Concrete Playwright harness setup, including how relays and browsers are orchestrated in CI.
- Multi-host scenario library and the fixture model for trust servers.
- Coverage policy for cryptographic invariants (property-based tests, fuzzing).

## Trust and verifiability principles

### Core principle

Record what actually happened cryptographically. Surface that to the user. Do not take anything on blind trust. Sensible defaults are fine for ergonomics, but full provenance must be inspectable so users can detect bad actors.

### Concrete implications

- **Timestamps.** A sender's claimed timestamp is one signed data point. Receivers publish **read-receipts** as separate signed events containing the timestamp they observed the message arrive. UI default is the sender's claim; expanding "message details" reveals every receiver's reception timestamp plus delegation chain status. Discrepancies become user-visible.
- **Edits.** Each edit is a signed op referencing the original event hash. UI shows the latest version; expansion reveals the full edit history with each edit's signed timestamp.
- **Deletes.** Delete ops are signed; the original remains cryptographically recorded until GC. UI surfaces "deleted by sender at T" alongside read-receipts of the delete.
- **Membership.** Add/remove ops are signed by an admin. The current member list at any moment is reconstructible from the op DAG. UI surfaces "added by X at T" on hover.
- **Presence.** Presence entries are signed events with TTL — they *are* the cryptographic claim "I was online at time T."
- **Identity.** A handle `alice@example.com` is verifiable through the delegation chain. Default UI shows the display name; expansion reveals the full handle, delegation expiry, and master-key origin.

### Read-receipts as a first-class op type

Read-receipts are not optional metadata; they are a built-in op type in the CRDT. On accepting an event, every receiver publishes a small `ReadReceipt { event_hash, observed_at }` signed by themselves. Subject to the same room encryption + signing rules as everything else.

Privacy: read-receipts in invite-only rooms are visible only to room members, like any other room event. In open rooms, anyone with the room name can see them — consistent with the room-name-as-shared-secret model. Receipt opt-out is a per-user UX preference layered on top, but the protocol always carries the receipt op type.

### UX commitment for every host

Every chat view must expose a "message details" expansion that reveals:

- Sender (handle plus signing-key fingerprint).
- Sender's claimed timestamp.
- All read-receipt timestamps from observed receivers.
- Delegation chain status ("valid until 2026-05-12, signed by `example.com` master key").
- Edit history (if any) with per-edit timestamps.

This is non-negotiable across web, TUI, and Minecraft mod hosts.

## Threat model summary

A consolidation of choices made across the design.

### What the protocol protects against

- **Passive network observers** (without TLS termination): see encrypted ciphertext and routing-key fingerprints. Cannot read content. Cannot determine which room is being communicated about without knowing the room name.
- **Eavesdroppers without a room name**: cannot identify which room any event belongs to (room fingerprint is derived from the room name; literal room names never appear on the wire).
- **Sender identity fraud**: events not signed by an identity in a valid delegation chain are rejected at every peer. Compromise of an ephemeral session key allows forgery only within the unexpired lease window of the parent delegation.
- **Relay tampering**: relays can drop or delay events but cannot modify them (signatures over events would fail). Relays can forward voice frames but cannot read or forge them.
- **Replay attacks**: signed events bind their context (op type, parent ops, timestamp); replays are detectable via priority comparison and CRDT semantics. Voice frames carry a monotonic counter inside the AEAD.
- **Trust-server compromise (limited)**: a compromised trust server can issue fraudulent delegations within its own handle namespace (`alice@example.com` could be hijacked if `example.com` is compromised). The blast radius is bounded to that domain. Cross-domain trust is not affected.

### What the protocol does not protect against

- **Metadata visibility to relays**: relays see who is in which room (by fingerprint), when peers are online, message timing and size.
- **Sender anonymity / unlinkability**: identities (federated or ephemeral) are visible to other room members and to relays.
- **Endpoint compromise**: a compromised device with access to local storage and decrypted session keys is fully exposed.
- **Coercion / non-deniability**: messages are non-repudiable. A recipient can prove what was said to a third party.
- **Spam / abuse at the protocol layer**: protection is per-room (admin moderation) only.
- **Quantum-classical adversaries today**: the hybrid PQC handshake assumes that one of the classical or PQ legs remains secure; if both are simultaneously broken, security falls.

## Deferred to subsystem specs

Each of the items below gets its own brainstorm → spec → plan → implement cycle.

1. **Identity subsystem** — multi-device model, master-key bootstrap trust anchor, Alice's authentication flow to her trust server, default lease duration, `sunset-trust` server protocol.
2. **Crypto subsystem** — exact Noise pattern, hybrid PQC parameters, group-key rotation strategy on membership change, library choices.
3. **Sync subsystem** — interest-set encoding, replication wire protocol, anti-entropy strategy, gossip topology, catch-up algorithm.
4. **Storage subsystems** — SQLite schema for the KV index, IndexedDB transactional layout and quota handling, GC scheduling and incremental sweep, content-blob on-disk layout.
5. **Room subsystem** — admin model (single owner / multi-admin / role hierarchy), member-list op semantics, ownership transfer.
6. **Voice subsystem** — codec selection, jitter buffer, group voice topology (mesh / SFU / hybrid), frame size, signing granularity.
7. **MC mod subsystem** — WASM runtime choice in the JVM (Chicory / Wasmer / GraalVM), Java host-services impl, target Minecraft versions, distribution channels.
8. **Web client subsystem** — Gleam app structure, UI/UX, routing, GitHub Pages deployment specifics.
9. **TUI subsystem** — UI framework (likely `ratatui`), keybindings, layout.
10. **Relay subsystem** — operational config, admin dashboard implementation, rate limiting, admin authentication.
11. **Observability subsystem** — concrete metrics surface, OTel export, log structure.
12. **Testing subsystem** — Playwright harness setup, multi-host scenario library, CI orchestration.
13. **Build / release subsystem** — CI specifics, signed-binary distribution, release cadence.

## References

- [`baybridge`](https://github.com/nicolaschan/baybridge) — design inspiration for the store layer's CRDT KV plus content-addressed blob model. Reimplemented from scratch within sunset.chat.
- [`insanity`](https://github.com/nicolaschan/insanity) — design inspiration for the room-name-as-shared-secret model and store-mediated peer rendezvous. Reimplemented from scratch within sunset.chat.
- [`udpp`](https://github.com/nicolaschan/udpp) — design inspiration for the future custom v2 transport. Reimplemented from scratch within sunset.chat.
- The Noise Protocol Framework (`https://noiseprotocol.org`).
- ML-KEM (FIPS 203) and ML-DSA (FIPS 204) — the families the hybrid PQC suites will be drawn from.
