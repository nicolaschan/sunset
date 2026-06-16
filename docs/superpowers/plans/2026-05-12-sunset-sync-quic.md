# sunset-sync-quic Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship a new workspace crate `sunset-sync-quic` that implements `sunset_sync::RawTransport` by hole-punching a UDP connection between two natted native peers and running QUIC on top of it, using a shared `Signaler` (typically a `sunset-store`-backed `RelaySignaler`) to exchange candidate addresses out-of-band.

**Architecture:** One `tokio::net::UdpSocket` shared per `QuicRawTransport` instance. Probe packets prefixed with magic `b"SnP1"` are siphoned off in a custom `quinn::AsyncUdpSocket` impl; the remainder is handed to a `quinn::Endpoint`. Per-peer `HolepunchCoordinator` discovers candidates (local interfaces + STUN), publishes them via `Signaler`, fires Ping/Pong probes, and once a working candidate is confirmed, drives quinn's `connect()` / `accept()` to bring up a QUIC connection that exposes a 4-byte-length-prefixed bidi stream (reliable) plus QUIC datagrams (unreliable). Dispatcher pattern mirrors `WebRtcRawTransport`.

**Tech Stack:** Rust (edition 2024, MSRV 1.85), `quinn` 0.11 (already in tree via wtransport), `rustls` 0.23, `rcgen` 0.14 for self-signed certs, `stunclient` 0.4 + `network-interface` 1.x (new deps, same as `~/src/udpp`), `postcard` for wire encoding, `async-trait` `?Send`, single-thread-friendly internals via `Rc` + `tokio::sync::Mutex`.

---

## Pre-flight: read these files before you start

You're walking into an existing workspace with strong opinions. Skim these:

1. `CLAUDE.md` — workspace rules. Pay extra attention to:
   - "Hermeticity rule" — every dep through `flake.nix`. New crates in `nix develop --command cargo …` only.
   - "Clippy policy: no suppressions" — `#[allow(clippy::…)]` / `#[expect(clippy::…)]` are forbidden; `scripts/check-no-clippy-allow.sh` enforces this. If clippy flags something, fix the root cause.
   - "Debugging discipline" — tests encode the contract. No `tokio::time::sleep` to mask races. No engine-internal `wait_for` polls.
2. `docs/superpowers/specs/2026-05-12-sunset-sync-quic-design.md` — the spec this plan implements.
3. `crates/sunset-sync/src/transport.rs` — `RawTransport` / `RawConnection` traits we implement.
4. `crates/sunset-sync/src/signaler.rs` — `Signaler` trait we consume.
5. `crates/sunset-sync-webrtc-browser/src/wasm.rs` — dispatcher pattern we mirror (per-peer registry + early-buffer).
6. `crates/sunset-sync-webtransport-native/src/lib.rs` — reliable-stream framing + datagram pattern we mirror.

Commands you'll use repeatedly:

```bash
nix develop --command cargo build -p sunset-sync-quic
nix develop --command cargo test -p sunset-sync-quic
nix develop --command cargo clippy -p sunset-sync-quic --all-features --all-targets -- -D warnings
nix develop --command cargo fmt -p sunset-sync-quic
./scripts/check-no-clippy-allow.sh
```

---

## File structure (lock-in)

New crate layout:

```
crates/sunset-sync-quic/
├── Cargo.toml
├── src/
│   ├── lib.rs           — public surface, re-exports, module wiring
│   ├── wire.rs          — Probe + Candidates wire types, magic constant, postcard codec
│   ├── discovery.rs     — local interface + STUN candidate discovery
│   ├── cert.rs          — rcgen-based self-signed cert; SPKI SHA-256
│   ├── socket.rs        — HolepunchSocket: quinn::AsyncUdpSocket impl that siphons probes
│   ├── coordinator.rs   — HolepunchCoordinator: probe loop, first-confirm-wins
│   ├── connection.rs    — QuicRawConnection: RawConnection impl over a quinn::Connection
│   └── transport.rs     — QuicRawTransport: RawTransport impl + dispatcher
└── tests/
    ├── holepunch_loopback.rs       — happy path end-to-end
    ├── simultaneous_open.rs        — glare case
    └── stun_skipped.rs             — local-only candidate flow
```

Workspace touchpoints (additions only, no existing-crate modifications):

- `Cargo.toml` (workspace root) — add `crates/sunset-sync-quic` to `[workspace] members`; add workspace deps `quinn`, `rcgen`, `rustls`, `network-interface`, `stunclient`.

---

## Task 1: Crate skeleton + workspace member entry

**Files:**
- Create: `crates/sunset-sync-quic/Cargo.toml`
- Create: `crates/sunset-sync-quic/src/lib.rs`
- Modify: `Cargo.toml` (workspace root)

- [ ] **Step 1: Read the workspace root Cargo.toml to confirm current member layout**

Run: `head -120 Cargo.toml`
Expected: see the existing `[workspace] members` array including `crates/sunset-sync-webtransport-native`.

- [ ] **Step 2: Add the new crate as a workspace member and register new workspace deps**

Edit `Cargo.toml`. In `[workspace] members`, append `"crates/sunset-sync-quic"`. In `[workspace.dependencies]`, add the new entries (right after `wtransport`):

```toml
quinn = { version = "0.11", default-features = false, features = ["runtime-tokio", "rustls-ring", "log"] }
rcgen = { version = "0.14", default-features = false, features = ["pem", "ring"] }
rustls = { version = "0.23", default-features = false, features = ["ring", "std"] }
network-interface = "1"
stunclient = "0.4"
sunset-sync-quic = { path = "crates/sunset-sync-quic" }
```

- [ ] **Step 3: Create the crate's Cargo.toml**

Create `crates/sunset-sync-quic/Cargo.toml`:

```toml
[package]
name = "sunset-sync-quic"
version.workspace = true
edition.workspace = true
license.workspace = true
rust-version.workspace = true

[lints]
workspace = true

[dependencies]
async-trait.workspace = true
bytes.workspace = true
futures.workspace = true
hex.workspace = true
network-interface.workspace = true
postcard.workspace = true
quinn.workspace = true
rcgen.workspace = true
rustls.workspace = true
serde.workspace = true
sha2.workspace = true
stunclient.workspace = true
sunset-sync.workspace = true
sunset-store.workspace = true
thiserror.workspace = true
tokio = { workspace = true, features = ["sync", "rt", "macros", "net", "time"] }
tracing.workspace = true

[dev-dependencies]
sunset-noise.workspace = true
sunset-core.workspace = true
sunset-store-memory.workspace = true
sunset-sync = { workspace = true, features = ["test-helpers"] }
tokio = { workspace = true, features = ["macros", "rt", "rt-multi-thread", "time", "sync", "net"] }
```

- [ ] **Step 4: Create the lib.rs entry stub**

Create `crates/sunset-sync-quic/src/lib.rs`:

```rust
//! sunset-sync-quic: NAT-hole-punched QUIC transport for sunset-sync.
//!
//! Implements [`sunset_sync::RawTransport`] using a shared
//! [`sunset_sync::Signaler`] (typically a `RelaySignaler` over a
//! `sunset-store`) to exchange candidate addresses out-of-band, then
//! probes for a working UDP path, then layers QUIC on top.
//!
//! See `docs/superpowers/specs/2026-05-12-sunset-sync-quic-design.md`.
```

- [ ] **Step 5: Confirm the crate builds (empty lib)**

Run: `nix develop --command cargo build -p sunset-sync-quic`
Expected: clean build, no warnings.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml crates/sunset-sync-quic/
git commit -m "$(cat <<'EOF'
sunset-sync-quic: crate skeleton + workspace registration

Empty crate + Cargo.toml + workspace-deps entries (quinn, rcgen,
rustls, network-interface, stunclient). Subsequent commits add the
actual transport.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 2: Wire types — `Probe` + `Candidates` + magic constant

**Files:**
- Create: `crates/sunset-sync-quic/src/wire.rs`
- Modify: `crates/sunset-sync-quic/src/lib.rs`

- [ ] **Step 1: Write the failing test**

Add to `crates/sunset-sync-quic/src/wire.rs`:

```rust
//! Wire formats for the holepunch side-channel and the on-socket probe
//! protocol.
//!
//! Postcard-encoded. `MAGIC` is a 4-byte prefix on every on-socket probe
//! datagram so a [`quinn::AsyncUdpSocket`] wrapper can route probes
//! away from quinn without parsing further.

use std::net::SocketAddr;

use serde::{Deserialize, Serialize};

/// First 4 bytes of every holepunch probe datagram. Chosen to be a
/// recognizable ASCII string (helps tcpdump readers) and to never
/// collide with quinn-emitted packets (quinn's own packets begin with
/// a QUIC v1/v2 header byte, distinct from this prefix). Probes are
/// recognized at the [`quinn::AsyncUdpSocket`] layer before quinn sees
/// them; quinn never has to disambiguate.
pub const MAGIC: [u8; 4] = *b"SnP1";

/// Per-(peer, session) probe datagram.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Probe {
    pub session_id: [u8; 16],
    pub role: ProbeRole,
    pub sender_pk: [u8; 32],
    pub nonce: [u8; 16],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProbeRole {
    Ping,
    Pong,
}

impl Probe {
    /// Encode this probe with the 4-byte MAGIC prefix.
    pub fn encode(&self) -> Result<Vec<u8>, postcard::Error> {
        let body = postcard::to_allocvec(self)?;
        let mut out = Vec::with_capacity(MAGIC.len() + body.len());
        out.extend_from_slice(&MAGIC);
        out.extend_from_slice(&body);
        Ok(out)
    }

    /// Decode a probe from a datagram that starts with MAGIC. Returns
    /// `Ok(None)` if the prefix doesn't match.
    pub fn decode(bytes: &[u8]) -> Result<Option<Self>, postcard::Error> {
        if bytes.len() < MAGIC.len() || bytes[..MAGIC.len()] != MAGIC {
            return Ok(None);
        }
        let probe: Probe = postcard::from_bytes(&bytes[MAGIC.len()..])?;
        Ok(Some(probe))
    }
}

/// One side's candidate-address advertisement, sent over [`sunset_sync::Signaler`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Candidates {
    pub session_id: [u8; 16],
    pub addresses: Vec<SocketAddr>,
    pub server_cert_sha256: [u8; 32],
}

/// Versioned wire enum carried inside [`sunset_sync::SignalMessage::payload`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum QuicSignal {
    Candidates(Candidates),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_roundtrips_with_magic_prefix() {
        let p = Probe {
            session_id: [7u8; 16],
            role: ProbeRole::Ping,
            sender_pk: [9u8; 32],
            nonce: [3u8; 16],
        };
        let bytes = p.encode().unwrap();
        assert_eq!(&bytes[..4], &MAGIC);
        let back = Probe::decode(&bytes).unwrap();
        assert_eq!(back, Some(p));
    }

    #[test]
    fn probe_decode_rejects_non_magic() {
        let bytes = b"NOTQUIC".to_vec();
        let back = Probe::decode(&bytes).unwrap();
        assert_eq!(back, None);
    }

    #[test]
    fn probe_decode_short_bytes_is_none() {
        let back = Probe::decode(&[0u8; 2]).unwrap();
        assert_eq!(back, None);
    }

    #[test]
    fn candidates_roundtrip_through_quic_signal() {
        let c = Candidates {
            session_id: [1u8; 16],
            addresses: vec!["127.0.0.1:7777".parse().unwrap(), "[::1]:7778".parse().unwrap()],
            server_cert_sha256: [42u8; 32],
        };
        let bytes = postcard::to_allocvec(&QuicSignal::Candidates(c.clone())).unwrap();
        let back: QuicSignal = postcard::from_bytes(&bytes).unwrap();
        assert!(matches!(back, QuicSignal::Candidates(ref got) if got == &c));
    }
}
```

