# sunset-relay (Plan D) — Subsystem design

- **Date:** 2026-04-27
- **Status:** Draft (subsystem-level)
- **Scope:** Native `sunset-relay` binary that runs `sunset-sync` over `sunset-sync-ws-native` + `sunset-noise`, with persistent storage via `sunset-store-fs`. Supports relay-to-relay federation. Distributed as a Nix-built Docker image. Plan D in the web roadmap (A → C → D → E).

## Non-negotiable goals

1. **A single binary, hermetically built.** `nix build .#sunset-relay` produces a statically-linkable native binary. `nix build .#sunset-relay-docker` produces a Docker image via `pkgs.dockerTools.buildLayeredImage`. No language-runtime install on the host required.
2. **Relays are real `sunset-sync` peers.** A relay binds a WebSocket listener (Noise responder) AND optionally opens outbound connections to other configured relays (Noise initiator). The bidirectional flow makes "message inserted at relay A propagates to clients connected at relay B" work end-to-end.
3. **Persistent storage.** `sunset-store-fs` keeps entries + content across restarts. Identity (Ed25519 secret seed) persists at `<data-dir>/identity.key` (mode 0600) so a relay's pubkey is stable across restarts.
4. **Multi-relay integration tests prove the federation works.** Headline test: alice → relay-A → relay-B → bob, all in-process. Failover test: alice + bob connected to both relays, kill one, traffic continues through the other.

## Non-goals (deferred)

- **HTTP admin endpoint** (status / metrics / sync dashboard). Architecture spec calls for one; not in v0.
- **Allowlists / rate limiting / per-room admission.** Open relay per the Plan C decision; defer to Plans 8+.
- **TLS termination at the relay.** v0 listens on plain `ws://`. Operators front with nginx/Caddy/Cloudflare for `wss://` termination. (TLS is independent of the Noise inner layer that already protects payloads.)
- **Multi-relay client-side redundancy logic.** Architecture spec mentions clients accepting several relay URLs in parallel; that's a client-side concern not the relay's. (The relay-to-relay federation in this plan IS the v0 redundancy story.)
- **Encrypted identity-at-rest.** v0 stores the Ed25519 seed in plaintext at `<data-dir>/identity.key`; protect the data directory at the OS level.
- **Reconnection / backoff for federated peer links.** v0 dials each configured peer once at startup; if the peer is down, log the failure and continue. A peer-link supervisor (reconnect-with-backoff) is a small follow-up.
- **Loop suppression for federated propagation.** Two relays subscribed to each other will see each entry twice on the wire (once each direction); the store's LWW + signature semantics dedup at insert time, so traffic is wasted but correctness is maintained. Smarter de-dup (e.g., recently-pushed Bloom filter) is a follow-up.

## Architecture

```
sunset-relay binary
   │
   ├── load Config from TOML file (CLI: --config <path>; env override RUST_LOG)
   ├── load-or-generate Identity at <data_dir>/identity.key
   │     (echo public key + ws://<listen_addr>#x25519=<hex> address to stdout)
   ├── open FsStore at <data_dir>/sqlite + <data_dir>/blobs
   │     (verifier = Arc::new(Ed25519Verifier))
   ├── construct WebSocketRawTransport::listening_on(<listen_addr>)
   ├── wrap with NoiseTransport::new(raw, identity_adapter)
   ├── construct SyncEngine::new(store, transport, config, peer_id, signer = identity)
   ├── publish_subscription(<configured_filter>, ttl_long)
   ├── for each configured federated peer URL:
   │     SyncEngine::add_peer(peer_addr)         (handshake, push/pull starts)
   ├── spawn engine.run() on a tokio task
   └── tokio::signal::ctrl_c() → graceful shutdown (engine close, store flush)
```

### Crate split

`crates/sunset-relay/` — the binary crate AND a small library that exposes a `Relay::new(config) → impl Future<Output = Result<()>>` so integration tests can spin up an in-process relay without subprocess plumbing.

```
crates/sunset-relay/
├── Cargo.toml
├── src/
│   ├── lib.rs              # pub use main, config, errors
│   ├── config.rs           # TOML config parsing + defaults
│   ├── identity.rs         # load_or_generate at path; pretty-print address
│   ├── relay.rs            # the Relay struct: setup + run loop
│   └── main.rs             # CLI flag parsing, calls into lib
└── tests/
    └── multi_relay.rs      # two-relay federation + failover tests
```

### Configuration

Default-everywhere TOML. Example:

