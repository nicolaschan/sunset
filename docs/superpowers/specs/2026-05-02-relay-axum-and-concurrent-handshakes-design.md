# Relay: axum + concurrent handshakes — design

**Status:** draft, pending user review.
**Authors:** Nicolas Chan & Claude (Opus 4.7).
**Related:**
- Supersedes the byte-peeking dispatcher in `crates/sunset-relay/src/router.rs`.
- Builds on `2026-04-27-sunset-relay-design.md`.
- Builds on `2026-04-27-sunset-sync-ws-native-design.md`.

## Problem

Two coupled problems with the relay's current networking layer:

1. **Hand-rolled HTTP/WS multiplexing.** `crates/sunset-relay/src/router.rs` peeks bytes from the TCP socket, classifies the request prologue (`/dashboard`, `/`, WebSocket upgrade), and forwards `TcpStream`s on an `mpsc::Sender<TcpStream>` to `WebSocketRawTransport::external_streams`. That's ~250 lines of code re-implementing routing badly: it decodes HTTP/1.1 by hand, has its own peek-timeout and headers-end heuristics, and is the source of brittleness any time we want to add another route or change content negotiation.
2. **Inbound work serializes through the engine's accept loop.** `SyncEngine::run` (`crates/sunset-sync/src/engine.rs`) awaits `transport.accept()` inline in its main `select!`. The accept call is a black box that does *all* per-connection inbound work — today that's WS upgrade (`tokio_tungstenite::accept_async`) + Noise IK responder, but the same shape would serialize a future TLS termination, a pre-auth challenge, or anything else done before a connection is "live." A single misbehaving client at *any* of those stages occupies the only accept slot until it completes or times out. The current mitigation is a 15-second per-handshake timeout (`crates/sunset-relay/tests/accept_resilience.rs` regression-tests this), but that bounds the worst case, not the typical case: even honest peers serialize against each other under load, and an adversary can keep the inbound pipeline at <1 connection per `accept_handshake_timeout` indefinitely.

These show up in production: a freshly restarted relay degrades after public-internet probes accumulate against it.

## Goals

- Replace the byte-peek HTTP classifier with a real Rust web framework (axum) for HTTP and WebSocket routing.
- **Spawn-per-connection inbound concurrency.** No matter what slow per-connection work the inbound pipeline has to do, no in-flight connection's progress should hold up the next connection's progress. axum already gives us this for the WS-upgrade stage (each request lands in its own task by construction). For the post-upgrade stage (today: Noise IK; tomorrow: maybe more), provide a generic spawn-per-connection helper that runs the slow work on its own task and returns ready-to-use connections to the engine.
- Keep `sunset-sync`'s engine `?Send` — WASM compatibility is load-bearing per `CLAUDE.md` and remains untouched in this iteration.
- Keep the relay binary as **thin glue**: HTTP framework, dashboard/identity routes (which are inherently relay-specific), engine wiring. Generic plumbing belongs in shared libraries.
- Position the relay so that a future "drop the engine's single-threaded restriction" change is a pure flip of `spawn_local`/`Rc` to `spawn`/`Arc`, with no architectural change.

## Non-goals

- Making the engine `Send + Sync`. That's a separate, much larger project (touches every backend, browser transports, store impls).
- Multi-thread parallelism for the engine itself. The relay's bottleneck is async-concurrency (spawn-per-connection), not thread-parallelism. We *do* run axum on a multi-thread runtime, but that's about avoiding `Send`-trait friction, not extracting throughput.
- Replacing the WebSocket protocol or wire format. The transport stays WS + Noise IK with postcard payloads; only the surrounding plumbing changes.
- Adding HTTPS/TLS. The relay continues to expose plain HTTP/WS; TLS termination is the operator's reverse proxy.
- Cross-host generalization of the dashboard. Status/identity HTML/JSON stays in `sunset-relay`.

## High-level architecture

Two tokio runtimes, bridged by `Send`-able channels.