In `crates/sunset-sync-quic/src/lib.rs`, add:

```rust
mod wire;
pub(crate) use wire::{Candidates, Probe, ProbeRole, QuicSignal, MAGIC};
```

- [ ] **Step 2: Run tests to verify they pass**

Run: `nix develop --command cargo test -p sunset-sync-quic --lib wire::tests`
Expected: all four tests pass.

- [ ] **Step 3: Run clippy**

Run: `nix develop --command cargo clippy -p sunset-sync-quic --all-features --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 4: Commit**

```bash
git add crates/sunset-sync-quic/
git commit -m "$(cat <<'EOF'
sunset-sync-quic: wire types (Probe, Candidates) + MAGIC prefix

Probe is the on-socket holepunch datagram, prefixed with b"SnP1" so the
quinn AsyncUdpSocket wrapper (added later) can siphon them off without
disturbing QUIC traffic. Candidates is the signaler-side advertisement.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 3: Self-signed cert + SPKI SHA-256

**Files:**
- Create: `crates/sunset-sync-quic/src/cert.rs`
- Modify: `crates/sunset-sync-quic/src/lib.rs`

- [ ] **Step 1: Write the failing test**

Add to `crates/sunset-sync-quic/src/cert.rs`:

```rust
//! Per-process self-signed QUIC server cert + its SPKI SHA-256 hash.
//! The hash is shared via the signaler so the peer can pin it for
//! rustls verification.

use sha2::{Digest, Sha256};

#[derive(Clone, Debug, thiserror::Error)]
pub enum CertError {
    #[error("rcgen: {0}")]
    Rcgen(String),
}

/// One generated self-signed cert and its DER-encoded private key.
#[derive(Clone)]
pub struct SelfSignedCert {
    pub cert_der: Vec<u8>,
    pub private_key_der: Vec<u8>,
    pub spki_sha256: [u8; 32],
}

/// Generate a fresh self-signed cert for SNI `"sunset"`. The cert is
/// only ever validated by SPKI hash pinning on the peer side — CN/SAN
/// values don't matter for our use.
pub fn generate() -> Result<SelfSignedCert, CertError> {
    let cert = rcgen::generate_simple_self_signed(vec!["sunset".to_string()])
        .map_err(|e| CertError::Rcgen(e.to_string()))?;
    let cert_der = cert.cert.der().to_vec();
    let private_key_der = cert.signing_key.serialize_der();

    // SPKI hash: the rustls `ServerCertVerifier` we wire up later will
    // recompute this from the leaf cert's SPKI bytes and compare.
    let (_, parsed) = x509_parser::parse_x509_certificate(&cert_der)
        .map_err(|e| CertError::Rcgen(format!("parse leaf: {e}")))?;
    let spki_bytes = parsed.tbs_certificate.subject_pki.raw;
    let mut hasher = Sha256::new();
    hasher.update(spki_bytes);
    let spki_sha256: [u8; 32] = hasher.finalize().into();

    Ok(SelfSignedCert {
        cert_der,
        private_key_der,
        spki_sha256,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_produces_distinct_certs() {
        let a = generate().unwrap();
        let b = generate().unwrap();
        assert_ne!(a.cert_der, b.cert_der);
        assert_ne!(a.spki_sha256, b.spki_sha256);
    }

    #[test]
    fn spki_sha256_is_deterministic_over_same_cert() {
        // Hash the cert twice via independent paths and confirm
        // generate() returned the same value as a direct recompute.
        let c = generate().unwrap();
        let (_, parsed) = x509_parser::parse_x509_certificate(&c.cert_der).unwrap();
        let mut hasher = Sha256::new();
        hasher.update(parsed.tbs_certificate.subject_pki.raw);
        let expected: [u8; 32] = hasher.finalize().into();
        assert_eq!(c.spki_sha256, expected);
    }
}
```

Add `x509-parser` to the crate's `Cargo.toml` `[dependencies]`:

```toml
x509-parser = { version = "0.16", default-features = false }
```

Also add to the workspace root `Cargo.toml` `[workspace.dependencies]`:

```toml
x509-parser = { version = "0.16", default-features = false }
```

And change the crate's entry to use `.workspace = true`:

```toml
x509-parser.workspace = true
```

In `crates/sunset-sync-quic/src/lib.rs`, add:

```rust
mod cert;
pub(crate) use cert::{generate as generate_cert, SelfSignedCert};
```

- [ ] **Step 2: Run tests to verify they pass**

Run: `nix develop --command cargo test -p sunset-sync-quic --lib cert::tests`
Expected: both tests pass.

- [ ] **Step 3: Run clippy**

Run: `nix develop --command cargo clippy -p sunset-sync-quic --all-features --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 4: Commit**

```bash
git add Cargo.toml crates/sunset-sync-quic/
git commit -m "$(cat <<'EOF'
sunset-sync-quic: per-process self-signed cert + SPKI SHA-256

Each transport instance generates one cert at startup; the hash is
shared with peers via the signaler so they can pin TLS verification
to this exact SPKI regardless of CN.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 4: Candidate discovery — local interfaces + STUN

**Files:**
- Create: `crates/sunset-sync-quic/src/discovery.rs`
- Modify: `crates/sunset-sync-quic/src/lib.rs`

- [ ] **Step 1: Write the failing test**

Add to `crates/sunset-sync-quic/src/discovery.rs`:

```rust
//! Discover a candidate set for a bound UDP socket:
//! * every local interface address (filtered for non-unspecified),
//!   stamped with the socket's bound port;
//! * the STUN-reflexive address for each provided STUN server (if any).

use std::collections::HashSet;
use std::net::{IpAddr, SocketAddr};

use network_interface::{Addr, NetworkInterface, NetworkInterfaceConfig};
use stunclient::StunClient;
use tokio::net::{lookup_host, UdpSocket};

/// Enumerate local-interface socket addrs stamped with `port`.
pub fn local_candidates(port: u16) -> Vec<SocketAddr> {
    let mut out = HashSet::new();
    let ifs = match NetworkInterface::show() {
        Ok(ifs) => ifs,
        Err(e) => {
            tracing::warn!("NetworkInterface::show: {e}");
            return vec![];
        }
    };
    for iface in ifs {
        for addr in iface.addr {
            let ip = match addr {
                Addr::V4(v4) => IpAddr::V4(v4.ip),
                Addr::V6(v6) => IpAddr::V6(v6.ip),
            };
            if ip.is_unspecified() {
                continue;
            }
            out.insert(SocketAddr::new(ip, port));
        }
    }
    out.into_iter().collect()
}

/// Best-effort STUN-reflexive address lookup over the bound socket.
/// Returns an empty `Vec` if all STUN servers fail or the list is empty.
pub async fn stun_candidates(socket: &UdpSocket, stun_servers: &[String]) -> Vec<SocketAddr> {
    let mut out = HashSet::new();
    for server in stun_servers {
        let resolved = match lookup_host(server).await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("stun lookup_host({server}): {e}");
                continue;
            }
        };
        for addr in resolved {
            let client = StunClient::new(addr);
            match client.query_external_address_async(socket).await {
                Ok(reflexive) => {
                    out.insert(reflexive);
                }
                Err(e) => {
                    tracing::warn!("stun query {addr}: {e}");
                }
            }
        }
    }
    out.into_iter().collect()
}

/// Union of local + STUN-reflexive candidates for the given socket and
/// STUN list. Filters unspecified addresses.
pub async fn discover(socket: &UdpSocket, stun_servers: &[String]) -> Vec<SocketAddr> {
    let port = match socket.local_addr() {
        Ok(a) => a.port(),
        Err(_) => return vec![],
    };
    let mut out: HashSet<SocketAddr> = local_candidates(port).into_iter().collect();
    out.extend(stun_candidates(socket, stun_servers).await);
    out.retain(|s| !s.ip().is_unspecified());
    out.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "current_thread")]
    async fn local_candidates_include_loopback_with_port() {
        let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let port = socket.local_addr().unwrap().port();
        let cands = local_candidates(port);
        assert!(
            cands.iter().any(|s| s.ip().is_loopback() && s.port() == port),
            "expected loopback in {cands:?}"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn discover_with_no_stun_returns_only_local() {
        let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let port = socket.local_addr().unwrap().port();
        let cands = discover(&socket, &[]).await;
        assert!(cands.iter().any(|s| s.ip().is_loopback() && s.port() == port));
        // No STUN was queried, so we shouldn't see any public addrs we
        // wouldn't recognize. (Hard to assert positively here — the
        // local IPs vary per box. The loopback check above is the
        // contract.)
    }
}
```

In `crates/sunset-sync-quic/src/lib.rs`, add:

```rust
mod discovery;
```

- [ ] **Step 2: Run tests to verify they pass**