```toml
# /etc/sunset-relay.toml — or pass via --config
listen_addr        = "0.0.0.0:8443"            # required
data_dir           = "/var/lib/sunset-relay"   # required
interest_filter    = "all"                     # "all" or future granular shapes
identity_secret    = "auto"                    # "auto" = <data_dir>/identity.key
peers              = []                        # outbound relay URLs (federated)

# Examples of `peers`:
# peers = [
#   "ws://other-relay.example.com:8443#x25519=abc123…",
#   "wss://yet-another.example.com#x25519=def456…",
# ]
```

CLI flags (minimal):

```
sunset-relay [--config <path>]
```

If `--config` is omitted, the relay falls back to defaults: `listen_addr=0.0.0.0:8443`, `data_dir=./data`, `interest_filter=all`, no federated peers. Useful for local development; in production operators will pass `--config`.

Env override:
- `RUST_LOG` — standard tracing-subscriber filter (e.g., `RUST_LOG=sunset_relay=info,sunset_sync=warn`).

### Identity persistence

`<data_dir>/identity.key` is a binary file containing exactly the 32-byte Ed25519 secret seed. File mode `0o600` enforced on creation; if existing file has wider permissions, log a WARN but continue (don't refuse to start — operators may have intentional ACL setups).

On startup the relay prints to stdout the relay's "shareable address":

```
sunset-relay starting
  ed25519: <64-hex>
  x25519:  <64-hex>
  listen:  ws://0.0.0.0:8443
  address: ws://0.0.0.0:8443#x25519=<64-hex>     ← share this with clients/peers
```

That `address` line is what operators copy into client / peer-relay configs.

### `Identity` adapter to `NoiseIdentity`

Same `IdentityNoiseAdapter` pattern used by Plan C's two-peer integration test. It belongs in `sunset-core` properly (so any host can use it without re-defining), but for v0 we duplicate the small adapter in `sunset-relay`. A follow-up plan can move the impl into sunset-core.

### Federated peer dialing

After the engine is running and the relay has published its own subscription, for each peer URL in `config.peers`, the relay calls `engine.add_peer(addr)`. The Noise IK handshake completes; the peer relays exchange subscriptions (both subscribe to "all", so they push everything to each other); the engines start syncing.

**v0 behavior on failure:** if a peer URL fails to dial (connection refused, DNS failure, Noise handshake fails), log a WARN and continue. The relay does not retry. Restart the relay to re-attempt.

**Loop behavior:** with two relays mutually subscribed to "all", an entry inserted at A propagates to B; B sees it from A and would push back to A. A's store sees `entry.priority <= existing.priority` → `Stale` → no re-broadcast. So one wasted round-trip per entry, but no infinite loop.

### Logging

`tracing` + `tracing-subscriber` with the `EnvFilter` layer reading `RUST_LOG`. Default filter (no env): `info` on `sunset_relay`, `warn` on everything else.

What gets logged:

- Startup banner (the address block above).
- Each connection accepted + the peer's claimed identity (from Noise handshake).
- Each federated peer dial attempt + outcome.
- Each `do_publish_subscription` write (the relay's own).
- Each shutdown stage on SIGTERM/SIGINT.

What does NOT get logged at INFO:

- Per-message activity (would be too noisy for an active relay; visible at DEBUG).
- Sync `DigestExchange` / `EventDelivery` mechanics (DEBUG).

### Docker image

`packages.sunset-relay-docker` in `flake.nix`:

```nix
sunset-relay-docker = pkgs.dockerTools.buildLayeredImage {
  name = "sunset-relay";
  tag = "latest";
  contents = [ packages.sunset-relay pkgs.cacert ];
  config = {
    Entrypoint = [ "/bin/sunset-relay" ];
    Cmd = [ "--config" "/etc/sunset-relay.toml" ];
    ExposedPorts."8443/tcp" = {};
    Env = [ "RUST_LOG=sunset_relay=info" ];
    Volumes."/var/lib/sunset-relay" = {};
  };
};
```

`pkgs.cacert` is included so outbound `wss://` to peer relays works (TLS root certs).

`packages.sunset-relay` itself is built via the same nix recipe pattern Plan A used for `sunset-core-wasm`: a custom `buildPhase` calling `cargo build` (since `rustPlatform.buildRustPackage`'s `cargoBuildHook` hard-codes `--target` and we need to control the full invocation). Native target — no wasm involved.

Operators run:

```
docker run -d \
  -p 8443:8443 \
  -v /var/lib/sunset-relay:/var/lib/sunset-relay \
  -v ./relay.toml:/etc/sunset-relay.toml \
  ghcr.io/<owner>/sunset-relay:latest
```

(The image registry path is operator-configurable; we don't push to a registry from this plan — that's a release-engineering concern.)

## Multi-relay integration tests

Both tests live in `crates/sunset-relay/tests/multi_relay.rs` and run in-process (no subprocess; no Docker; just `Relay::new(...)` + `tokio::spawn`). FsStore's per-test temp directory is cleaned up by `tempfile`.

### Test 1: two-relay propagation

```text
clients         relays
                                               
alice ─────► relay-A ◄──────► relay-B ◄───── bob

alice publishes message
  → relay-A receives + stores
  → relay-A pushes to relay-B
  → relay-B stores
  → relay-B pushes to bob
  → bob decodes, asserts author + body
```

This is the headline acceptance test. It exercises every layer of the stack.

### Test 2: federated redundancy / failover

```text
clients         relays
                                               
alice ─────┬─► relay-A ◄──────► relay-B ◄┬─── bob
           └────────────────────────────┘
        (alice connects to BOTH; bob connects to BOTH)

1. alice writes msg-1 → both A and B receive (alice sends both ways)
2. bob receives msg-1 from whichever pushed first
3. relay-A is shut down (its task is aborted)
4. alice writes msg-2 → relay-B still receives (alice's other path)
5. bob still receives msg-2 via relay-B
```

Failover test proves the redundancy property. If we ever break the architecture's "redundant relays converge cheaply" claim, this test catches it.

### What the tests do NOT exercise (out of scope)

- TLS termination
- Docker container instantiation (covered separately by a `nix flake check` or `nix build` invocation, not by Rust tests)
- Disk-full / IO errors
- Network partitions beyond "kill relay process"
- High-volume / load testing

## Trait surface

`crates/sunset-relay/src/lib.rs` exposes:

```rust
pub mod config;
pub mod identity;
pub mod relay;

pub use config::Config;
pub use relay::{Relay, RelayHandle};
```

```rust
pub struct Relay { /* private */ }

impl Relay {
    /// Construct a Relay from a fully-resolved Config. Opens the store,
    /// loads/generates identity, binds the listener — but does NOT start
    /// the engine. Returns a handle for the caller to drive the run loop.
    pub async fn new(config: Config) -> Result<RelayHandle>;
}

pub struct RelayHandle {
    pub local_address: String,        // ws://host:port#x25519=<hex>
    pub ed25519_public: [u8; 32],
    pub x25519_public:  [u8; 32],
    /* private fields */
}

impl RelayHandle {
    /// Run the engine forever. Returns on shutdown signal (SIGTERM/SIGINT)
    /// in the binary path, or when the caller drops the handle in tests.
    pub async fn run(self) -> Result<()>;

    /// For tests: return the address clients should dial.
    pub fn dial_address(&self) -> String { self.local_address.clone() }
}
```

The binary's `main.rs`:

```rust
#[tokio::main(flavor = "current_thread")]
async fn main() -> sunset_relay::Result<()> {
    let local = tokio::task::LocalSet::new();
    local.run_until(async {
        let config = sunset_relay::Config::from_cli_or_default()?;
        let relay = sunset_relay::Relay::new(config).await?;
        let handle = relay; // Relay::new returns the handle directly
        handle.run().await
    }).await
}
```

(Single-threaded tokio because sunset-sync is `?Send` for wasm-compat — the relay runs natively but we keep the same model end-to-end.)

## Tests + verification

- **Native unit tests in sunset-relay**: Config TOML round-trip; identity load-or-generate (in tempdir); address-string formatting.
- **Integration tests** (`tests/multi_relay.rs`): the two scenarios above.
- **No regressions**: full workspace `cargo test --workspace --all-features` and `cargo clippy ... -D warnings`.
- **Docker image builds**: `nix build .#sunset-relay-docker` produces a `result/` symlink to the OCI tarball; `docker load < result` succeeds (actual `docker run` not part of the automated test — operators verify locally).

## Items deferred

- HTTP admin / status surface
- Allowlists, rate limiting, per-room admission
- TLS termination at the relay (use a fronting proxy in v0)
- Reconnection-with-backoff for federated peer links
- Loop-suppression / deduping smarts for federated propagation
- Encrypted identity-at-rest
- Multi-relay client-side redundancy (client-side concern; client/UI plans handle it)
- High-volume / load testing harnesses
- Push notifications for offline clients

## Self-review checklist

- [x] Four non-negotiables (single binary, relay-as-peer, persistence, multi-relay tests) are met by named mechanisms.
- [x] Config schema is concrete with defaults documented.
- [x] Identity persistence path + permissions are pinned.
- [x] Federation behavior on peer dial failure is explicit (log + continue).
- [x] Federation loop behavior is acknowledged (LWW dedups; one wasted round-trip).
- [x] Multi-relay tests are concrete enough to plan against.
- [x] Docker derivation uses `dockerTools.buildLayeredImage` per user direction.
- [x] In-process testability via `Relay::new` is explicit so tests don't need subprocesses.
- [x] Out-of-scope items prevent scope creep.