```
┌─────────────── multi-thread tokio runtime (axum) ───────────────┐
│                                                                  │
│  axum::Router                                                    │
│    .route("/dashboard", get(dashboard_handler))                  │
│    .route("/",          get(root_handler))                       │
│                                                                  │
│  • State: { cmd_tx, ws_tx } — both Send.                         │
│  • dashboard_handler:  cmd_tx.send(Snapshot { reply }).await;    │
│                        render HTML from DashboardSnapshot.       │
│  • root_handler:                                                 │
│      if WebSocketUpgrade present:                                │
│          ws.on_upgrade(|socket| ws_tx.send(socket).ok())         │
│      else:                                                       │
│          cmd_tx.send(Identity { reply }).await; render JSON.     │
└──────────────────────────────────────────────────────────────────┘
                │ ws_tx: mpsc::UnboundedSender<axum::extract::ws::WebSocket>
                │ cmd_tx: mpsc::UnboundedSender<RelayCommand>
                ▼
┌──── current_thread tokio runtime + LocalSet (engine side) ──────┐
│                                                                  │
│  • Command pump (spawn_local): drains cmd_rx; with Rc<Engine>    │
│    + Arc<Store>, builds DashboardSnapshot or Identity reply,     │
│    answers via oneshot.                                          │
│                                                                  │
│  • SpawningAcceptor (sunset-sync): wraps a RawTransport whose    │
│    accept() drains ws_rx. Internal pump: for each raw conn,      │
│    spawn_local a task running do_handshake_responder under       │
│    tokio::time::timeout(handshake_timeout, …). Successes go to   │
│    the auth channel; failures/timeouts drop+log. Implements      │
│    Transport::accept by draining auth_rx; delegates connect to   │
│    a wrapped NoiseTransport.                                     │
│                                                                  │
│  • SyncEngine (existing run loop): its transport.accept() reads  │
│    ready-to-use Connections from the SpawningAcceptor's auth     │
│    channel. Engine never awaits any inbound stage.               │
│                                                                  │
│  • Federated dial: unchanged. engine.add_peer() goes out via the │
│    existing client-side WebSocketRawTransport::dial_only.        │
└──────────────────────────────────────────────────────────────────┘
```

### Why two runtimes

axum requires `Send` futures because `tokio::serve` internally `tokio::spawn`s one task per connection. Handlers that close over the engine's `Rc<Engine>` won't compile. We've considered three reconciliations:

1. **Axum on the same `current_thread` LocalSet runtime.** axum's serve loop wants `Send` regardless of underlying runtime; we'd be fighting axum's bounds.
2. **Two runtimes (chosen).** Axum on multi-thread, engine on `current_thread + LocalSet`. Bridge via `mpsc::UnboundedSender` (Send) for upgraded WebSockets and engine commands. Each runtime's idiomatic patterns work natively. Handlers hold only `Send` state. Engine internals stay `Rc`-based and WASM-compatible.
3. **Skip axum, use hyper-1 directly under LocalSet.** Hyper's bounds are looser. But we'd hand-roll routing for the third time and forfeit the framework benefit that motivated this change.