Run: `nix develop --command cargo test -p sunset-sync-quic --lib discovery::tests`
Expected: both tests pass.

- [ ] **Step 3: Run clippy**

Run: `nix develop --command cargo clippy -p sunset-sync-quic --all-features --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 4: Commit**

```bash
git add crates/sunset-sync-quic/
git commit -m "$(cat <<'EOF'
sunset-sync-quic: candidate discovery (local interfaces + STUN)

discover() returns the union of every non-unspecified local interface
addr (stamped with the bound port) plus the STUN-reflexive addresses
from the configured servers. STUN failures are logged and skipped.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 5: `HolepunchSocket` — `quinn::AsyncUdpSocket` that siphons probes

**Files:**
- Create: `crates/sunset-sync-quic/src/socket.rs`
- Modify: `crates/sunset-sync-quic/src/lib.rs`

quinn 0.11's `AsyncUdpSocket` trait shape (paraphrased — verify against the actual `quinn::AsyncUdpSocket` doc when you implement):

```rust
pub trait AsyncUdpSocket: Send + Sync + std::fmt::Debug {
    fn create_io_poller(self: Arc<Self>) -> Pin<Box<dyn UdpPoller>>;
    fn try_send(&self, transmit: &Transmit) -> io::Result<()>;
    fn poll_recv(&self, cx: &mut Context, bufs: &mut [IoSliceMut], meta: &mut [RecvMeta]) -> Poll<io::Result<usize>>;
    fn local_addr(&self) -> io::Result<SocketAddr>;
    fn max_transmit_segments(&self) -> usize { 1 }
    fn max_receive_segments(&self) -> usize { 1 }
    fn may_fragment(&self) -> bool { true }
}
```

We need to:

1. Wrap a tokio `UdpSocket`.
2. In `poll_recv`, after the inner socket fills the buffer, examine the leading 4 bytes. If they equal `MAGIC`, push the `(src, payload)` onto a probe channel and return `Poll::Pending` (so quinn keeps polling, ignoring this packet entirely). If not, leave the bytes for quinn.
3. In `try_send`, just forward to the inner socket.

- [ ] **Step 1: Write the failing test**

Create `crates/sunset-sync-quic/src/socket.rs`:

```rust
//! `quinn::AsyncUdpSocket` wrapper that siphons holepunch probes
//! (datagrams whose first 4 bytes equal [`crate::wire::MAGIC`]) off the
//! quinn data path and forwards them to a separate channel.

use std::fmt;
use std::io;
use std::io::IoSliceMut;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use bytes::Bytes;
use quinn::udp::{RecvMeta, Transmit};
use quinn::{AsyncUdpSocket, Runtime, UdpPoller};
use tokio::sync::mpsc;

use crate::wire::MAGIC;

pub struct HolepunchSocket {
    inner: Arc<quinn::TokioRuntime>,
    udp: Arc<tokio::net::UdpSocket>,
    probe_tx: mpsc::UnboundedSender<(SocketAddr, Bytes)>,
    /// quinn's own AsyncUdpSocket impl over a tokio UdpSocket. We
    /// delegate poll_recv/try_send to this and intercept only when the
    /// magic prefix shows up.
    delegate: Box<dyn AsyncUdpSocket>,
}

impl fmt::Debug for HolepunchSocket {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HolepunchSocket")
            .field("local_addr", &self.udp.local_addr().ok())
            .finish()
    }
}

impl HolepunchSocket {
    pub fn new(
        udp: Arc<tokio::net::UdpSocket>,
        probe_tx: mpsc::UnboundedSender<(SocketAddr, Bytes)>,
    ) -> io::Result<Self> {
        let runtime = Arc::new(quinn::TokioRuntime);
        // quinn::TokioRuntime::wrap_udp_socket expects a std::net::UdpSocket
        // OR a configured `UdpSocketState`. Easiest: hand over our
        // tokio UdpSocket via `from_std`-like helpers from quinn.
        // Adjust to whatever quinn 0.11's helper is named.
        let inner_std = udp.as_ref().try_clone().or_else(|_e| {
            // Fallback: take a std::net::UdpSocket via into_std/clone
            udp.as_ref()
                .try_clone()
                .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("clone udp: {e}")))
        })?;
        let _ = inner_std;
        // The actual delegate construction is finalized in Step 3 — see
        // the implementation note there. For the test in this step we
        // just need the wrapper to compile and round-trip a probe.
        let delegate = runtime
            .wrap_udp_socket(udp.as_ref().try_clone()?.into_std()?)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("wrap_udp_socket: {e}")))?;
        Ok(Self {
            inner: runtime,
            udp,
            probe_tx,
            delegate,
        })
    }

    /// Outbound probe send (raw datagram, bypasses quinn entirely).
    pub async fn send_probe(&self, dst: SocketAddr, bytes: &[u8]) -> io::Result<usize> {
        self.udp.send_to(bytes, dst).await
    }
}

impl AsyncUdpSocket for HolepunchSocket {
    fn create_io_poller(self: Arc<Self>) -> Pin<Box<dyn UdpPoller>> {
        // Delegate's poller drives the underlying file descriptor's
        // writability. Probes share the same fd, but tokio's
        // UdpSocket::send_to handles its own poll_ready internally.
        // Cloning Arc here lets quinn reuse the delegate's poller.
        let delegate = Arc::clone(&self.delegate_arc());
        delegate.create_io_poller()
    }

    fn try_send(&self, transmit: &Transmit) -> io::Result<()> {
        self.delegate.try_send(transmit)
    }

    fn poll_recv(
        &self,
        cx: &mut Context,
        bufs: &mut [IoSliceMut<'_>],
        meta: &mut [RecvMeta],
    ) -> Poll<io::Result<usize>> {
        loop {
            // Pump the delegate. If it returns N filled segments,
            // walk them and route probes off.
            let n = match self.delegate.poll_recv(cx, bufs, meta) {
                Poll::Ready(Ok(n)) => n,
                other => return other,
            };
            let mut quinn_segments = 0;
            for i in 0..n {
                let len = meta[i].len;
                let buf = &bufs[i][..len];
                if len >= MAGIC.len() && buf[..MAGIC.len()] == MAGIC {
                    // Route to probe handler. Ignore tx errors (handler
                    // gone == receiver dropped, nothing actionable here).
                    let _ = self.probe_tx.send((meta[i].addr, Bytes::copy_from_slice(buf)));
                } else {
                    // Compact non-probe segments to the front of bufs[]
                    // so quinn sees a contiguous run of QUIC packets.
                    if quinn_segments != i {
                        // Move bytes from bufs[i] into bufs[quinn_segments]
                        let src_len = meta[i].len;
                        // bufs is &mut [IoSliceMut]; we can't `swap`
                        // IoSliceMuts directly. Instead, copy bytes.
                        let (left, right) = bufs.split_at_mut(i);
                        let dst = &mut left[quinn_segments];
                        let src = &right[0];
                        dst[..src_len].copy_from_slice(&src[..src_len]);
                        meta[quinn_segments] = meta[i];
                    }
                    quinn_segments += 1;
                }
            }
            if quinn_segments == 0 {
                // All n segments were probes. Loop and ask quinn's
                // delegate for more — but only if there's more
                // available without blocking. If poll_recv would
                // block, return Pending.
                //
                // The simplest correct behavior: re-poll. If the
                // delegate has nothing buffered, it returns Pending
                // and registers the waker — quinn gets re-woken when
                // more data arrives.
                continue;
            }
            return Poll::Ready(Ok(quinn_segments));
        }
    }

    fn local_addr(&self) -> io::Result<SocketAddr> {
        self.delegate.local_addr()
    }
}

// Helper: keep an Arc<dyn AsyncUdpSocket> view of the delegate so
// create_io_poller can hand it back to quinn. We can't take ownership
// of `Box<dyn AsyncUdpSocket>` and clone it (it's not Clone), so we
// stash a parallel Arc clone of the underlying value at construction.
impl HolepunchSocket {
    fn delegate_arc(&self) -> Arc<dyn AsyncUdpSocket> {
        // For Step 1 we cheat: we'll restructure in Step 3 of this task
        // so the field IS an Arc instead of a Box. The test in this
        // step only exercises send_probe and the magic-prefix routing.
        unreachable!("delegate_arc placeholder — restructure in Step 3");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::{Probe, ProbeRole};
    use tokio::net::UdpSocket;

    #[tokio::test(flavor = "current_thread")]
    async fn magic_prefixed_datagram_is_routed_to_probe_channel() {
        let listener = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
        let listener_addr = listener.local_addr().unwrap();
        let (probe_tx, mut probe_rx) = mpsc::unbounded_channel();
        let hole = HolepunchSocket::new(Arc::clone(&listener), probe_tx).unwrap();

        // Synthesize a probe datagram and send via send_probe.
        let p = Probe {
            session_id: [1u8; 16],
            role: ProbeRole::Ping,
            sender_pk: [2u8; 32],
            nonce: [3u8; 16],
        };
        let wire = p.encode().unwrap();

        // Send from a separate socket so the listener receives via its
        // recv path.
        let sender = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        sender.send_to(&wire, listener_addr).await.unwrap();

        // Poll the HolepunchSocket via quinn's recv shape. The
        // smallest harness is a future that calls poll_recv until
        // either a Ready(N>0) comes back or the probe_rx receives.
        let timeout = tokio::time::timeout(std::time::Duration::from_secs(1), probe_rx.recv())
            .await
            .expect("probe channel timed out");
        let (src, body) = timeout.expect("probe channel closed");
        assert_eq!(src.ip(), std::net::IpAddr::V4("127.0.0.1".parse().unwrap()));
        assert_eq!(&body[..MAGIC.len()], &MAGIC);
        drop(hole);
    }
}
```

**Implementation note for Step 3:** You'll restructure the `delegate` field to `Arc<dyn AsyncUdpSocket>` after the placeholder shows you that `create_io_poller` needs an `Arc`. The test in Step 1 doesn't drive `create_io_poller` so the placeholder is fine for the failing-test step. In Step 3, replace `Box<dyn AsyncUdpSocket>` with `Arc<dyn AsyncUdpSocket>` everywhere, delete `delegate_arc()`, and have `create_io_poller` call `Arc::clone(&self.delegate).create_io_poller()`. Restructure `poll_recv` to require driving the underlying socket via the actual quinn 0.11 API — read the quinn docs for the exact signature before writing the impl.

In `crates/sunset-sync-quic/src/lib.rs`, add:

```rust
mod socket;
```

- [ ] **Step 2: Run the test, confirm it fails as expected**

Run: `nix develop --command cargo test -p sunset-sync-quic --lib socket::tests::magic_prefixed_datagram_is_routed_to_probe_channel`

Expected: compile error or panic from the `delegate_arc` placeholder.

- [ ] **Step 3: Implement `HolepunchSocket` properly**

Replace the `delegate: Box<dyn AsyncUdpSocket>` field with `delegate: Arc<dyn AsyncUdpSocket>`. Remove the `delegate_arc()` helper. Update `create_io_poller` to use `Arc::clone(&self.delegate).create_io_poller()`.

For `poll_recv`, the actual quinn 0.11 API may differ in argument names — read `quinn::AsyncUdpSocket` docs (`cargo doc -p quinn --open`) before finalizing. The structural shape is: pump the delegate; route magic-prefixed packets to `probe_tx`; compact non-probe packets to the front of the slice; if all are probes, loop back to re-poll the delegate.

Run: `nix develop --command cargo test -p sunset-sync-quic --lib socket::tests`
Expected: the magic-prefix routing test passes.

- [ ] **Step 4: Run clippy**

Run: `nix develop --command cargo clippy -p sunset-sync-quic --all-features --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 5: Commit**

```bash
git add crates/sunset-sync-quic/
git commit -m "$(cat <<'EOF'
sunset-sync-quic: HolepunchSocket — quinn AsyncUdpSocket wrapper

In poll_recv, datagrams beginning with MAGIC ('SnP1') are routed to a
side channel for the holepunch coordinator instead of being passed
to quinn. All other datagrams flow through to quinn unchanged.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 6: `HolepunchCoordinator` — probe loop, first-confirm-wins

**Files:**
- Create: `crates/sunset-sync-quic/src/coordinator.rs`
- Modify: `crates/sunset-sync-quic/src/lib.rs`

The coordinator drives the per-(local-peer, remote-peer) probe loop:

- 250 ms cadence to every remote candidate.
- Listens for inbound `Probe` (from the `probe_rx` channel attached to `HolepunchSocket`).
  - On `Ping`: send `Pong` echoing the nonce, **and** record the confirmed addr.
  - On `Pong`: if the nonce matches one we sent, record the confirmed addr.
- Resolves with the first confirmed `(peer_pk, remote_addr)` or a 5 s timeout.

- [ ] **Step 1: Write the failing test**

Create `crates/sunset-sync-quic/src/coordinator.rs`:

```rust
//! Per-peer probe loop driving the NAT holepunch. Resolves with the
//! first confirmed candidate addr (in either direction) or times out.

use std::collections::HashSet;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use tokio::net::UdpSocket;
use tokio::sync::mpsc;

use crate::wire::{Probe, ProbeRole};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ConfirmedPath {
    pub addr: SocketAddr,
}

#[derive(Clone, Debug, thiserror::Error)]
pub enum HolepunchError {
    #[error("holepunch: no candidate confirmed in {0:?}")]
    Timeout(Duration),
    #[error("holepunch: probe channel closed")]
    ProbeChannelClosed,
}

pub struct HolepunchCoordinator {
    socket: Arc<UdpSocket>,
    session_id: [u8; 16],
    local_pk: [u8; 32],
    remote_pk: [u8; 32],
    remote_candidates: Vec<SocketAddr>,
    /// random nonces we've ever emitted (to recognize Pong replies).
    pending_nonces: HashSet<[u8; 16]>,
    probe_rx: mpsc::UnboundedReceiver<(SocketAddr, Bytes)>,
}

impl HolepunchCoordinator {
    pub fn new(
        socket: Arc<UdpSocket>,
        session_id: [u8; 16],
        local_pk: [u8; 32],
        remote_pk: [u8; 32],
        remote_candidates: Vec<SocketAddr>,
        probe_rx: mpsc::UnboundedReceiver<(SocketAddr, Bytes)>,
    ) -> Self {
        Self {
            socket,
            session_id,
            local_pk,
            remote_pk,
            remote_candidates,
            pending_nonces: HashSet::new(),
            probe_rx,
        }
    }

    /// Drive the probe loop. Resolves with the first confirmed path,
    /// or a Timeout error after `deadline`.
    pub async fn run(mut self, deadline: Duration) -> Result<ConfirmedPath, HolepunchError> {
        let mut probe_interval = tokio::time::interval(Duration::from_millis(250));
        let mut overall_deadline = Box::pin(tokio::time::sleep(deadline));
        // Use rng-free nonces by hashing (session_id, peer_pk, tick).
        // For determinism in tests we'd want this seeded, but the
        // pending_nonces set masks the actual values — only equality
        // matters.
        let mut tick: u64 = 0;
        loop {
            tokio::select! {
                _ = probe_interval.tick() => {
                    tick = tick.wrapping_add(1);
                    let nonce = derive_nonce(&self.session_id, &self.local_pk, tick);
                    self.pending_nonces.insert(nonce);
                    let probe = Probe {
                        session_id: self.session_id,
                        role: ProbeRole::Ping,
                        sender_pk: self.local_pk,
                        nonce,
                    };
                    let wire = match probe.encode() {
                        Ok(w) => w,
                        Err(e) => {
                            tracing::warn!("probe encode: {e}");
                            continue;
                        }
                    };
                    for cand in &self.remote_candidates {
                        if let Err(e) = self.socket.send_to(&wire, *cand).await {
                            tracing::debug!("probe send_to({cand}): {e}");
                        }
                    }
                }
                inbound = self.probe_rx.recv() => {
                    let (src, body) = inbound.ok_or(HolepunchError::ProbeChannelClosed)?;
                    let probe = match Probe::decode(&body) {
                        Ok(Some(p)) => p,
                        _ => continue,
                    };
                    if probe.session_id != self.session_id {
                        continue;
                    }
                    if probe.sender_pk != self.remote_pk {
                        continue;
                    }
                    match probe.role {
                        ProbeRole::Ping => {
                            let pong = Probe {
                                session_id: self.session_id,
                                role: ProbeRole::Pong,
                                sender_pk: self.local_pk,
                                nonce: probe.nonce,
                            };
                            let wire = match pong.encode() {
                                Ok(w) => w,
                                Err(e) => {
                                    tracing::warn!("pong encode: {e}");
                                    continue;
                                }
                            };
                            if let Err(e) = self.socket.send_to(&wire, src).await {
                                tracing::debug!("pong send_to({src}): {e}");
                            }
                            return Ok(ConfirmedPath { addr: src });
                        }
                        ProbeRole::Pong => {
                            if self.pending_nonces.contains(&probe.nonce) {
                                return Ok(ConfirmedPath { addr: src });
                            }
                        }
                    }
                }
                _ = &mut overall_deadline => {
                    return Err(HolepunchError::Timeout(deadline));
                }
            }
        }
    }
}

fn derive_nonce(session_id: &[u8; 16], local_pk: &[u8; 32], tick: u64) -> [u8; 16] {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(session_id);
    h.update(local_pk);
    h.update(tick.to_le_bytes());
    let digest: [u8; 32] = h.finalize().into();
    let mut out = [0u8; 16];
    out.copy_from_slice(&digest[..16]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::{Probe, ProbeRole, MAGIC};

    #[tokio::test(flavor = "current_thread")]
    async fn ping_pong_resolves_confirmed_path_via_loopback() {
        // Two UDP sockets, two coordinators, both pointing at each
        // other's local_addr. Verify both resolve with the peer's
        // actual SocketAddr within deadline.
        let a_sock = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
        let b_sock = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
        let a_addr = a_sock.local_addr().unwrap();
        let b_addr = b_sock.local_addr().unwrap();

        let (a_tx, a_rx) = mpsc::unbounded_channel();
        let (b_tx, b_rx) = mpsc::unbounded_channel();

        // Spawn raw recv loops to feed each coordinator's probe channel.
        let a_sock_cl = Arc::clone(&a_sock);
        tokio::task::spawn_local(async move {
            let mut buf = [0u8; 1500];
            loop {
                let (n, src) = match a_sock_cl.recv_from(&mut buf).await {
                    Ok(v) => v,
                    Err(_) => return,
                };
                if a_tx
                    .send((src, Bytes::copy_from_slice(&buf[..n])))
                    .is_err()
                {
                    return;
                }
            }
        });
        let b_sock_cl = Arc::clone(&b_sock);
        tokio::task::spawn_local(async move {
            let mut buf = [0u8; 1500];
            loop {
                let (n, src) = match b_sock_cl.recv_from(&mut buf).await {
                    Ok(v) => v,
                    Err(_) => return,
                };
                if b_tx
                    .send((src, Bytes::copy_from_slice(&buf[..n])))
                    .is_err()
                {
                    return;
                }
            }
        });

        let session = [9u8; 16];
        let a_pk = [1u8; 32];
        let b_pk = [2u8; 32];
        let a = HolepunchCoordinator::new(
            Arc::clone(&a_sock),
            session,
            a_pk,
            b_pk,
            vec![b_addr],
            a_rx,
        );
        let b = HolepunchCoordinator::new(
            Arc::clone(&b_sock),
            session,
            b_pk,
            a_pk,
            vec![a_addr],
            b_rx,
        );

        let (a_res, b_res) = tokio::join!(a.run(Duration::from_secs(3)), b.run(Duration::from_secs(3)));
        let a_path = a_res.expect("a path");
        let b_path = b_res.expect("b path");
        assert_eq!(a_path.addr, b_addr);
        assert_eq!(b_path.addr, a_addr);

        // Sanity check the wire format used MAGIC.
        let _ = MAGIC;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn timeout_when_no_remote_candidate_responds() {
        let sock = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
        let (_tx, rx) = mpsc::unbounded_channel();
        // Send probes to a black-hole address (a port we know is closed
        // on localhost — pick a high random ephemeral and assume
        // nothing is listening). 127.0.0.1:1 is reliably closed.
        let coord = HolepunchCoordinator::new(
            sock,
            [0u8; 16],
            [0u8; 32],
            [0u8; 32],
            vec!["127.0.0.1:1".parse().unwrap()],
            rx,
        );
        let err = coord
            .run(Duration::from_millis(800))
            .await
            .expect_err("expected timeout");
        assert!(matches!(err, HolepunchError::Timeout(_)));
    }
}
```