(2) wins: it solves both problems with a small bridge layer (~one command-pump task plus the SpawningAcceptor's pump, both pure mpsc/oneshot wiring), and it leaves the future "drop single-threaded engine" change as a local refactor — `spawn_local` → `spawn`, `Rc` → `Arc` — without touching the two-runtime split.

### Concurrency model — spawn-per-connection at every stage

The principle: **every per-connection slow stage runs on its own task**, so no single connection holds up the rest. The inbound pipeline today has two slow stages — WS upgrade and Noise IK — and the principle is satisfied at each:

- **WS upgrade (axum).** axum spawns one task per HTTP request as part of its built-in serving loop. The WS handler completes the WebSocket upgrade and pushes the upgraded `axum::extract::ws::WebSocket` over `ws_tx`. Slow upgrade negotiations don't block other connections.
- **Post-upgrade promotion (sunset-sync).** A generic spawn-per-connection helper lives in `sunset-sync` (sketch: takes a stream of raw connections + a `Fn(RawConnection) -> Future<Result<Connection>>` "promote" callback + a per-task timeout). For each raw connection, it `spawn_local`s the promote future. Successful promotions land on an internal channel; failures and timeouts log and drop. The helper is generic over what the promote future actually does — today it's Noise IK responder; future stages (TLS, anti-DoS challenge, protocol negotiation) compose into the same callback.
- **Engine-side accept** is now a pure channel drain. `Transport::accept()` reads ready-to-use connections from the helper's authenticated channel. The engine never awaits any handshake.

The existing per-handshake timeout in the engine (`SyncConfig::accept_handshake_timeout` and `accept_with_timeout` in `engine.rs`) becomes redundant under this model — slow stages are bounded inside their own spawned tasks — and is **deleted** (see Migration plan).

The structural future-proofing: every `spawn_local` here is a place we'd flip to `tokio::spawn` once the engine is `Send + Sync`. The two-runtime split survives that change unchanged; in the future world, both runtimes could be the same multi-thread runtime if we wanted, but the bridge channels are what makes the relay's HTTP/WS layer multi-thread *today* and the engine multi-thread *eventually*.

## Component split

| Crate | Responsibility | Changes |
|-------|---------------|---------|
| `sunset-sync-ws-native` | Both client and server-side WS *transport*. Client: `dial_only` (unchanged). Server: drains a channel of upgraded WebSocket streams. **Does not know about Noise.** New optional Cargo feature `axum`: provides axum-native helpers for mounting a WS upgrade handler. | Drop `listening_on` and `external_streams` modes (both unused outside the crate's own self-tests post-change). New `axum` feature module: an axum WebSocket upgrade handler / extractor that produces the upgraded socket onto an mpsc the new server-side `WebSocketRawTransport` constructor drains. `WebSocketRawConnection` gains an axum-`WebSocket` variant behind the feature. |
| `sunset-sync` | Generic spawn-per-connection acceptor helper. Sketch: a `SpawningAcceptor<R: RawTransport, F, …>` (or equivalent factory function) that owns an internal `spawn_local` pump — for each `raw.accept()` result, spawns a task that runs the user-supplied promote future under `tokio::time::timeout(timeout, …)` and pushes successful results onto an internal channel. Implements the accept side of `Transport`; pairs with a connector (e.g. `NoiseTransport`) for the `connect` side. **Cleanup:** delete `accept_with_timeout` and `SyncConfig::accept_handshake_timeout` — the engine no longer needs either. | New helper. Engine `run` loop simplifies to `transport.accept()` (no timeout wrapper). `SyncConfig` field removed. |
| `sunset-noise` | Per-raw-connection Noise IK handshake. **Unchanged.** `do_handshake_responder` (existing public function in `crates/sunset-noise/src/handshake.rs`) is what the relay passes as the promote callback to the new acceptor. `NoiseTransport` continues to handle the connector (initiator) side. | None. |
| `sunset-relay` | **Thin glue.** Spin two runtimes. Define `RelayCommand` enum + bridge channels. Mount axum routes. WS route delegates to `sunset-sync-ws-native`'s axum handler. Compose `WebSocketRawTransport::serving()` + `sunset-sync::SpawningAcceptor` (with `do_handshake_responder` as the promote callback) for inbound; `NoiseTransport` over `WebSocketRawTransport::dial_only` for outbound. `/dashboard` and `/` JSON identity go through `cmd_tx` and read `Rc<Engine>` on the engine runtime. The handshake-timeout duration is owned here (config/CLI/const, plan-writing detail). | Delete `router.rs`. Reshape `relay.rs` (replace listener+dispatch with two-runtime setup). Reshape `status.rs` into "build `DashboardSnapshot` from `Rc<Engine>` (engine side)" + "render HTML/JSON from snapshot (axum side, Send-friendly)". `resolver_adapter.rs` (reqwest-based federated peer URL resolution) stays as-is. |

## Data flow

### Inbound WebSocket

1. Peer dials `ws://relay/`.
2. axum's `WebSocketUpgrade` extractor matches the `Upgrade` header in `root_handler`. The handler returns `ws.on_upgrade(|socket| async move { ws_tx.send(socket).ok(); })`. axum completes the WS protocol upgrade *on its own per-request task* — slow upgrade negotiations don't block other connections.
3. On the engine runtime, `WebSocketRawTransport::serving()` drains `ws_rx`. Each upgraded WebSocket is wrapped as a `WebSocketRawConnection::Axum` and yielded by `RawTransport::accept()`.
4. `sunset-sync::SpawningAcceptor`'s pump task drains `raw.accept()`. For *each* raw connection: `tokio::task::spawn_local(async move { tokio::time::timeout(handshake_timeout, do_handshake_responder(raw_conn, identity)).await; … })`. On success, push the authenticated `NoiseConnection` onto the acceptor's internal channel. On timeout / IK failure / IO error: drop and log at `debug!`. The slow Noise IK responder runs on its own per-connection task; nothing else waits on it.
5. Engine's `Transport::accept()` (= `SpawningAcceptor::accept`) reads the next authenticated connection from the channel and spawns its per-peer task (existing logic in `engine.rs:spawn_peer`). Engine never blocks on any inbound stage.

### Dashboard / identity

1. axum handler receives a `GET /dashboard` (or `GET /` without an upgrade header).
2. Handler builds a `oneshot::channel<DashboardSnapshot>` (or `<IdentitySnapshot>`), sends `RelayCommand::Snapshot { reply }` (or `Identity { reply }`) over `cmd_tx`, awaits the reply.
3. The engine runtime's command pump task drains the command. With `Rc<Engine>` + `Arc<Store>` access, it builds a `DashboardSnapshot`: peers, configured peers, connection states, store stats (entry count, blob count, last-write seq), identity public keys, dial URL, listen address, uptime. It sends the snapshot back via the oneshot.
4. axum handler renders HTML (or JSON for identity) from the snapshot, returns response.

`DashboardSnapshot` is a plain `Send` POD struct; the rendering function is `Send`. The `Rc`-laden engine never crosses runtimes.

### Federated dial (outbound)

Unchanged. `engine.add_peer(addr)` resolves the URL via `sunset-relay-resolver` (reqwest client), then dials via `WebSocketRawTransport::dial_only` and the existing client-side `NoiseTransport`. The two-runtime split has no effect on outbound connections.

## Crate-level details

### `sunset-sync-ws-native`

**Public surface after change:**

- `WebSocketRawTransport::dial_only() -> Self` — unchanged.
- `WebSocketRawTransport::serving() -> (Self, ServingHandle)` — new. The transport's `accept()` drains a channel of upgraded WebSocket streams; the `ServingHandle` exposes the channel sender for whatever upstream framework is doing the upgrade.
- (Behind `axum` feature) `axum_ws_handler<S>(handle: ServingHandle) -> impl Handler<…, S>` — an axum handler factory that closes over the channel sender and returns an axum-compatible handler that performs the WS upgrade and pushes the result. Or an extractor-shaped wrapper, whichever lands more cleanly during plan-writing — both serve the same purpose.

**Internal:**

- `WebSocketRawConnection` enum gains an `Axum(axum::extract::ws::WebSocket)` variant (gated by feature). Its `send_reliable`/`recv_reliable`/`close` impls delegate to axum's `WebSocket::send`/`recv`/`close` with the same Binary-only message discipline as today. Ping/Pong are handled inside the WebSocket layer (tokio-tungstenite auto-pongs at the protocol level); when they surface up to `recv_reliable`, the impl skips them and reads the next message — same pattern as the existing `WebSocketRawConnection::recv_reliable`.
- The `TransportMode` enum in the current crate is deleted along with `listening_on` and `external_streams`. The transport collapses to "client (dial_only)" or "server (drains a channel)."

**Tests:**

- Existing `tests/two_peer_ws_noise.rs` rewritten to use axum in-process: build a `Router`, bind a `TcpListener`, spawn `axum::serve`, dial it from `dial_only`. Same assertion (round-trip Noise+Hello).
- The crate's own `lib.rs` integration test (`raw_send_recv_roundtrip`) likewise migrates to axum.

### `sunset-sync`

**Public surface after change:**

- New `SpawningAcceptor` (exact type name plan-writing detail) — generic spawn-per-connection helper. Sketch:
    - Constructor takes a `RawTransport`, a connector `Transport` (used for the `connect` side), a `Fn(R::Connection) -> Future<Result<C>>` "promote" callback, and a per-task timeout.
    - On construction, spawn an internal pump task on the local set: `loop { match raw.accept().await { Ok(rc) => { spawn_local(async move { let r = tokio::time::timeout(timeout, promote(rc)).await; on_ok send(c) on_err drop+log }); } Err(_) => log+continue } }`.
    - Implements `Transport` such that `accept()` drains the auth channel and `connect()` delegates to the held connector. The engine sees a normal `Transport`; the acceptor's internals are private.
    - Generic over the promote callback so future "slow stages" (TLS termination, anti-DoS challenge, protocol negotiation) compose without changes here.

**Cleanup:**

- Delete `accept_with_timeout` (`crates/sunset-sync/src/engine.rs`). The engine `select!` arm becomes a plain `transport.accept()` await — under the new wiring it just drains a channel, so no timeout wrapper is needed.
- Delete `SyncConfig::accept_handshake_timeout`. The handshake timeout is no longer the engine's responsibility; it's a parameter to `SpawningAcceptor`. Configuration of the duration moves to whoever constructs the acceptor (the relay, today).

**Tests:**

- New unit test: feed three raw connections to a `SpawningAcceptor` with a slow promote function. Two stall (the promote future never completes). The third's promote completes promptly. Assert `Transport::accept()` returns the third's connection without waiting on the other two — i.e., the slow ones do not serialize.
- Add a test that the per-task timeout fires independently per task: the stalled tasks return after exactly the timeout and don't accumulate forever.

### `sunset-noise`

**No changes.** `do_handshake_responder` (existing public function in `crates/sunset-noise/src/handshake.rs`) is what the relay passes as the promote callback to `SpawningAcceptor`. `NoiseTransport::new(raw, identity)` continues to handle the connector (initiator) side and is what `SpawningAcceptor`'s `connect` delegates through.

### `sunset-relay`

**`relay.rs`:**

- Two-runtime entry point. The binary spawns:
    - A multi-thread runtime (`tokio::runtime::Builder::new_multi_thread().enable_all()`). On it: `axum::serve(listener, app)`.
    - A current-thread runtime + `LocalSet`. On it: command pump, `SpawningAcceptor`'s internal pump (started when the acceptor is constructed), engine task.
- The `Config` struct and identity/store setup don't move.
- `RelayHandle::run` becomes a small orchestrator that awaits both runtimes' shutdown signals and propagates SIGINT/SIGTERM to both.

**New: `bridge.rs` (or inline in `relay.rs`):**

- `RelayCommand` enum (`Snapshot { reply }`, `Identity { reply }`).
- Command pump task — drains `cmd_rx`, walks `Rc<Engine>` + `Arc<Store>` to build replies. (The WS-side pump now lives in `sunset-sync::SpawningAcceptor`, not the relay.)

**`status.rs`:**

- Splits cleanly into:
    - `snapshot.rs` (new): `build_dashboard_snapshot(engine: &Engine, store: &dyn Store, ...) -> DashboardSnapshot`. Engine-side; takes `Rc`/`Arc` references and builds a `Send` POD.
    - `render.rs` (new) or stays in `status.rs`: `render_dashboard(snapshot: &DashboardSnapshot) -> Html<String>` and `render_identity(snapshot: &IdentitySnapshot) -> Json<...>`. axum-side.

**Deletions:**

- `router.rs` (the byte-peeking dispatcher).
- The `external_streams` plumbing wired in `relay.rs`.

**`main.rs`:**

- Minor: switches from a single-runtime `current_thread + LocalSet` setup to the two-runtime pattern above. Banner formatting unchanged.

## Error handling

- **WS upgrade failure** (malformed `Sec-WebSocket-Key`, missing version header, unsupported subprotocol): handled by axum, which returns 4xx. Never reaches the engine. No engine-side cleanup needed.
- **Noise handshake failure** (bad signature, IK failure, partial bytes, timeout, peer dropped TCP): handled in the per-conn `spawn_local` task. Logged at `debug!`/`info!`. Nothing propagates to the engine; the engine doesn't know an attempt occurred. This matches today's behavior in spirit, but at task-granularity instead of select-arm-granularity — and crucially, one bad task can't prevent the next promote task from running.
- **Bridge channel closure** (engine runtime exited, command/ws channels dropped): axum handlers return 503. We won't normally see this — both runtimes shut down together on SIGINT/SIGTERM. If the engine task panics, supervisor logic in `RelayHandle::run` aborts the axum runtime as well; we don't run a half-relay.
- **Acceptor pump death** (e.g., `raw.accept()` returns a fatal error): logged + engine accept channel closed. Engine treats this as transport failure (existing behavior). The two-runtime supervisor escalates to full shutdown.
- **Per-task timeout** (slow/malicious peer): the `spawn_local`'d promote task hits `tokio::time::timeout(handshake_timeout, …)`, drops the in-flight raw connection (and any partially-built state), logs, returns. The TCP/WS resources close on drop. No accumulation.
- **Subscriber-style backpressure** on the bridge channels: not a concern in v1. WS upgrades on a relay are bounded by the OS's accept rate and the per-handshake timeout (so `ws_rx` length is bounded by the number of in-flight handshakes — typically small, even under load); command channel traffic is bounded by HTTP request rate which is human-driven for the dashboard. We use unbounded channels and document this; a future change can bound them if observation demands.

## Testing strategy

- **Existing `accept_resilience.rs`** (the "rude WS client" + "failed WS handshake" tests): preserved verbatim. They pass *more strongly* under the new wiring — a rude client occupies one `spawn_local`'d task for at most the configured handshake timeout, not the engine's entire accept loop. The healthy-client deadline (5 s, 20 s) shouldn't change. Note: where the tests reference `SyncConfig::accept_handshake_timeout` to set or interpret bounds, they shift to the relay's per-acceptor timeout config.
- **New `relay_concurrent_handshakes.rs`** (regression target for the concurrency property):
    - Spin up the relay with a small per-acceptor handshake timeout (e.g. 1 s) so the test runs fast.
    - Launch N (say, 8) rude WS clients that complete the upgrade and stall.
    - Launch one healthy client.
    - Assert the healthy client completes its full Noise+Hello within ~3 s — i.e., not roughly N × handshake-timeout.
    - With today's serialized-handshake model and that 1 s timeout, the healthy dial would land around 8 s after the rude probes; under the new model it must be ~RTT.
- **Existing `multi_relay.rs`** and **`resolver_integration.rs`**: unaffected; should pass unchanged.
- **`http_index.rs`**: rewritten to hit axum routes instead of the byte-peek classifier; same assertions on the dashboard HTML and the JSON identity body.
- **`sunset-sync-ws-native::two_peer_ws_noise`**: rewritten to spin up axum in-process for the server side. Same round-trip assertion. Replaces the deleted `listening_on` self-test.
- **`sunset-sync::SpawningAcceptor` unit tests**: described above (third promote completes without waiting on slow ones; per-task timeout fires independently). These do not depend on Noise — they use a synthetic `RawTransport` and a synthetic promote function so the concurrency property is tested at the helper's level.

## Migration plan

This is a single-PR change (no in-flight feature flag needed; the relay binary is the only consumer):

1. Add `axum` as a workspace dep + the `axum` feature on `sunset-sync-ws-native`.
2. Implement the new server-side surface in `sunset-sync-ws-native`: drop `listening_on`/`external_streams`, add `serving()` + the axum handler.
3. Implement `SpawningAcceptor` in `sunset-sync`. Add unit tests using a synthetic `RawTransport` + synthetic promote function (no Noise dependency at the test layer).
4. Delete `accept_with_timeout` and `SyncConfig::accept_handshake_timeout` from `sunset-sync`. Update any test fixtures that referenced the field.
5. Restructure `sunset-relay`: delete `router.rs`, split `status.rs`, add the two-runtime setup + bridge tasks. Compose `WebSocketRawTransport::serving()` + `SpawningAcceptor` (with `do_handshake_responder` as the promote callback) for inbound. Migrate `tests/http_index.rs` to axum. Add `tests/relay_concurrent_handshakes.rs`.
6. Verify `cargo test --workspace --all-features` and `cargo clippy --workspace --all-features --all-targets -- -D warnings` per `CLAUDE.md`.
7. Manual smoke test: start the relay, hit `/dashboard` in a browser, dial it from `sunset-web-wasm`.

No backwards-compatibility shims are needed — there are no other native consumers of the deleted modes, and the wire format is unchanged.

## Open questions

- **axum WS extractor vs. raw upgrade.** During plan-writing, decide whether the `sunset-sync-ws-native::axum` integration uses `axum::extract::ws::WebSocket` directly (cleaner, slightly higher-level) or extracts the underlying `Upgraded` IO and constructs `tokio_tungstenite::WebSocketStream` ourselves (lower-level, identical to client-side message handling). Either is fine; the choice is local to the new `Axum` variant of `WebSocketRawConnection`.
- **Exact `SpawningAcceptor` API shape.** Trait bounds on the promote callback, whether construction takes a separate connector or wraps a `Transport` directly, lazy-vs-eager pump start, etc. Plan-writing detail.
- **Bridge channel bounds.** v1 uses unbounded channels for simplicity; if production shows pathological backpressure scenarios, revisit.
- **Future `RelayCommand` extensions.** v1 is read-only (Snapshot, Identity). Future admin actions (force-disconnect a peer, revoke trust, etc.) extend the same enum without architectural change.