The first test needs a `LocalSet` because `spawn_local` is used. Tokio's `flavor = "current_thread"` runtime exposes spawn_local via `tokio::task::LocalSet`. The current_thread runtime allows spawn_local directly, but only inside a LocalSet block. Wrap the body of `ping_pong_resolves_confirmed_path_via_loopback` with `LocalSet::new().run_until(async move { … }).await`. Refer to `crates/sunset-sync-webtransport-native/src/lib.rs`'s tests for the exact incantation.

In `crates/sunset-sync-quic/src/lib.rs`, add:

```rust
mod coordinator;
pub use coordinator::HolepunchError;
```

- [ ] **Step 2: Run tests and confirm both pass**

Run: `nix develop --command cargo test -p sunset-sync-quic --lib coordinator::tests`
Expected: both tests pass within ~1 s.

- [ ] **Step 3: Run clippy**

Run: `nix develop --command cargo clippy -p sunset-sync-quic --all-features --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 4: Commit**

```bash
git add crates/sunset-sync-quic/
git commit -m "$(cat <<'EOF'
sunset-sync-quic: HolepunchCoordinator (probe loop)

Per-peer ping/pong loop over the shared UDP socket. 250 ms probe
cadence to every remote candidate; first Ping → reply with Pong and
return, first matching-nonce Pong → return. Times out per the
deadline supplied by the caller.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 7: `QuicRawConnection` — `RawConnection` impl over a quinn::Connection

**Files:**
- Create: `crates/sunset-sync-quic/src/connection.rs`
- Modify: `crates/sunset-sync-quic/src/lib.rs`

The connection holds:
- `quinn::Connection` (kept alive across send/recv operations).
- One persistent bidi stream (`SendStream` + `RecvStream`) opened at construction.

We mirror `WebTransportRawConnection` from `sunset-sync-webtransport-native` byte-for-byte for the framing (4-byte BE length prefix, 16 MiB cap, 1200 B datagram cap).

- [ ] **Step 1: Write the failing test**

Create `crates/sunset-sync-quic/src/connection.rs`:

```rust
//! `RawConnection` over a single quinn::Connection: one persistent
//! bidi stream framed with a 4-byte big-endian length prefix
//! (reliable) plus QUIC datagrams (unreliable, cap 1200 B).

use async_trait::async_trait;
use bytes::Bytes;
use tokio::sync::Mutex;

use sunset_sync::{Error as SyncError, RawConnection, Result as SyncResult};

pub const MAX_DATAGRAM_PAYLOAD: usize = 1200;
const MAX_RELIABLE_FRAME: usize = 16 * 1024 * 1024;

pub struct QuicRawConnection {
    connection: quinn::Connection,
    send: Mutex<quinn::SendStream>,
    recv: Mutex<quinn::RecvStream>,
}

impl QuicRawConnection {
    pub fn new(
        connection: quinn::Connection,
        send: quinn::SendStream,
        recv: quinn::RecvStream,
    ) -> Self {
        Self {
            connection,
            send: Mutex::new(send),
            recv: Mutex::new(recv),
        }
    }
}

#[async_trait(?Send)]
impl RawConnection for QuicRawConnection {
    async fn send_reliable(&self, bytes: Bytes) -> SyncResult<()> {
        if bytes.len() > MAX_RELIABLE_FRAME {
            return Err(SyncError::Transport(format!(
                "quic send_reliable: frame too large ({} > {MAX_RELIABLE_FRAME})",
                bytes.len()
            )));
        }
        let len = u32::try_from(bytes.len())
            .map_err(|_| SyncError::Transport("quic send_reliable: len > u32::MAX".into()))?;
        let mut s = self.send.lock().await;
        s.write_all(&len.to_be_bytes())
            .await
            .map_err(|e| SyncError::Transport(format!("quic send len: {e}")))?;
        s.write_all(&bytes)
            .await
            .map_err(|e| SyncError::Transport(format!("quic send body: {e}")))?;
        Ok(())
    }

    async fn recv_reliable(&self) -> SyncResult<Bytes> {
        let mut r = self.recv.lock().await;
        let mut len_buf = [0u8; 4];
        r.read_exact(&mut len_buf)
            .await
            .map_err(|e| SyncError::Transport(format!("quic recv len: {e}")))?;
        let len = u32::from_be_bytes(len_buf) as usize;
        if len > MAX_RELIABLE_FRAME {
            return Err(SyncError::Transport(format!(
                "quic recv_reliable: frame too large ({len} > {MAX_RELIABLE_FRAME})"
            )));
        }
        let mut buf = vec![0u8; len];
        r.read_exact(&mut buf)
            .await
            .map_err(|e| SyncError::Transport(format!("quic recv body: {e}")))?;
        Ok(Bytes::from(buf))
    }

    async fn send_unreliable(&self, bytes: Bytes) -> SyncResult<()> {
        if bytes.len() > MAX_DATAGRAM_PAYLOAD {
            return Err(SyncError::Transport(format!(
                "quic send_unreliable: payload too large ({} > {MAX_DATAGRAM_PAYLOAD})",
                bytes.len()
            )));
        }
        self.connection
            .send_datagram(bytes)
            .map_err(|e| SyncError::Transport(format!("quic send_datagram: {e}")))
    }

    async fn recv_unreliable(&self) -> SyncResult<Bytes> {
        let dg = self
            .connection
            .read_datagram()
            .await
            .map_err(|e| SyncError::Transport(format!("quic read_datagram: {e}")))?;
        Ok(dg)
    }

    async fn close(&self) -> SyncResult<()> {
        self.connection.close(0u32.into(), b"closed");
        Ok(())
    }
}
```

In `crates/sunset-sync-quic/src/lib.rs`, add:

```rust
mod connection;
pub use connection::{QuicRawConnection, MAX_DATAGRAM_PAYLOAD};
```

There is no standalone unit test for this file — its behavior is exercised by the integration tests in Tasks 9–11 (the only way to construct a `quinn::Connection` is to run the actual handshake; mocking it would be theatre).

- [ ] **Step 2: Compile-check**

Run: `nix develop --command cargo build -p sunset-sync-quic`
Expected: clean build.

- [ ] **Step 3: Run clippy**

Run: `nix develop --command cargo clippy -p sunset-sync-quic --all-features --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 4: Commit**

```bash
git add crates/sunset-sync-quic/
git commit -m "$(cat <<'EOF'
sunset-sync-quic: QuicRawConnection (RawConnection impl)

One persistent bidi stream with 4-byte BE length-prefix framing
(reliable, cap 16 MiB), plus quinn datagrams (unreliable, cap 1200 B).
End-to-end exercised by the integration tests in subsequent tasks.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 8: `QuicRawTransport` — bind + connect + accept

**Files:**
- Create: `crates/sunset-sync-quic/src/transport.rs`
- Modify: `crates/sunset-sync-quic/src/lib.rs`

The `transport.rs` module is the heart of the crate. It:

1. Owns the shared `Arc<UdpSocket>`, a `HolepunchSocket`, a `quinn::Endpoint` (configured with the `HolepunchSocket` as its `AsyncUdpSocket`), and a `SelfSignedCert`.
2. Spawns a `signaler.recv()` dispatcher task that:
   - Decodes each inbound `SignalMessage::payload` as a `QuicSignal`.
   - Routes `Candidates` to per-peer queues (mirror `WebRtcRawTransport`).
3. Exposes `connect(addr)`:
   - Decode `addr` to a remote `PeerId`.
   - Generate a `session_id`. Build local `Candidates` (using `discovery::discover`). Send to the peer via `signaler.send`.
   - Await the peer's `Candidates` from the per-peer queue (max 5 s).
   - Spawn the `HolepunchCoordinator`. Await `ConfirmedPath` (max 5 s from when we have both sides' candidates — implement as a combined 5 s budget from `connect()` start for simplicity).
   - Determine initiator-ness from `PeerId` ordering. If we're the initiator, call `endpoint.connect(remote_addr, "sunset")` with a `ClientConfig` that pins the peer's `server_cert_sha256`; open one bidi stream. If we're the responder, call `endpoint.accept()` and wait for the inbound bidi stream.
   - Wrap in `QuicRawConnection` and return.
4. Exposes `accept()`:
   - Drains an `mpsc::UnboundedReceiver<Result<QuicRawConnection>>` populated by the dispatcher's per-peer accept tasks.

Because this task is dense, break it into substeps. Each substep ends with a `cargo build` / `cargo clippy` check; the full integration test that exercises end-to-end behavior comes in Task 9.

- [ ] **Step 1: Stub `QuicRawTransport::bind` + state struct**

Create `crates/sunset-sync-quic/src/transport.rs`. Use the field layout described in the spec. Implement `bind()` which:
- Binds an `Arc<tokio::net::UdpSocket>` to `"0.0.0.0:0"`.
- Creates the probe channel (`mpsc::unbounded_channel`).
- Constructs `HolepunchSocket`.
- Generates `SelfSignedCert`.
- Builds a `quinn::ServerConfig` and `quinn::ClientConfig` (default for now; pinning is added per-connect).
- Builds `quinn::Endpoint` via `Endpoint::new_with_abstract_socket(EndpointConfig::default(), Some(server_config), Arc::new(holepunch_socket), Arc::new(quinn::TokioRuntime))`.
- Stashes everything in fields.

Sketch (do **not** copy verbatim — verify each quinn API call against `cargo doc -p quinn`):

```rust
use std::collections::HashMap;
use std::net::SocketAddr;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use futures::channel::mpsc as fmpsc;
use tokio::net::UdpSocket;
use tokio::sync::Mutex;

use sunset_sync::{
    Error as SyncError, PeerAddr, PeerId, RawTransport, Result as SyncResult, Signaler,
};

use crate::cert::{generate as generate_cert, SelfSignedCert};
use crate::connection::QuicRawConnection;
use crate::socket::HolepunchSocket;

pub struct QuicRawTransport {
    signaler: Rc<dyn Signaler>,
    local_peer: PeerId,
    stun_servers: Vec<String>,
    udp: Arc<UdpSocket>,
    cert: SelfSignedCert,
    endpoint: quinn::Endpoint,
    inner: Rc<std::cell::RefCell<Inner>>,
    completed_rx: Rc<Mutex<fmpsc::UnboundedReceiver<SyncResult<QuicRawConnection>>>>,
}

struct Inner {
    /// Per-peer in-progress queues. Each connect() or accept() task
    /// registers here before its first await.
    per_peer: HashMap<PeerId, fmpsc::UnboundedSender<crate::wire::Candidates>>,
    /// Cloned by every spawned accept task to push the completed conn.
    completed_tx: fmpsc::UnboundedSender<SyncResult<QuicRawConnection>>,
    dispatcher_started: bool,
}

impl QuicRawTransport {
    pub async fn bind(
        signaler: Rc<dyn Signaler>,
        local_peer: PeerId,
        stun_servers: Vec<String>,
    ) -> SyncResult<Self> {
        let udp = Arc::new(
            UdpSocket::bind("0.0.0.0:0")
                .await
                .map_err(|e| SyncError::Transport(format!("bind udp: {e}")))?,
        );
        let (probe_tx, probe_rx) = tokio::sync::mpsc::unbounded_channel();
        let _ = probe_rx; // wired into the coordinator per-connection later
        let hole = HolepunchSocket::new(Arc::clone(&udp), probe_tx)
            .map_err(|e| SyncError::Transport(format!("holepunch socket: {e}")))?;
        let cert = generate_cert().map_err(|e| SyncError::Transport(format!("cert gen: {e}")))?;
        let endpoint = build_endpoint(Arc::new(hole), &cert)
            .map_err(|e| SyncError::Transport(format!("build endpoint: {e}")))?;
        let (completed_tx, completed_rx) = fmpsc::unbounded();
        Ok(Self {
            signaler,
            local_peer,
            stun_servers,
            udp,
            cert,
            endpoint,
            inner: Rc::new(std::cell::RefCell::new(Inner {
                per_peer: HashMap::new(),
                completed_tx,
                dispatcher_started: false,
            })),
            completed_rx: Rc::new(Mutex::new(completed_rx)),
        })
    }
}

fn build_endpoint(
    socket: Arc<HolepunchSocket>,
    cert: &SelfSignedCert,
) -> Result<quinn::Endpoint, String> {
    // Build a ServerConfig from the self-signed cert. The actual API
    // call to convert (cert_der, key_der) into a rustls::ServerConfig
    // and then into quinn::ServerConfig changes between quinn versions
    // — refer to quinn 0.11 docs to confirm.
    todo!("translate cert + key into rustls::ServerConfig and then quinn::ServerConfig");
}

#[async_trait(?Send)]
impl RawTransport for QuicRawTransport {
    type Connection = QuicRawConnection;

    async fn connect(&self, _addr: PeerAddr) -> SyncResult<Self::Connection> {
        todo!("Step 2");
    }

    async fn accept(&self) -> SyncResult<Self::Connection> {
        todo!("Step 3");
    }
}
```

**Probe channel split**: this stub creates one `probe_tx`/`probe_rx` pair *for the whole transport*. The coordinator wants the receiver. But there's only one — and per-peer coordinators each need to filter for their own `(session_id, sender_pk)`. Solution: keep one shared receiver inside the dispatcher and route each decoded probe to the matching per-peer coordinator's queue. **Refactor**: in this step the `_ = probe_rx;` placeholder reflects this; the dispatcher (Step 4 of this task) takes ownership of the receiver and routes.

- [ ] **Step 2: Implement `build_endpoint`**

The exact API for converting a self-signed cert into a `quinn::ServerConfig` in quinn 0.11 looks like:

```rust
fn build_endpoint(
    socket: Arc<HolepunchSocket>,
    cert: &SelfSignedCert,
) -> Result<quinn::Endpoint, String> {
    let key = rustls::pki_types::PrivateKeyDer::try_from(cert.private_key_der.clone())
        .map_err(|e| format!("private key: {e}"))?;
    let cert_chain = vec![rustls::pki_types::CertificateDer::from(cert.cert_der.clone())];

    let mut server_crypto = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(cert_chain, key)
        .map_err(|e| format!("rustls server config: {e}"))?;
    server_crypto.alpn_protocols = vec![b"sunset-quic-v1".to_vec()];

    let server_quic =
        quinn::crypto::rustls::QuicServerConfig::try_from(server_crypto)
            .map_err(|e| format!("quic server crypto: {e}"))?;
    let server_config = quinn::ServerConfig::with_crypto(Arc::new(server_quic));

    let endpoint = quinn::Endpoint::new_with_abstract_socket(
        quinn::EndpointConfig::default(),
        Some(server_config),
        socket,
        Arc::new(quinn::TokioRuntime),
    )
    .map_err(|e| format!("endpoint new: {e}"))?;
    Ok(endpoint)
}
```

If quinn 0.11's actual function signatures differ from the sketch (it's likely `Endpoint::new_with_abstract_socket` takes a slightly different combination of args — read the docs), adjust without rewriting the conceptual structure.

Confirm the build:

Run: `nix develop --command cargo build -p sunset-sync-quic`
Expected: clean (modulo the `todo!` placeholders in `connect`/`accept`).

- [ ] **Step 3: Implement the signaler dispatcher**

Add `ensure_dispatcher(&self)` that spins a `tokio::task::spawn_local` task draining `signaler.recv()`. For each `SignalMessage`:

```rust
let payload = msg.payload;
let signal: crate::wire::QuicSignal = match postcard::from_bytes(&payload) {
    Ok(s) => s,
    Err(_) => continue,
};
match signal {
    crate::wire::QuicSignal::Candidates(c) => {
        let from = msg.from;
        let existing = self.inner.borrow().per_peer.get(&from).cloned();
        if let Some(tx) = existing {
            let _ = tx.unbounded_send(c);
            continue;
        }
        // Fresh inbound — register before spawn, then spawn accept task.
        let (peer_tx, peer_rx) = fmpsc::unbounded();
        self.inner.borrow_mut().per_peer.insert(from.clone(), peer_tx);
        let _ = peer_rx.unbounded_send(c); // wrong direction — fix
        // Spawn accept handler.
    }
}
```

(The two-line "unbounded_send the value back into the rx" is obviously wrong — keep the just-received `c` in a local and pass it to the spawned task as the *first* candidates already received. The dispatcher then registers an `peer_tx` for any *subsequent* messages for the same peer.)

The accept task body:

```rust
async fn run_accept(
    transport: ???,
    from: PeerId,
    initial_candidates: crate::wire::Candidates,
    further_rx: fmpsc::UnboundedReceiver<crate::wire::Candidates>,
) {
    // (mirrors run_connect, just doesn't generate the first send —
    // we already received the peer's Candidates, but we still need
    // to send ours.)
}
```

The dispatcher must also handle the early-buffer (peer sends Candidates *before* we have a per_peer slot ready). Same TTL-bound buffer as `WebRtcRawTransport`. For minimum-viable v1, **skip the early buffer** and rely on the per-peer slot being registered before any subsequent peer message lands. Race in practice: the dispatcher serializes one inbound at a time and registers per_peer **before** spawning, so subsequent messages for the same `from` always find the slot. (Different from WebRTC's pre-Offer ICE candidates problem: there's no analog here — Candidates is one shot.)

Confirm the build:

Run: `nix develop --command cargo build -p sunset-sync-quic`
Expected: clean.

- [ ] **Step 4: Implement `connect()`**

`connect()` does:

```rust
async fn connect(&self, addr: PeerAddr) -> SyncResult<Self::Connection> {
    self.ensure_dispatcher();
    let remote = parse_addr(&addr)?;

    // Register per-peer slot before doing any await.
    let (peer_tx, mut peer_rx) = fmpsc::unbounded();
    {
        let mut inner = self.inner.borrow_mut();
        // Reject duplicate connect for the same peer (could happen
        // under glare; the second caller will see Err and the engine's
        // supervisor will retry).
        if inner.per_peer.contains_key(&remote) {
            return Err(SyncError::Transport(format!(
                "quic connect: handshake already in progress with {remote:?}"
            )));
        }
        inner.per_peer.insert(remote.clone(), peer_tx);
    }

    let result = self.run_handshake(remote.clone(), &mut peer_rx, ConnectRole::Initiator).await;

    // Always clean up the per_peer slot before returning.
    self.inner.borrow_mut().per_peer.remove(&remote);
    result
}

enum ConnectRole { Initiator, Acceptor { initial: crate::wire::Candidates } }
```

`run_handshake`:

1. Build local `Candidates` (session_id from `rand`; addresses from `discovery::discover(&self.udp, &self.stun_servers)`; cert hash from `self.cert.spki_sha256`).
2. Encode as `QuicSignal::Candidates(...)` → postcard bytes → `SignalMessage { to: remote, from: self.local_peer, seq: 0, payload }` → `self.signaler.send(...)`.
3. Receive the peer's `Candidates`:
   - If `Acceptor { initial }`: use `initial` immediately.
   - Else: `tokio::time::timeout(Duration::from_secs(5), peer_rx.next()).await`.
4. Set up the probe handler — see below.
5. Spawn `HolepunchCoordinator::run`. Await with a 5 s budget.
6. Decide initiator: `is_initiator = self.local_peer.0 < remote.0`. (Using the `Ord` impl on the `Bytes`-backed `VerifyingKey` — verify it sorts lexicographically. If not, sort raw bytes manually.)
7. If `is_initiator`: build `quinn::ClientConfig` pinned to `peer_candidates.server_cert_sha256`; call `self.endpoint.connect_with(client_config, confirmed.addr, "sunset")`; await connection; `open_bi()`; return `QuicRawConnection`.
8. Else: `self.endpoint.accept().await.ok_or(timeout)?`; await connection; `accept_bi()`; return.

**Critical probe-routing detail**: the `HolepunchSocket::new` channel is shared per *transport*, not per *connection*. We need a way for each per-peer coordinator to see only its own probes. Two approaches:

- (a) Hand each coordinator the shared `probe_rx` directly — but it's a single receiver, so only one coordinator runs at a time. Not viable for parallel multi-peer.
- (b) Run a small router inside the transport: drain the global `probe_rx`, decode each `Probe`, look up the per-peer coordinator by `(session_id, sender_pk)`, push into its private channel. The coordinator's `probe_rx` is one of these private channels.

Pick (b). At `bind()` time, spawn a `task::spawn_local` that drains the global probe channel and routes by `(session_id, sender_pk)` via a `RefCell<HashMap<([u8;16], [u8;32]), mpsc::UnboundedSender<(SocketAddr, Bytes)>>>` shared via `Rc`. Each `run_handshake` registers its key before spawning the coordinator and deregisters on return. **Note the deregistration must happen even on error paths** — wrap with a `scopeguard::guard`-style pattern (manual `Drop`-on-scope-exit using a struct that owns the `Rc` and the key).

This routing infra is a focused, testable surface. **Add a unit test** at the end of this step that constructs two routes, feeds in two probes, and asserts each goes to the right queue.

Confirm:

Run: `nix develop --command cargo build -p sunset-sync-quic`
Expected: clean.
Run: `nix develop --command cargo test -p sunset-sync-quic --lib`
Expected: all unit tests so far pass.

- [ ] **Step 5: Implement `accept()`**

`accept()` drains `self.completed_rx`. The dispatcher's "fresh inbound" arm (Step 3) spawns a task that calls `run_handshake` with `ConnectRole::Acceptor { initial }` and pushes the result onto `completed_tx`.

```rust
async fn accept(&self) -> SyncResult<Self::Connection> {
    self.ensure_dispatcher();
    let mut rx = self.completed_rx.lock().await;
    use futures::StreamExt;
    rx.next()
        .await
        .ok_or_else(|| SyncError::Transport("quic accept: completed channel closed".into()))?
}
```

The "fresh inbound" arm in `ensure_dispatcher` reads:

```rust
let transport_for_task = /* clone the relevant fields/Rcs */;
tokio::task::spawn_local(async move {
    let result = transport_for_task
        .run_handshake(from.clone(), &mut further_rx, ConnectRole::Acceptor { initial })
        .await;
    let _ = completed_tx.unbounded_send(result);
    transport_for_task.inner.borrow_mut().per_peer.remove(&from);
});
```

Add `addr` parsing:

```rust
fn parse_addr(addr: &PeerAddr) -> SyncResult<PeerId> {
    let s = std::str::from_utf8(addr.as_bytes())
        .map_err(|e| SyncError::Transport(format!("addr not utf-8: {e}")))?;
    let suffix = s
        .strip_prefix("quic://")
        .ok_or_else(|| SyncError::Transport(format!("addr not quic://: {s}")))?;
    let bytes =
        hex::decode(suffix).map_err(|e| SyncError::Transport(format!("hex decode: {e}")))?;
    Ok(PeerId(sunset_store::VerifyingKey::new(Bytes::from(bytes))))
}
```

Confirm:

Run: `nix develop --command cargo build -p sunset-sync-quic`
Expected: clean.

- [ ] **Step 6: Clippy + commit**

Run: `nix develop --command cargo clippy -p sunset-sync-quic --all-features --all-targets -- -D warnings`
Run: `nix develop --command cargo fmt -p sunset-sync-quic`

```bash
git add crates/sunset-sync-quic/
git commit -m "$(cat <<'EOF'
sunset-sync-quic: QuicRawTransport (RawTransport impl)

bind() opens one UDP socket, wraps it in HolepunchSocket, builds a
quinn::Endpoint with a fresh self-signed cert. connect() / accept()
exchange Candidates via the signaler, holepunch via the coordinator,
then bring up a quinn::Connection (initiator role decided by pubkey
order). One persistent bidi stream + datagrams = RawConnection.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 9: Integration test — happy-path holepunch on loopback

**Files:**
- Create: `crates/sunset-sync-quic/tests/holepunch_loopback.rs`

- [ ] **Step 1: Write the test**

Create `crates/sunset-sync-quic/tests/holepunch_loopback.rs`:

```rust
//! End-to-end honest test: two QuicRawTransport instances over a
//! real UDP socket on 127.0.0.1, sharing a MemoryStore-backed
//! RelaySignaler. One side calls connect(); the other side's
//! accept() returns the matching connection. Both roundtrip a
//! reliable message and a datagram.
//!
//! No stub signaler, no probe-loop bypass, no test-only inspector
//! poking — CLAUDE.md debugging discipline.

use std::rc::Rc;
use std::sync::Arc;

use bytes::Bytes;
use tokio::task::LocalSet;

use sunset_core::{Ed25519Verifier, Identity, RelaySignaler, Room};
use sunset_core::crypto::constants::test_fast_params;
use sunset_store_memory::MemoryStore;
use sunset_sync::{PeerAddr, PeerId, RawConnection, RawTransport};
use sunset_sync_quic::QuicRawTransport;

#[tokio::test(flavor = "current_thread")]
async fn loopback_holepunch_reliable_and_datagram_roundtrip() {
    let local = LocalSet::new();
    local
        .run_until(async {
            let alice_id = Identity::from_secret_bytes(&[1u8; 32]);
            let bob_id = Identity::from_secret_bytes(&[2u8; 32]);
            let alice_pk = PeerId(alice_id.store_verifying_key());
            let bob_pk = PeerId(bob_id.store_verifying_key());

            let store = Arc::new(MemoryStore::new(Arc::new(Ed25519Verifier)));
            let room = Room::open_with_params("alpha", &test_fast_params()).expect("open room");
            let fp = room.fingerprint();

            let alice_signaler = RelaySignaler::new(alice_id, fp.to_hex(), &store);
            let bob_signaler = RelaySignaler::new(bob_id, fp.to_hex(), &store);

            let alice_t = QuicRawTransport::bind(
                alice_signaler,
                alice_pk.clone(),
                vec![], // no STUN — loopback test
            )
            .await
            .expect("alice bind");
            let bob_t = QuicRawTransport::bind(bob_signaler, bob_pk.clone(), vec![])
                .await
                .expect("bob bind");

            let bob_addr = PeerAddr::new(Bytes::from(format!(
                "quic://{}",
                hex::encode(bob_pk.verifying_key().as_bytes())
            )));

            // Alice connects; Bob accepts.
            let (a_conn, b_conn) = tokio::join!(alice_t.connect(bob_addr), bob_t.accept());
            let a_conn = a_conn.expect("alice connect");
            let b_conn = b_conn.expect("bob accept");

            a_conn
                .send_reliable(Bytes::from_static(b"hello bob"))
                .await
                .expect("alice send_reliable");
            let got = b_conn.recv_reliable().await.expect("bob recv_reliable");
            assert_eq!(got.as_ref(), b"hello bob");

            b_conn
                .send_unreliable(Bytes::from_static(b"dgram"))
                .await
                .expect("bob send_unreliable");
            let dg = a_conn.recv_unreliable().await.expect("alice recv_unreliable");
            assert_eq!(dg.as_ref(), b"dgram");

            a_conn.close().await.expect("close alice");
            b_conn.close().await.expect("close bob");
        })
        .await;
}
```

The `Room::open_with_params` / `Ed25519Verifier` / `RelaySignaler` paths match the pattern in `crates/sunset-core/src/signaling.rs` tests — copy that wiring. If `sunset_core::crypto::constants::test_fast_params` is not exported, find the actual public name (it's a v1 helper used in `multi_room_tests`).

- [ ] **Step 2: Run the test**

Run: `nix develop --command cargo test -p sunset-sync-quic --test holepunch_loopback`
Expected: passes within ~5 s. If it doesn't, do **not** add sleeps or raise timeouts — apply `superpowers:systematic-debugging` per CLAUDE.md and fix the root cause.

- [ ] **Step 3: Clippy + commit**

Run: `nix develop --command cargo clippy -p sunset-sync-quic --all-features --all-targets -- -D warnings`

```bash
git add crates/sunset-sync-quic/
git commit -m "$(cat <<'EOF'
sunset-sync-quic: integration test — loopback holepunch end-to-end

Two QuicRawTransport instances on 127.0.0.1, sharing a MemoryStore-
backed RelaySignaler. Alice.connect() races Bob.accept(); both sides
roundtrip a reliable message and a QUIC datagram. Honest end-to-end —
no stub signaler, no probe-loop bypass, no internal-state polling.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 10: Integration test — simultaneous-open glare

**Files:**
- Create: `crates/sunset-sync-quic/tests/simultaneous_open.rs`

- [ ] **Step 1: Write the test**

Create `crates/sunset-sync-quic/tests/simultaneous_open.rs`:

```rust
//! Both sides call connect(quic://<other>) at the same time. The
//! pubkey tiebreak decides who is the QUIC client; the other side's
//! connect() observes the inbound and returns the matching connection.
//!
//! Implementation note: `QuicRawTransport::connect` rejects a second
//! call for a peer that already has an in-flight handshake. Under
//! true glare, the second call to connect() that races with an
//! inbound Candidates from the other side will register its per_peer
//! slot first, then the dispatcher sees the Candidates and routes
//! into that slot — so connect() succeeds.

use std::rc::Rc;
use std::sync::Arc;

use bytes::Bytes;
use tokio::task::LocalSet;

use sunset_core::{Ed25519Verifier, Identity, RelaySignaler, Room};
use sunset_core::crypto::constants::test_fast_params;
use sunset_store_memory::MemoryStore;
use sunset_sync::{PeerAddr, PeerId, RawConnection, RawTransport};
use sunset_sync_quic::QuicRawTransport;

#[tokio::test(flavor = "current_thread")]
async fn simultaneous_open_resolves_via_pubkey_tiebreak() {
    let local = LocalSet::new();
    local
        .run_until(async {
            let alice_id = Identity::from_secret_bytes(&[1u8; 32]);
            let bob_id = Identity::from_secret_bytes(&[2u8; 32]);
            let alice_pk = PeerId(alice_id.store_verifying_key());
            let bob_pk = PeerId(bob_id.store_verifying_key());

            let store = Arc::new(MemoryStore::new(Arc::new(Ed25519Verifier)));
            let room = Room::open_with_params("alpha", &test_fast_params()).unwrap();
            let fp = room.fingerprint();
            let alice_signaler = RelaySignaler::new(alice_id, fp.to_hex(), &store);
            let bob_signaler = RelaySignaler::new(bob_id, fp.to_hex(), &store);

            let alice_t = QuicRawTransport::bind(alice_signaler, alice_pk.clone(), vec![])
                .await
                .unwrap();
            let bob_t = QuicRawTransport::bind(bob_signaler, bob_pk.clone(), vec![])
                .await
                .unwrap();

            let bob_addr = PeerAddr::new(Bytes::from(format!(
                "quic://{}",
                hex::encode(bob_pk.verifying_key().as_bytes())
            )));
            let alice_addr = PeerAddr::new(Bytes::from(format!(
                "quic://{}",
                hex::encode(alice_pk.verifying_key().as_bytes())
            )));

            let (a_res, b_res) =
                tokio::join!(alice_t.connect(bob_addr), bob_t.connect(alice_addr));
            let a_conn = a_res.expect("alice connect");
            let b_conn = b_res.expect("bob connect");

            a_conn
                .send_reliable(Bytes::from_static(b"glare ok"))
                .await
                .unwrap();
            let got = b_conn.recv_reliable().await.unwrap();
            assert_eq!(got.as_ref(), b"glare ok");
        })
        .await;
}
```

- [ ] **Step 2: Run + commit**

Run: `nix develop --command cargo test -p sunset-sync-quic --test simultaneous_open`
Expected: passes.

```bash
git add crates/sunset-sync-quic/
git commit -m "$(cat <<'EOF'
sunset-sync-quic: integration test — simultaneous-open glare

Both peers call connect() at the same time. The pubkey tiebreak picks
the QUIC client; the dispatcher's per_peer registry routes the inbound
Candidates into the racing connect's slot, so both connect() calls
return the same underlying QUIC connection.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 11: Integration test — STUN-skipped flow

**Files:**
- Create: `crates/sunset-sync-quic/tests/stun_skipped.rs`

- [ ] **Step 1: Write the test**

```rust
//! With `stun_servers = vec![]`, discovery returns only local
//! interfaces. Two peers on 127.0.0.1 still complete the holepunch
//! end-to-end. Confirms the STUN-unreachable failure mode degrades
//! gracefully rather than erroring out.

// Same setup as holepunch_loopback.rs but explicitly demonstrates
// the path even on a host with no STUN connectivity. The loopback
// test already passes stun_servers = vec![]; this test is the same
// shape but documents the STUN-skipped intent and asserts the
// confirmed candidate IS a loopback addr. (Inspecting the confirmed
// addr requires exposing a debug accessor on QuicRawConnection.)

// Implementation: add a `pub fn remote_addr(&self) -> SocketAddr` on
// QuicRawConnection (delegating to quinn::Connection::remote_address)
// and assert it's loopback here.
```

Add to `crates/sunset-sync-quic/src/connection.rs`:

```rust
impl QuicRawConnection {
    pub fn remote_addr(&self) -> std::net::SocketAddr {
        self.connection.remote_address()
    }
}
```

And in `crates/sunset-sync-quic/src/lib.rs`, re-export this accessor (already public via `pub use`).

Then in the test:

```rust
let a_conn = alice_t.connect(bob_addr).await.unwrap();
assert!(a_conn.remote_addr().ip().is_loopback(), "expected loopback, got {:?}", a_conn.remote_addr());
```

(Full test body mirrors `holepunch_loopback.rs` with this single additional assertion.)

- [ ] **Step 2: Run + commit**

Run: `nix develop --command cargo test -p sunset-sync-quic --test stun_skipped`
Expected: passes.

```bash
git add crates/sunset-sync-quic/
git commit -m "$(cat <<'EOF'
sunset-sync-quic: integration test — STUN-skipped local-only flow

With stun_servers=[], discovery returns only local interface addrs.
Holepunch still converges end-to-end on loopback. Confirms the
working remote_addr is a loopback (not a public STUN-derived
reflexive), exercising the failure-mode-degradation path.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 12: Workspace clippy + fmt + test sweep

**Files:** none changed; sanity-check the whole tree.

- [ ] **Step 1: Run the full workspace clippy**

Run: `nix develop --command cargo clippy --workspace --all-features --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 2: Run fmt check**

Run: `nix develop --command cargo fmt --all --check`
Expected: clean.

- [ ] **Step 3: Run the clippy-allow ban**

Run: `./scripts/check-no-clippy-allow.sh`
Expected: clean (no `#[allow(clippy::...)]` introduced).

- [ ] **Step 4: Run the full workspace test suite**

Run: `nix develop --command cargo test --workspace --all-features`
Expected: every existing test still passes plus the new ones.

- [ ] **Step 5: If anything failed in Steps 1–4, fix root cause and re-run from the start of Task 12**

Do not suppress, do not skip, do not raise timeouts. CLAUDE.md verbatim.

- [ ] **Step 6: Once green, no commit needed (the prior task commits stand). Move to the stability gate.**

---

## Task 13: Push + open PR + stability gate

- [ ] **Step 1: Push the branch**

```bash
git push -u origin sunset-sync-quic
```

- [ ] **Step 2: Open the PR**

```bash
gh pr create --title "sunset-sync-quic: NAT-hole-punched QUIC transport" --body "$(cat <<'EOF'
## Summary

New crate `sunset-sync-quic`. Implements `sunset_sync::RawTransport` over a NAT-hole-punched UDP socket carrying QUIC. Uses `sunset-store` (via the existing `Signaler` abstraction) as the signaling side-channel to exchange candidate addresses — no upstream changes.

- Local interface enumeration + STUN-reflexive candidate discovery.
- Probe protocol (magic prefix `b"SnP1"`) multiplexed with QUIC on one socket via a custom `quinn::AsyncUdpSocket`.
- Probes: 250 ms cadence, first confirmed candidate wins, 5 s total budget.
- Per-process self-signed TLS cert; SPKI hash exchanged via signaler and pinned by the peer's rustls config.
- Dispatcher + per-peer registry mirrors `WebRtcRawTransport`.
- Three honest integration tests (loopback happy path, simultaneous-open glare, STUN-skipped). No mocks past the integration boundary.

Spec: `docs/superpowers/specs/2026-05-12-sunset-sync-quic-design.md`
Plan: `docs/superpowers/plans/2026-05-12-sunset-sync-quic.md`

## Test plan

- [x] `cargo test -p sunset-sync-quic --all-features`
- [x] `cargo test --workspace --all-features`
- [x] `cargo clippy --workspace --all-features --all-targets -- -D warnings`
- [x] `scripts/check-no-clippy-allow.sh`
- [ ] CI green 5 consecutive times (stability gate)

🤖 Generated with [Claude Code](https://claude.com/claude-code)
EOF
)"
```

- [ ] **Step 3: Watch initial CI**

Run: `gh pr checks --watch`

If red: invoke `superpowers:systematic-debugging`. CLAUDE.md verbatim — no shortcuts.

- [ ] **Step 4: Run `/review` on the PR**

In the Claude session, invoke `/review` against the newly opened PR. Address every actionable comment (apply or substantively reply); push fix-up commits as needed.

- [ ] **Step 5: Stability gate — 5× CI re-runs**

Once `/review` is clean and CI is green, trigger 5 consecutive full CI runs. All 5 must pass. If any fails, do not call it flaky — `superpowers:systematic-debugging`, fix root cause, restart the gate at 0/5.

```bash
# Trigger 5 reruns sequentially; each one must complete green.
for i in 1 2 3 4 5; do
  gh workflow run "Tests" --ref sunset-sync-quic
  # Wait for the new run to finish.
  sleep 30
  RUN_ID=$(gh run list --workflow="Tests" --branch=sunset-sync-quic --limit=1 --json databaseId -q '.[0].databaseId')
  gh run watch "$RUN_ID" --exit-status
done
```

(Verify the workflow name matches `.github/workflows/test.yml`'s `name:` field. Adjust accordingly.)

- [ ] **Step 6: Declare done**

The PR is done iff:

1. All required CI checks have passed 5 consecutive times on the final commit.
2. `/review` produces no actionable feedback unaddressed.
3. Every commit respects CLAUDE.md.
4. No destructive/suppression shortcuts were used.
5. There's no UI surface in this change → no Playwright e2e requirement.
6. The branch was created from a fresh worktree off latest `origin/master`.

Otherwise: post a status comment via `gh pr comment` explaining where we stopped, what was tried, and what the user needs to decide. Don't lower the bar.

---

## Self-review (planner)

**Spec coverage check** (skimming the spec section-by-section):

- "Crate" / "Layered model" → Task 1 (skeleton).
- "Wire layout (data plane)" → Tasks 7 (QuicRawConnection) + 8 (QuicRawTransport bringup).
- "Candidate discovery" → Task 4.
- "Candidate exchange (signaling)" → Task 8 Step 4 (run_handshake) + Step 3 (dispatcher).
- "Probe loop" → Task 6 (HolepunchCoordinator).
- "Multiplexing QUIC and probes" → Task 5 (HolepunchSocket).
- "QUIC handshake" → Task 8 Step 4 (initiator/responder, cert pinning).
- "RawTransport surface" / "Address format" → Task 8.
- "Dispatcher pattern" → Task 8 Step 3.
- "Failure modes" — STUN unreachable covered by Task 11; timeouts covered by Task 6 unit test + Task 8 budgets; QUIC handshake failure propagation covered by the integration tests' error paths.
- "Tests" — unit tests in Tasks 2, 3, 4, 6; integration in Tasks 9, 10, 11.

**Placeholder scan:**

- One genuine "decide based on doc inspection" instruction in Task 5 Step 3 (read quinn 0.11's actual `AsyncUdpSocket` signatures before finalizing). That is **required engineering judgment** rather than a placeholder for the engineer to fill in unspecified behavior — the conceptual structure is fully specified.
- One genuine "decide" in Task 8 Step 2 (the exact `quinn::Endpoint::new_with_abstract_socket` argument order may vary slightly between quinn point releases). Same category.
- No "TBD" / "TODO" / "fill in error handling" left as gaps.

**Type consistency:**

- `QuicSignal`, `Candidates`, `Probe`, `ProbeRole`, `MAGIC` declared in Task 2 and referenced consistently in Tasks 5, 6, 8.
- `SelfSignedCert` declared in Task 3 and referenced consistently in Task 8.
- `HolepunchSocket::new(udp, probe_tx)` declared in Task 5, referenced in Task 8 Step 1.
- `HolepunchCoordinator::new(...)` signature consistent across Task 6 + Task 8 Step 4.
- `QuicRawConnection::new(connection, send, recv)` consistent across Task 7 + Task 8.

No name drift.
