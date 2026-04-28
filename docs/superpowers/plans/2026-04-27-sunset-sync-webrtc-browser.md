# sunset-sync browser WebRTC transport (V1) — Implementation Plan

> **For agentic workers:** Use superpowers:executing-plans (or superpowers:subagent-driven-development) to execute this plan task-by-task.

**Goal:** Land V1 of the voice roadmap. After this plan: two browsers connected to a relay can call `Client::connect_direct(peer_pubkey)`, establish a WebRTC peer connection through Noise_KK-encrypted relay-mediated signaling, then continue chatting over the direct datachannel even if the relay subprocess is killed mid-conversation.

**Spec:** `docs/superpowers/specs/2026-04-27-sunset-sync-webrtc-browser-design.md`.

**Out of scope:** native WebRTC (relay-side); TURN servers; auto-upgrade; reconnection; renegotiation. Native stub for `cargo build --workspace` only.

---

## Architecture summary

```
Gleam UI → sunset-web-wasm Client.connect_direct(pubkey)
                ↓
                MultiTransport<NoiseTransport<WebSocketRawTransport>,    [primary: relay]
                               NoiseTransport<WebRtcRawTransport>>       [secondary: direct]
                                                                ↑
                                                                NEW
                                                                ↑ uses
                                                          RelaySignaler (lives in sunset-web-wasm)
                                                                ↑ implements
                                                          Signaler trait (NEW in sunset-sync)
                                                                ↑ wraps
                                                          existing SyncEngine pushing/pulling
                                                          Noise_KK-encrypted CRDT entries
                                                          named <room_fp>/webrtc/<from>/<to>/<seq>
```

Two layers of Noise, both `snow`-backed, same crate (`sunset-noise`):
- **Connection-layer Noise IK** (Plan C — existing) wraps the WebRTC datachannel bytes
- **Signaling-layer Noise KK** (NEW) wraps the SDP/ICE exchange in CRDT entries

---

## File structure

```
sunset/
├── Cargo.toml                                      # MODIFY: workspace add sunset-sync-webrtc-browser member
├── crates/
│   ├── sunset-sync/src/
│   │   ├── signaler.rs                             # NEW: Signaler trait + SignalMessage
│   │   ├── multi_transport.rs                      # NEW: MultiTransport<T1, T2>
│   │   └── lib.rs                                  # MODIFY: add modules + re-exports
│   ├── sunset-noise/src/
│   │   └── kk.rs                                   # NEW: Noise_KK helpers (parallel to handshake.rs's IK helpers)
│   ├── sunset-sync-webrtc-browser/                 # NEW
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs                              # cfg-gated re-exports
│   │       ├── stub.rs                             # native fallback
│   │       └── wasm.rs                             # WebRtcRawTransport + WebRtcRawConnection
│   ├── sunset-web-wasm/src/
│   │   ├── relay_signaler.rs                       # NEW: Signaler impl over SyncEngine
│   │   ├── client.rs                               # MODIFY: add connect_direct + peer_connection_mode
│   │   └── lib.rs                                  # MODIFY: re-exports
│   └── sunset-web-wasm/tests/
│       └── construct.rs                            # MODIFY: add WebRTC client test
├── web/src/sunset_web/
│   ├── sunset.gleam                                # MODIFY: add connect_direct + peer_connection_mode externals
│   ├── sunset.ffi.mjs                              # MODIFY: corresponding JS shims
│   └── sunset_web.gleam                            # MODIFY: per-peer connection-mode badge
└── web/e2e/
    └── kill_relay.spec.js                         # NEW: WebRTC + kill-relay headline test
```

---

## Tasks

### Task 1: `Signaler` trait + `SignalMessage` in sunset-sync

**Files:**
- Create: `crates/sunset-sync/src/signaler.rs`
- Modify: `crates/sunset-sync/src/lib.rs`

- [ ] **Step 1:** Create `crates/sunset-sync/src/signaler.rs`:

  ```rust
  //! Side-channel for transports that need an out-of-band exchange before
  //! data flow can begin (WebRTC SDP/ICE, future patterns).
  //!
  //! The trait is generic — it shovels opaque bytes between named peers.
  //! The transport that uses a Signaler defines its own wire format
  //! inside `payload`.

  use async_trait::async_trait;
  use bytes::Bytes;

  use crate::Result;
  use crate::types::PeerId;

  /// One signaling message exchanged between two named peers.
  #[derive(Clone, Debug)]
  pub struct SignalMessage {
      pub from: PeerId,
      pub to: PeerId,
      /// Per-(from,to) monotonic counter so receivers can dedupe + order.
      pub seq: u64,
      /// Opaque payload — the using transport defines the wire format.
      pub payload: Bytes,
  }

  #[async_trait(?Send)]
  pub trait Signaler: 'static {
      /// Send a signaling message to a remote peer.
      async fn send(&self, message: SignalMessage) -> Result<()>;

      /// Wait for the next inbound signaling message addressed to us.
      async fn recv(&self) -> Result<SignalMessage>;
  }
  ```

- [ ] **Step 2:** Add to `crates/sunset-sync/src/lib.rs` (alphabetical position):

  ```rust
  pub mod signaler;
  pub use signaler::{SignalMessage, Signaler};
  ```

- [ ] **Step 3:** Verify:
  ```
  nix develop --command cargo fmt -p sunset-sync
  nix develop --command cargo build -p sunset-sync
  nix develop --command cargo clippy -p sunset-sync --all-targets -- -D warnings
  ```

- [ ] **Step 4:** Commit:
  ```
  git add crates/sunset-sync/src/signaler.rs crates/sunset-sync/src/lib.rs
  git commit -m "Add Signaler trait + SignalMessage for transports needing out-of-band exchange"
  ```

---

### Task 2: Noise_KK module in sunset-noise

**Files:**
- Create: `crates/sunset-noise/src/kk.rs`
- Modify: `crates/sunset-noise/src/lib.rs`
- Modify: `crates/sunset-noise/src/pattern.rs`

- [ ] **Step 1:** Add to `crates/sunset-noise/src/pattern.rs`:

  ```rust
  /// Noise pattern used by signaling exchanges (e.g., WebRTC SDP/ICE).
  /// KK = both statics known a priori. Full PFS via mutual ephemerals.
  pub const NOISE_KK_PATTERN: &str = "Noise_KK_25519_XChaChaPoly_BLAKE2b";
  ```

  And add to that file's `tests` mod:

  ```rust
  #[test]
  fn kk_pattern_is_pinned() {
      assert_eq!(NOISE_KK_PATTERN, "Noise_KK_25519_XChaChaPoly_BLAKE2b");
  }
  ```

- [ ] **Step 2:** Create `crates/sunset-noise/src/kk.rs`. The structure mirrors `handshake.rs`'s IK helpers but for the 2-message KK pattern + a transport-mode `KkSession` that ratchets per-message.

  ```rust
  //! Noise_KK helpers for pairwise signaling exchanges with full PFS.
  //!
  //! Used by WebRTC signaling (and future patterns) where both peers
  //! already know each other's static X25519 keys (derived from their
  //! Ed25519 identities per `identity::ed25519_seed_to_x25519_secret`).

  use snow::{Builder, HandshakeState, TransportState};
  use zeroize::Zeroizing;

  use crate::error::{Error, Result};
  use crate::pattern::NOISE_KK_PATTERN;

  /// Initiator side of a KK handshake. Build it with both statics, write
  /// message 1 (carries the offer payload), then read message 2 to finish
  /// + transition to transport mode.
  pub struct KkInitiator {
      hs: HandshakeState,
  }

  impl KkInitiator {
      /// `local_x25519_secret`: derived from this peer's Ed25519 secret seed.
      /// `remote_x25519_pub`: derived from the remote peer's Ed25519 pubkey.
      pub fn new(
          local_x25519_secret: &Zeroizing<[u8; 32]>,
          remote_x25519_pub: &[u8; 32],
      ) -> Result<Self> {
          let hs = Builder::new(
              NOISE_KK_PATTERN
                  .parse()
                  .map_err(|e| Error::Snow(format!("{e:?}")))?,
          )
          .local_private_key(&local_x25519_secret[..])
          .remote_public_key(remote_x25519_pub)
          .build_initiator()?;
          Ok(Self { hs })
      }

      /// Write the first handshake message with `payload` encrypted inside.
      /// Returns the wire bytes (≤ 65535).
      pub fn write_message_1(&mut self, payload: &[u8]) -> Result<Vec<u8>> {
          let mut buf = vec![0u8; payload.len() + 256];
          let n = self.hs.write_message(payload, &mut buf)?;
          buf.truncate(n);
          Ok(buf)
      }

      /// Read the responder's message 2 + return decrypted payload.
      /// Consumes the initiator into a session.
      pub fn read_message_2(mut self, msg: &[u8]) -> Result<(Vec<u8>, KkSession)> {
          let mut buf = vec![0u8; msg.len()];
          let n = self.hs.read_message(msg, &mut buf)?;
          buf.truncate(n);
          let transport = self.hs.into_transport_mode()?;
          Ok((buf, KkSession { transport }))
      }
  }

  /// Responder side of a KK handshake.
  pub struct KkResponder {
      hs: HandshakeState,
  }

  impl KkResponder {
      pub fn new(
          local_x25519_secret: &Zeroizing<[u8; 32]>,
          remote_x25519_pub: &[u8; 32],
      ) -> Result<Self> {
          let hs = Builder::new(
              NOISE_KK_PATTERN
                  .parse()
                  .map_err(|e| Error::Snow(format!("{e:?}")))?,
          )
          .local_private_key(&local_x25519_secret[..])
          .remote_public_key(remote_x25519_pub)
          .build_responder()?;
          Ok(Self { hs })
      }

      /// Read the initiator's message 1 + return decrypted payload.
      pub fn read_message_1(&mut self, msg: &[u8]) -> Result<Vec<u8>> {
          let mut buf = vec![0u8; msg.len()];
          let n = self.hs.read_message(msg, &mut buf)?;
          buf.truncate(n);
          Ok(buf)
      }

      /// Write message 2 with `payload` encrypted inside. Consumes the
      /// responder into a session.
      pub fn write_message_2(mut self, payload: &[u8]) -> Result<(Vec<u8>, KkSession)> {
          let mut buf = vec![0u8; payload.len() + 256];
          let n = self.hs.write_message(payload, &mut buf)?;
          buf.truncate(n);
          let transport = self.hs.into_transport_mode()?;
          Ok((buf, KkSession { transport }))
      }
  }

  /// Post-handshake transport state. Encrypts/decrypts subsequent messages
  /// with ratcheting keys; full PFS preserved per message.
  pub struct KkSession {
      transport: TransportState,
  }

  impl KkSession {
      pub fn encrypt(&mut self, plaintext: &[u8]) -> Result<Vec<u8>> {
          let mut buf = vec![0u8; plaintext.len() + 16];
          let n = self.transport.write_message(plaintext, &mut buf)?;
          buf.truncate(n);
          Ok(buf)
      }

      pub fn decrypt(&mut self, ciphertext: &[u8]) -> Result<Vec<u8>> {
          let mut buf = vec![0u8; ciphertext.len()];
          let n = self.transport.read_message(ciphertext, &mut buf)?;
          buf.truncate(n);
          Ok(buf)
      }
  }

  #[cfg(test)]
  mod tests {
      use super::*;
      use crate::identity::ed25519_seed_to_x25519_secret;

      fn pub_for(seed: &[u8; 32]) -> [u8; 32] {
          let secret = ed25519_seed_to_x25519_secret(seed);
          use curve25519_dalek::{MontgomeryPoint, scalar::Scalar};
          MontgomeryPoint::mul_base(&Scalar::from_bytes_mod_order(*secret)).to_bytes()
      }

      #[test]
      fn kk_handshake_roundtrip_with_payloads() {
          let alice_seed = [1u8; 32];
          let bob_seed = [2u8; 32];
          let alice_secret = ed25519_seed_to_x25519_secret(&alice_seed);
          let bob_secret = ed25519_seed_to_x25519_secret(&bob_seed);
          let alice_pub = pub_for(&alice_seed);
          let bob_pub = pub_for(&bob_seed);

          let mut init = KkInitiator::new(&alice_secret, &bob_pub).unwrap();
          let msg1 = init.write_message_1(b"offer payload").unwrap();

          let mut resp = KkResponder::new(&bob_secret, &alice_pub).unwrap();
          let recv1 = resp.read_message_1(&msg1).unwrap();
          assert_eq!(recv1, b"offer payload");

          let (msg2, mut bob_session) = resp.write_message_2(b"answer payload").unwrap();
          let (recv2, mut alice_session) = init.read_message_2(&msg2).unwrap();
          assert_eq!(recv2, b"answer payload");

          // Subsequent transport-mode messages each direction.
          let ct1 = alice_session.encrypt(b"ice candidate 1").unwrap();
          let pt1 = bob_session.decrypt(&ct1).unwrap();
          assert_eq!(pt1, b"ice candidate 1");

          let ct2 = bob_session.encrypt(b"ice candidate 2").unwrap();
          let pt2 = alice_session.decrypt(&ct2).unwrap();
          assert_eq!(pt2, b"ice candidate 2");
      }

      #[test]
      fn kk_rejects_wrong_static() {
          let alice_seed = [1u8; 32];
          let bob_seed = [2u8; 32];
          let mallory_seed = [99u8; 32];

          let alice_secret = ed25519_seed_to_x25519_secret(&alice_seed);
          let mallory_secret = ed25519_seed_to_x25519_secret(&mallory_seed);
          let bob_pub = pub_for(&bob_seed);
          let alice_pub = pub_for(&alice_seed);

          let mut init = KkInitiator::new(&alice_secret, &bob_pub).unwrap();
          let msg1 = init.write_message_1(b"offer").unwrap();

          // Bob expects message from alice but mallory is reading. Their
          // KK responder is built with mallory's static + alice's pub —
          // the static-static DH won't match what alice's initiator did,
          // so the handshake decryption fails.
          let mut wrong = KkResponder::new(&mallory_secret, &alice_pub).unwrap();
          assert!(wrong.read_message_1(&msg1).is_err());
      }
  }
  ```

- [ ] **Step 3:** Add to `crates/sunset-noise/src/lib.rs`:
  ```rust
  pub mod kk;
  pub use kk::{KkInitiator, KkResponder, KkSession};
  pub use pattern::NOISE_KK_PATTERN;
  ```

- [ ] **Step 4:** Verify:
  ```
  nix develop --command cargo fmt -p sunset-noise
  nix develop --command cargo test -p sunset-noise kk::tests
  nix develop --command cargo clippy -p sunset-noise --all-targets -- -D warnings
  nix develop --command cargo build -p sunset-noise --target wasm32-unknown-unknown --lib
  ```

  Expect 2 KK tests pass + existing tests still pass + wasm-clean.

- [ ] **Step 5:** Commit:
  ```
  git add crates/sunset-noise/src/kk.rs crates/sunset-noise/src/lib.rs crates/sunset-noise/src/pattern.rs
  git commit -m "Add Noise_KK helpers for pairwise signaling with full PFS"
  ```

---

### Task 3: `MultiTransport<T1, T2>` adapter in sunset-sync

**Files:**
- Create: `crates/sunset-sync/src/multi_transport.rs`
- Modify: `crates/sunset-sync/src/lib.rs`

- [ ] **Step 1:** Create `crates/sunset-sync/src/multi_transport.rs`:

  ```rust
  //! Compose two transports into one. The wrapped `SyncEngine` sees a
  //! single Transport; routing across the two underlying transports is
  //! invisible to it.
  //!
  //! Routing rule: PeerAddr's URL prefix decides which underlying
  //! transport gets the dial.
  //!   - `ws://...` or `wss://...` → primary
  //!   - `webrtc://...`            → secondary
  //!   - other                     → Error::Transport("multi: unknown
  //!                                  scheme...")
  //!
  //! Inbound (`accept`): both transports race; whichever yields first
  //! wins. The connection's `peer_id()` carries the per-transport
  //! authentication identity.

  use async_trait::async_trait;
  use bytes::Bytes;
  use futures::future::FutureExt;

  use crate::error::{Error, Result};
  use crate::transport::{Transport, TransportConnection};
  use crate::types::{PeerAddr, PeerId};

  pub struct MultiTransport<T1: Transport, T2: Transport> {
      primary: T1,
      secondary: T2,
  }

  impl<T1: Transport, T2: Transport> MultiTransport<T1, T2> {
      pub fn new(primary: T1, secondary: T2) -> Self {
          Self { primary, secondary }
      }
  }

  #[async_trait(?Send)]
  impl<T1, T2> Transport for MultiTransport<T1, T2>
  where
      T1: Transport,
      T1::Connection: 'static,
      T2: Transport,
      T2::Connection: 'static,
  {
      type Connection = MultiConnection<T1::Connection, T2::Connection>;

      async fn connect(&self, addr: PeerAddr) -> Result<Self::Connection> {
          let s = std::str::from_utf8(addr.as_bytes())
              .map_err(|e| Error::Transport(format!("multi: addr not utf-8: {e}")))?;
          if s.starts_with("ws://") || s.starts_with("wss://") {
              Ok(MultiConnection::Primary(self.primary.connect(addr).await?))
          } else if s.starts_with("webrtc://") {
              Ok(MultiConnection::Secondary(self.secondary.connect(addr).await?))
          } else {
              Err(Error::Transport(format!(
                  "multi: unknown scheme in {s} (expected ws://, wss://, or webrtc://)"
              )))
          }
      }

      async fn accept(&self) -> Result<Self::Connection> {
          let primary = self.primary.accept().fuse();
          let secondary = self.secondary.accept().fuse();
          futures::pin_mut!(primary, secondary);

          futures::select! {
              p = primary => Ok(MultiConnection::Primary(p?)),
              s = secondary => Ok(MultiConnection::Secondary(s?)),
          }
      }
  }

  pub enum MultiConnection<C1, C2> {
      Primary(C1),
      Secondary(C2),
  }

  #[async_trait(?Send)]
  impl<C1, C2> TransportConnection for MultiConnection<C1, C2>
  where
      C1: TransportConnection,
      C2: TransportConnection,
  {
      async fn send_reliable(&self, bytes: Bytes) -> Result<()> {
          match self {
              MultiConnection::Primary(c) => c.send_reliable(bytes).await,
              MultiConnection::Secondary(c) => c.send_reliable(bytes).await,
          }
      }

      async fn recv_reliable(&self) -> Result<Bytes> {
          match self {
              MultiConnection::Primary(c) => c.recv_reliable().await,
              MultiConnection::Secondary(c) => c.recv_reliable().await,
          }
      }

      async fn send_unreliable(&self, bytes: Bytes) -> Result<()> {
          match self {
              MultiConnection::Primary(c) => c.send_unreliable(bytes).await,
              MultiConnection::Secondary(c) => c.send_unreliable(bytes).await,
          }
      }

      async fn recv_unreliable(&self) -> Result<Bytes> {
          match self {
              MultiConnection::Primary(c) => c.recv_unreliable().await,
              MultiConnection::Secondary(c) => c.recv_unreliable().await,
          }
      }

      fn peer_id(&self) -> PeerId {
          match self {
              MultiConnection::Primary(c) => c.peer_id(),
              MultiConnection::Secondary(c) => c.peer_id(),
          }
      }

      async fn close(&self) -> Result<()> {
          match self {
              MultiConnection::Primary(c) => c.close().await,
              MultiConnection::Secondary(c) => c.close().await,
          }
      }
  }
  ```

- [ ] **Step 2:** Add to `crates/sunset-sync/src/lib.rs`:
  ```rust
  pub mod multi_transport;
  pub use multi_transport::{MultiConnection, MultiTransport};
  ```

- [ ] **Step 3:** Verify:
  ```
  nix develop --command cargo fmt -p sunset-sync
  nix develop --command cargo build -p sunset-sync
  nix develop --command cargo clippy -p sunset-sync --all-targets -- -D warnings
  ```

- [ ] **Step 4:** Commit:
  ```
  git add crates/sunset-sync/src/multi_transport.rs crates/sunset-sync/src/lib.rs
  git commit -m "Add MultiTransport adapter: route by URL scheme"
  ```

---

### Task 4: Scaffold `sunset-sync-webrtc-browser` crate

**Files:**
- Modify: `Cargo.toml` (workspace root)
- Create: `crates/sunset-sync-webrtc-browser/Cargo.toml`
- Create: `crates/sunset-sync-webrtc-browser/src/{lib,stub,wasm}.rs`

- [ ] **Step 1:** Add to root `Cargo.toml`'s `[workspace.dependencies]` (alphabetical):
  ```toml
  sunset-sync-webrtc-browser = { path = "crates/sunset-sync-webrtc-browser" }
  ```
  Add `crates/sunset-sync-webrtc-browser` to `[workspace] members`.

  Add web-sys features needed (extend the existing `web-sys` workspace dep):
  ```toml
  web-sys = { version = "0.3", features = [
    "WebSocket", "MessageEvent", "BinaryType", "CloseEvent", "Event", "console",
    "RtcPeerConnection", "RtcConfiguration", "RtcIceServer",
    "RtcSessionDescription", "RtcSessionDescriptionInit", "RtcSdpType",
    "RtcDataChannel", "RtcDataChannelInit", "RtcDataChannelEvent",
    "RtcIceCandidate", "RtcIceCandidateInit", "RtcPeerConnectionIceEvent",
    "RtcOfferOptions", "RtcAnswerOptions",
  ] }
  ```

- [ ] **Step 2:** Create `crates/sunset-sync-webrtc-browser/Cargo.toml`:

  ```toml
  [package]
  name = "sunset-sync-webrtc-browser"
  version.workspace = true
  edition.workspace = true
  license.workspace = true
  rust-version.workspace = true

  [lib]
  crate-type = ["cdylib", "rlib"]

  [lints]
  workspace = true

  [dependencies]
  async-trait.workspace = true
  bytes.workspace = true
  futures.workspace = true
  sunset-sync.workspace = true
  thiserror.workspace = true

  [target.'cfg(target_arch = "wasm32")'.dependencies]
  js-sys.workspace = true
  wasm-bindgen.workspace = true
  wasm-bindgen-futures.workspace = true
  web-sys.workspace = true

  [target.'cfg(target_arch = "wasm32")'.dev-dependencies]
  wasm-bindgen-test.workspace = true
  ```

- [ ] **Step 3:** Create `crates/sunset-sync-webrtc-browser/src/lib.rs`:

  ```rust
  //! Browser-side `sunset_sync::RawTransport` over `web_sys::RtcPeerConnection`
  //! datachannel.
  //!
  //! Pair with `sunset_noise::NoiseTransport<R>` (Plan C) for the
  //! authenticated encrypted layer over the bytes pipe.
  //!
  //! See `docs/superpowers/specs/2026-04-27-sunset-sync-webrtc-browser-design.md`.

  #[cfg(target_arch = "wasm32")]
  mod wasm;
  #[cfg(target_arch = "wasm32")]
  pub use wasm::{WebRtcRawConnection, WebRtcRawTransport};

  #[cfg(not(target_arch = "wasm32"))]
  mod stub;
  #[cfg(not(target_arch = "wasm32"))]
  pub use stub::{WebRtcRawConnection, WebRtcRawTransport};
  ```

- [ ] **Step 4:** Create `crates/sunset-sync-webrtc-browser/src/stub.rs` (mirror sunset-sync-ws-browser's stub, returning `Error::Transport("native stub")` on every call). Include a no-op `WebRtcRawTransport::new(_signaler, _local_peer)` constructor matching the wasm impl's signature.

- [ ] **Step 5:** Create `crates/sunset-sync-webrtc-browser/src/wasm.rs` (placeholder, populated in Tasks 5+6+7):
  ```rust
  //! Real wasm32 implementation. Populated in subsequent tasks.

  pub struct WebRtcRawTransport;
  pub struct WebRtcRawConnection;
  ```

- [ ] **Step 6:** Verify both build paths:
  ```
  nix develop --command cargo fmt -p sunset-sync-webrtc-browser
  nix develop --command cargo build -p sunset-sync-webrtc-browser
  nix develop --command cargo build -p sunset-sync-webrtc-browser --target wasm32-unknown-unknown
  ```

- [ ] **Step 7:** Commit:
  ```
  git add Cargo.toml Cargo.lock crates/sunset-sync-webrtc-browser/
  git commit -m "Scaffold sunset-sync-webrtc-browser crate"
  ```

---

### Task 5: WebRtcRawTransport — outbound (`connect`) flow

**Files:**
- Modify: `crates/sunset-sync-webrtc-browser/src/wasm.rs`
- Modify: `crates/sunset-sync-webrtc-browser/src/stub.rs`

- [ ] **Step 1:** Replace `wasm.rs` with the connect-side implementation. The structure:

  - `WebRtcRawTransport::new(signaler, local_peer, ice_servers)` constructor
  - `connect(addr)`: parse PeerAddr (`webrtc://<base64-peer-id>`), create RtcPeerConnection, create datachannel, generate offer, send SignalMessage with payload = postcard(WebRtcSignalKind::Offer(sdp)) via signaler, await answer + ICE, await datachannel `open` event, return `WebRtcRawConnection`
  - `accept()`: spawn an inbound-handling task that drains `signaler.recv()` for offers; each offer creates a new RtcPeerConnection in responder mode

  Full code:

  ```rust
  use std::cell::RefCell;
  use std::rc::Rc;

  use async_trait::async_trait;
  use bytes::Bytes;
  use futures::channel::{mpsc, oneshot};
  use futures::{StreamExt, SinkExt};
  use js_sys::{ArrayBuffer, Reflect, Uint8Array};
  use serde::{Deserialize, Serialize};
  use wasm_bindgen::prelude::*;
  use wasm_bindgen::JsCast;
  use wasm_bindgen_futures::JsFuture;
  use web_sys::{
      BinaryType, MessageEvent, RtcConfiguration, RtcDataChannel, RtcDataChannelEvent,
      RtcDataChannelInit, RtcIceCandidate, RtcIceCandidateInit, RtcIceServer,
      RtcPeerConnection, RtcPeerConnectionIceEvent, RtcSdpType, RtcSessionDescriptionInit,
  };

  use sunset_sync::{
      Error, PeerAddr, PeerId, RawConnection, RawTransport, Result, SignalMessage,
      Signaler,
  };
  use sunset_store::VerifyingKey;

  #[derive(Serialize, Deserialize)]
  enum WebRtcSignalKind {
      Offer(String),
      Answer(String),
      IceCandidate(String),
  }

  pub struct WebRtcRawTransport {
      signaler: Rc<dyn Signaler>,
      local_peer: PeerId,
      ice_urls: Vec<String>,
      // Inbound accept side: spawn-once on first accept().
      inbound_started: RefCell<bool>,
      inbound_rx: RefCell<Option<mpsc::UnboundedReceiver<WebRtcRawConnection>>>,
  }

  impl WebRtcRawTransport {
      /// `ice_urls` should typically contain at least one STUN server,
      /// e.g. `["stun:stun.l.google.com:19302".into()]`.
      pub fn new(
          signaler: Rc<dyn Signaler>,
          local_peer: PeerId,
          ice_urls: Vec<String>,
      ) -> Self {
          let (inbound_tx, inbound_rx) = mpsc::unbounded::<WebRtcRawConnection>();
          // Park the tx side on the struct so we can use it from the
          // accept-loop spawn.
          drop(inbound_tx); // simplification: actual implementation keeps a
                            // handle and starts the inbound loop on first accept().
                            // See Task 6 for the full inbound machinery.
          Self {
              signaler,
              local_peer,
              ice_urls,
              inbound_started: RefCell::new(false),
              inbound_rx: RefCell::new(Some(inbound_rx)),
          }
      }
  }

  /// Parse `webrtc://<base64-peer-id>` and return the PeerId bytes.
  fn parse_addr_peer_id(addr: &PeerAddr) -> Result<PeerId> {
      let s = std::str::from_utf8(addr.as_bytes())
          .map_err(|e| Error::Transport(format!("addr not utf-8: {e}")))?;
      let suffix = s
          .strip_prefix("webrtc://")
          .ok_or_else(|| Error::Transport(format!("addr not webrtc://: {s}")))?;
      let bytes = base64_decode(suffix)
          .ok_or_else(|| Error::Transport(format!("base64 decode failed: {suffix}")))?;
      Ok(PeerId(VerifyingKey::new(Bytes::from(bytes))))
  }

  fn base64_decode(s: &str) -> Option<Vec<u8>> {
      // Use js_sys::atob via web-sys; or pull in `base64` dep. For
      // simplicity here, use `js_sys::atob`.
      use wasm_bindgen::JsValue;
      let res = js_sys::Function::new_no_args(&format!("return atob('{}');", s))
          .call0(&JsValue::NULL)
          .ok()?;
      let s2: String = res.as_string()?;
      Some(s2.bytes().collect())
  }

  #[async_trait(?Send)]
  impl RawTransport for WebRtcRawTransport {
      type Connection = WebRtcRawConnection;

      async fn connect(&self, addr: PeerAddr) -> Result<Self::Connection> {
          let remote_peer = parse_addr_peer_id(&addr)?;

          // 1. Build RtcPeerConnection with our ICE config.
          let mut config = RtcConfiguration::new();
          let servers = js_sys::Array::new();
          for url in &self.ice_urls {
              let mut s = RtcIceServer::new();
              let urls = js_sys::Array::new();
              urls.push(&JsValue::from_str(url));
              s.urls(&urls);
              servers.push(&s);
          }
          config.ice_servers(&servers);
          let pc = RtcPeerConnection::new_with_configuration(&config)
              .map_err(|e| Error::Transport(format!("RtcPeerConnection: {e:?}")))?;

          // 2. Create reliable datachannel (initiator side).
          let mut dc_init = RtcDataChannelInit::new();
          dc_init.ordered(true);
          let dc = pc.create_data_channel_with_data_channel_dict("sunset-sync", &dc_init);
          dc.set_binary_type(BinaryType::Arraybuffer);

          // 3. Wire up channels: ICE candidates collection, datachannel open.
          let (mut ice_tx, ice_rx) = mpsc::unbounded::<String>();
          let (open_tx, open_rx) = oneshot::channel::<()>();
          let (msg_tx, msg_rx) = mpsc::unbounded::<Bytes>();

          let on_ice = Closure::<dyn FnMut(RtcPeerConnectionIceEvent)>::new(
              move |ev: RtcPeerConnectionIceEvent| {
                  if let Some(c) = ev.candidate() {
                      let cand_str = js_sys::JSON::stringify(&c.to_json())
                          .ok()
                          .and_then(|s| s.as_string())
                          .unwrap_or_default();
                      let _ = ice_tx.unbounded_send(cand_str);
                  }
              },
          );
          pc.set_onicecandidate(Some(on_ice.as_ref().unchecked_ref()));

          let open_tx_cell = Rc::new(RefCell::new(Some(open_tx)));
          let on_open_ref = open_tx_cell.clone();
          let on_open = Closure::<dyn FnMut(JsValue)>::new(move |_| {
              if let Some(tx) = on_open_ref.borrow_mut().take() {
                  let _ = tx.send(());
              }
          });
          dc.set_onopen(Some(on_open.as_ref().unchecked_ref()));

          let on_msg = Closure::<dyn FnMut(MessageEvent)>::new(move |ev: MessageEvent| {
              let data = ev.data();
              if let Ok(buf) = data.dyn_into::<ArrayBuffer>() {
                  let arr = Uint8Array::new(&buf);
                  let mut bytes = vec![0u8; arr.length() as usize];
                  arr.copy_to(&mut bytes);
                  let _ = msg_tx.unbounded_send(Bytes::from(bytes));
              }
          });
          dc.set_onmessage(Some(on_msg.as_ref().unchecked_ref()));

          // 4. Create offer + setLocalDescription.
          let offer_promise = pc.create_offer();
          let offer = JsFuture::from(offer_promise)
              .await
              .map_err(|e| Error::Transport(format!("createOffer: {e:?}")))?;
          let sdp: String = Reflect::get(&offer, &JsValue::from_str("sdp"))
              .ok()
              .and_then(|v| v.as_string())
              .ok_or_else(|| Error::Transport("offer.sdp missing".into()))?;
          let mut sd = RtcSessionDescriptionInit::new(RtcSdpType::Offer);
          sd.sdp(&sdp);
          JsFuture::from(pc.set_local_description(&sd))
              .await
              .map_err(|e| Error::Transport(format!("setLocalDescription: {e:?}")))?;

          // 5. Send the offer via the signaler.
          let payload = postcard::to_stdvec(&WebRtcSignalKind::Offer(sdp))
              .map_err(|e| Error::Transport(format!("postcard: {e}")))?;
          self.signaler
              .send(SignalMessage {
                  from: self.local_peer.clone(),
                  to: remote_peer.clone(),
                  seq: 0,
                  payload: Bytes::from(payload),
              })
              .await?;

          // 6. Drive an offer-side loop: receive answer + ICE candidates from
          // the signaler; forward our ICE candidates via the signaler. Run
          // concurrently with awaiting the open event.
          let pc_for_loop = pc.clone();
          let signaler_for_loop = self.signaler.clone();
          let local_peer = self.local_peer.clone();
          let remote_peer_for_loop = remote_peer.clone();
          wasm_bindgen_futures::spawn_local(async move {
              // Forward our ICE candidates.
              let mut ice_rx = ice_rx;
              let mut seq: u64 = 1;
              loop {
                  futures::select! {
                      cand = ice_rx.next().fuse() => {
                          if let Some(c) = cand {
                              let p = postcard::to_stdvec(&WebRtcSignalKind::IceCandidate(c))
                                  .unwrap_or_default();
                              let _ = signaler_for_loop.send(SignalMessage {
                                  from: local_peer.clone(),
                                  to: remote_peer_for_loop.clone(),
                                  seq,
                                  payload: Bytes::from(p),
                              }).await;
                              seq += 1;
                          }
                      }
                      // (Receiving from signaler for answer + remote ICE
                      // candidates is interleaved here; the actual impl uses
                      // a per-connection inbound queue keyed by remote
                      // PeerId. See Task 6.)
                      complete => break,
                  }
              }
          });

          // 7. Await the datachannel's open event.
          open_rx
              .await
              .map_err(|_| Error::Transport("datachannel open never fired".into()))?;

          Ok(WebRtcRawConnection {
              dc,
              rx: RefCell::new(msg_rx),
              peer_id: remote_peer,
              _on_ice: on_ice,
              _on_open: on_open,
              _on_msg: on_msg,
          })
      }

      async fn accept(&self) -> Result<Self::Connection> {
          // Inbound machinery comes in Task 6. For now, never resolve.
          std::future::pending::<()>().await;
          unreachable!()
      }
  }

  pub struct WebRtcRawConnection {
      dc: RtcDataChannel,
      rx: RefCell<mpsc::UnboundedReceiver<Bytes>>,
      peer_id: PeerId,
      _on_ice: Closure<dyn FnMut(RtcPeerConnectionIceEvent)>,
      _on_open: Closure<dyn FnMut(JsValue)>,
      _on_msg: Closure<dyn FnMut(MessageEvent)>,
  }

  #[async_trait(?Send)]
  impl RawConnection for WebRtcRawConnection {
      async fn send_reliable(&self, bytes: Bytes) -> Result<()> {
          self.dc
              .send_with_u8_array(&bytes)
              .map_err(|e| Error::Transport(format!("dc.send: {e:?}")))
      }

      async fn recv_reliable(&self) -> Result<Bytes> {
          use std::pin::Pin;
          use futures::Stream;
          use futures::future::poll_fn;
          poll_fn(|cx| {
              let mut rx = self.rx.borrow_mut();
              Stream::poll_next(Pin::new(&mut *rx), cx).map(|opt| {
                  opt.ok_or_else(|| Error::Transport("dc closed".into()))
              })
          })
          .await
      }

      async fn send_unreliable(&self, _: Bytes) -> Result<()> {
          // v1 reliable-only. Voice fills this in.
          Err(Error::Transport(
              "webrtc: unreliable channel not implemented in v1".into(),
          ))
      }

      async fn recv_unreliable(&self) -> Result<Bytes> {
          Err(Error::Transport(
              "webrtc: unreliable channel not implemented in v1".into(),
          ))
      }

      async fn close(&self) -> Result<()> {
          self.dc.close();
          Ok(())
      }
  }
  ```

  **Note on inbound handling:** The connect side needs to receive answer + remote ICE candidates from the signaler, but the signaler is shared with the accept side too. This requires per-connection demultiplexing keyed by remote PeerId. That logic lands in Task 6. The connect flow above will fail to complete the handshake until Task 6 lands; that's expected.

- [ ] **Step 2:** Stub side: matching constructor signature for native, returning Err on calls.

- [ ] **Step 3:** Verify wasm build:
  ```
  nix develop --command cargo build -p sunset-sync-webrtc-browser --target wasm32-unknown-unknown
  ```

  Expect compile success. Don't run tests yet (handshake won't complete).

- [ ] **Step 4:** Commit:
  ```
  git add crates/sunset-sync-webrtc-browser/src/wasm.rs crates/sunset-sync-webrtc-browser/src/stub.rs
  git commit -m "WebRtcRawTransport: connect flow + offer creation (incomplete handshake)"
  ```

---

### Task 6: Inbound dispatch + per-connection signaling demux

**Files:**
- Modify: `crates/sunset-sync-webrtc-browser/src/wasm.rs`

The connect flow from Task 5 needs to receive answers + remote ICE candidates. The accept flow needs to receive offers. Both sides share one signaler. Solution: a single dispatcher task drains `signaler.recv()` and routes by remote PeerId + signal kind.

- [ ] **Step 1:** Refactor `WebRtcRawTransport` to spawn (lazily, on first connect/accept) a `signal_dispatcher` task that:
  - Drains `signaler.recv()` in a loop
  - For each message, decodes the postcard payload as `WebRtcSignalKind`
  - Routes:
    - `Offer(_)` → push onto an "inbound offers" queue (consumed by accept())
    - `Answer(_)` or `IceCandidate(_)` → look up the per-connection inbound channel keyed by `(from_peer)`; push there

  Per-connection state: a `HashMap<PeerId, mpsc::UnboundedSender<WebRtcSignalKind>>` mapping remote PeerId → the connection's inbound queue. Connect-side registers an entry before sending the offer; accept-side registers when handling an inbound offer.

  Concrete sketch:

  ```rust
  pub struct WebRtcRawTransport {
      signaler: Rc<dyn Signaler>,
      local_peer: PeerId,
      ice_urls: Vec<String>,
      // Per-remote-peer inbound queues, populated by the dispatcher task.
      inbound_per_peer: Rc<RefCell<HashMap<PeerId, mpsc::UnboundedSender<WebRtcSignalKind>>>>,
      // Inbound offers queue (consumed by accept()).
      offers_rx: RefCell<Option<mpsc::UnboundedReceiver<(PeerId, String)>>>,
      dispatcher_started: RefCell<bool>,
  }
  ```

  On first call to `connect()` or `accept()`, lazily spawn the dispatcher task with the cloned `signaler` + the per-peer table + the offers tx side.

  Connect-side updates:
  1. Before sending the offer: register `(remote_peer, our_inbound_sender)` in the table.
  2. After sending the offer: receive from our inbound queue. Expect `Answer(sdp)` first; setRemoteDescription. Then loop reading `IceCandidate(_)` messages and adding them via `pc.add_ice_candidate(...)` until the connection is established.

  Accept-side updates:
  1. Drain `offers_rx`; for each `(from_peer, offer_sdp)`:
     - Build RtcPeerConnection in responder mode
     - setRemoteDescription(offer)
     - createAnswer + setLocalDescription
     - Send `Answer(sdp)` via signaler
     - Register `(from_peer, our_inbound_sender)` in the per-peer table
     - Receive ICE candidates from our inbound queue + add them
     - Wait for datachannel open
     - Return WebRtcRawConnection

- [ ] **Step 2:** Verify wasm build + clippy:
  ```
  nix develop --command cargo fmt -p sunset-sync-webrtc-browser
  nix develop --command cargo build -p sunset-sync-webrtc-browser --target wasm32-unknown-unknown
  nix develop --command cargo clippy -p sunset-sync-webrtc-browser --all-targets --target wasm32-unknown-unknown -- -D warnings
  ```

- [ ] **Step 3:** Commit:
  ```
  git add crates/sunset-sync-webrtc-browser/src/wasm.rs
  git commit -m "Add inbound dispatcher: route Answer/IceCandidate to per-connection queues; handle inbound Offers in accept"
  ```

---

### Task 7: wasm-bindgen-test (compile + construct)

**Files:**
- Create: `crates/sunset-sync-webrtc-browser/tests/construct.rs`

- [ ] **Step 1:** Mirror the Plan E.transport pattern — assert that the constructor compiles + builds + impls RawTransport, with a stub Signaler.

  ```rust
  #![cfg(target_arch = "wasm32")]

  use std::rc::Rc;

  use async_trait::async_trait;
  use bytes::Bytes;
  use sunset_store::VerifyingKey;
  use sunset_sync::{PeerId, RawTransport, Result, SignalMessage, Signaler};
  use sunset_sync_webrtc_browser::WebRtcRawTransport;
  use wasm_bindgen_test::*;

  wasm_bindgen_test_configure!(run_in_node_experimental);

  struct StubSignaler;

  #[async_trait(?Send)]
  impl Signaler for StubSignaler {
      async fn send(&self, _: SignalMessage) -> Result<()> { Ok(()) }
      async fn recv(&self) -> Result<SignalMessage> {
          std::future::pending::<()>().await;
          unreachable!()
      }
  }

  #[wasm_bindgen_test]
  fn webrtc_transport_constructs() {
      let signaler: Rc<dyn Signaler> = Rc::new(StubSignaler);
      let local = PeerId(VerifyingKey::new(Bytes::from_static(&[1u8; 32])));
      let t = WebRtcRawTransport::new(
          signaler,
          local,
          vec!["stun:stun.l.google.com:19302".into()],
      );
      let _: &dyn TraitMarker = &t;
  }

  trait TraitMarker {}
  impl<T: RawTransport> TraitMarker for T {}
  ```

- [ ] **Step 2:** Run:
  ```
  nix develop --command bash -c 'cd crates/sunset-sync-webrtc-browser && wasm-pack test --node'
  ```
  Expect 1 passed.

- [ ] **Step 3:** Commit:
  ```
  git add crates/sunset-sync-webrtc-browser/tests/construct.rs
  git commit -m "Add wasm-bindgen-test: WebRtcRawTransport constructs"
  ```

---

### Task 8: `RelaySignaler` impl in sunset-web-wasm

**Files:**
- Create: `crates/sunset-web-wasm/src/relay_signaler.rs`
- Modify: `crates/sunset-web-wasm/src/lib.rs`
- Modify: `crates/sunset-web-wasm/Cargo.toml`

The `RelaySignaler` wraps the existing `SyncEngine` + `MemoryStore` to publish/subscribe to signaling entries. Internally it manages a Noise_KK handshake state machine per remote peer.

- [ ] **Step 1:** Add to `crates/sunset-web-wasm/Cargo.toml`'s `[dependencies]`:
  ```toml
  sunset-sync-webrtc-browser.workspace = true
  ```

- [ ] **Step 2:** Create `crates/sunset-web-wasm/src/relay_signaler.rs`. The implementation:

  - Reads ed25519 secret seed → derives X25519 secret (via `sunset_noise::ed25519_seed_to_x25519_secret`).
  - Per-remote-peer `KkInitiator` / `KkResponder` / `KkSession` state.
  - `send(SignalMessage)`:
    - Look up or create a KK state for `to`. If none exists and `seq == 0`: we're the initiator, build `KkInitiator` and call `write_message_1(payload)`. If `seq > 0`: must be a transport-mode message, use `KkSession::encrypt(payload)`.
    - Wrap the resulting bytes in a `ContentBlock` + build a `SignedKvEntry`:
      - `name = format!("{}/webrtc/{}/{}/{:016x}", room_fp_hex, hex(from), hex(to), seq).into_bytes()`
      - Insert via the engine's local store.
  - `recv() -> SignalMessage`:
    - Subscribe to `Filter::NamePrefix(format!("{}/webrtc/", room_fp_hex).into_bytes())` (already registered via `publish_room_subscription`-like mechanism). Engine's existing subscription already covers this since the room messages filter is `<fp>/msg/`; we add a parallel call that subscribes to `<fp>/webrtc/`.

      **Decision:** add a one-time setup call `publish_signaling_subscription` on the Client (similar to `publish_room_subscription`), called once on Client construction. Subscribes to `<room_fp>/webrtc/` so all signaling entries propagate to us via the relay.

    - For each entry whose `to == self.local_peer`:
      - Decrypt: if it's a known peer's transport-mode message, use the KK session. If it's the first message from a new peer, it's an inbound offer — build a `KkResponder` and call `read_message_1`.
      - Return the decrypted payload as a SignalMessage.

  Sketch (full implementation is ~300 lines):

  ```rust
  pub struct RelaySignaler {
      local_identity: sunset_core::Identity,
      local_x25519_secret: zeroize::Zeroizing<[u8; 32]>,
      room_fp_hex: String,
      store: std::sync::Arc<sunset_store_memory::MemoryStore>,
      // Per-remote-peer KK state. We stash both pre-handshake initiator/
      // responder *plus* post-handshake session so we can route subsequent
      // messages.
      sessions: std::cell::RefCell<std::collections::HashMap<PeerId, KkPeerState>>,
      inbound_rx: std::cell::RefCell<futures::channel::mpsc::UnboundedReceiver<sunset_sync::SignalMessage>>,
  }

  enum KkPeerState {
      InitiatorPending(sunset_noise::KkInitiator),
      ResponderPending(sunset_noise::KkResponder),
      Established(sunset_noise::KkSession),
  }

  impl RelaySignaler {
      pub async fn new(
          identity: sunset_core::Identity,
          room_fingerprint: [u8; 32],
          store: std::sync::Arc<sunset_store_memory::MemoryStore>,
          engine: ...,
      ) -> Self {
          // ... wire up subscription stream → inbound_tx ...
      }
  }

  #[async_trait(?Send)]
  impl Signaler for RelaySignaler {
      async fn send(&self, msg: SignalMessage) -> Result<()> { ... }
      async fn recv(&self) -> Result<SignalMessage> {
          let mut rx = self.inbound_rx.borrow_mut();
          rx.next().await.ok_or_else(|| sunset_sync::Error::Transport("signaler closed".into()))
      }
  }
  ```

- [ ] **Step 3:** Add `pub mod relay_signaler;` to `lib.rs` and re-export `RelaySignaler`.

- [ ] **Step 4:** Verify build (no test for this module yet — exercised by Task 9 + 11):
  ```
  nix develop --command cargo build -p sunset-web-wasm --target wasm32-unknown-unknown
  ```

- [ ] **Step 5:** Commit:
  ```
  git add crates/sunset-web-wasm/Cargo.toml crates/sunset-web-wasm/src/relay_signaler.rs crates/sunset-web-wasm/src/lib.rs
  git commit -m "Add RelaySignaler: Noise_KK over CRDT entries via existing SyncEngine"
  ```

---

### Task 9: Wire `connect_direct` + `peer_connection_mode` into Client

**Files:**
- Modify: `crates/sunset-web-wasm/src/client.rs`

The Client gains:
- A `MultiTransport<NoiseTransport<WebSocketRawTransport>, NoiseTransport<WebRtcRawTransport>>` engine (refactor from single-transport).
- `connect_direct(peer_pubkey: &[u8])` — invokes the WebRTC transport's connect path.
- `peer_connection_mode(peer_pubkey: &[u8]) -> String` — returns "via_relay" | "direct" | "unknown" based on which transport's MultiConnection is currently active.

- [ ] **Step 1:** Refactor the engine type:
  ```rust
  type Engine = SyncEngine<MemoryStore,
      NoiseTransport<MultiTransport<WebSocketRawTransport, WebRtcRawTransport>>>;
  ```

  Wait — the `NoiseTransport<R>` decorator wraps a single RawTransport. To wrap MultiTransport, we'd have it wrap the MultiTransport that itself contains two RawTransports. But MultiTransport is over Transport, not RawTransport.

  **Layering correction:** `MultiTransport<Transport, Transport>` (not RawTransport). So:
  ```rust
  type WsT = NoiseTransport<WebSocketRawTransport>;
  type RtcT = NoiseTransport<WebRtcRawTransport>;
  type Engine = SyncEngine<MemoryStore, MultiTransport<WsT, RtcT>>;
  ```

  Both inner transports are independently Noise-wrapped. MultiTransport multiplexes between them.

- [ ] **Step 2:** Construct in `Client::new`:
  - Build the WsT as before.
  - Build the RtcT: `NoiseTransport::new(WebRtcRawTransport::new(relay_signaler, local_peer, ice_urls), identity_adapter)`.
  - Build `MultiTransport::new(ws, rtc)`.
  - Build SyncEngine.

  RelaySignaler needs to be set up AFTER the engine exists (it uses the engine's store + local writes go through the engine). Order: construct everything in two phases — first the engine with a placeholder/stub signaler, then wire up the RelaySignaler. Or use an `Rc<RefCell<Option<RelaySignaler>>>` pattern.

  Cleanest: store wraps independently in an `Arc<MemoryStore>`; signaler uses the store directly (writes go to local store, which the engine then pushes via subscription). RelaySignaler is constructed first with the store, THEN the WebRtcRawTransport is constructed with the signaler, THEN the MultiTransport, THEN the SyncEngine. Cyclic-dep-free.

- [ ] **Step 3:** Add `Client::connect_direct(peer_pubkey: &[u8]) -> Result<(), JsError>`:
  ```rust
  pub async fn connect_direct(&self, peer_pubkey: &[u8]) -> Result<(), JsError> {
      let pk: [u8; 32] = peer_pubkey
          .try_into()
          .map_err(|_| JsError::new("peer_pubkey must be 32 bytes"))?;
      let addr_str = format!("webrtc://{}", base64_encode(&pk));
      let addr = sunset_sync::PeerAddr::new(Bytes::from(addr_str));
      self.engine
          .add_peer(addr)
          .await
          .map_err(|e| JsError::new(&format!("connect_direct: {e}")))
  }
  ```

  `base64_encode` helper using `js_sys::btoa`.

- [ ] **Step 4:** Add `Client::peer_connection_mode(peer_pubkey: &[u8]) -> String`:
  ```rust
  pub fn peer_connection_mode(&self, peer_pubkey: &[u8]) -> String {
      // The SyncEngine maintains a peer table; consult it.
      // For v1, simplest: ask the engine which transport is active for
      // the given peer. If the engine doesn't expose this, store our
      // own table that listens for peer-add/remove events.
      // ...
      // Returns "via_relay" | "direct" | "unknown".
  }
  ```

  May require a small SyncEngine API addition (`fn connection_mode_for(peer_id) -> Option<&str>`). Or maintain the state in the Client by tracking `connect_direct` calls + their results.

- [ ] **Step 5:** Verify:
  ```
  nix develop --command cargo build -p sunset-web-wasm --target wasm32-unknown-unknown
  nix develop --command cargo clippy -p sunset-web-wasm --all-targets --target wasm32-unknown-unknown -- -D warnings
  ```

- [ ] **Step 6:** Commit:
  ```
  git add crates/sunset-web-wasm/src/client.rs
  git commit -m "Add Client::connect_direct + peer_connection_mode; wire MultiTransport"
  ```

---

### Task 10: Gleam externals for `connect_direct` + connection-mode badge

**Files:**
- Modify: `web/src/sunset_web/sunset.gleam`
- Modify: `web/src/sunset_web/sunset.ffi.mjs`
- Modify: `web/src/sunset_web.gleam` (model + view)

- [ ] **Step 1:** Add Gleam externals (in sunset.gleam):
  ```gleam
  @external(javascript, "./sunset.ffi.mjs", "clientConnectDirect")
  pub fn client_connect_direct(
    client: ClientHandle,
    peer_pubkey: BitArray,
    callback: fn(Result(Nil, String)) -> Nil,
  ) -> Nil

  @external(javascript, "./sunset.ffi.mjs", "clientPeerConnectionMode")
  pub fn client_peer_connection_mode(
    client: ClientHandle,
    peer_pubkey: BitArray,
  ) -> String
  ```

- [ ] **Step 2:** Add to sunset.ffi.mjs:
  ```javascript
  export async function clientConnectDirect(client, peerPubkey, callback) {
    try {
      const bytes = bitsToBytes(peerPubkey);
      await client.connect_direct(bytes);
      callback(new Ok(undefined));
    } catch (e) {
      callback(new GError(String(e)));
    }
  }

  export function clientPeerConnectionMode(client, peerPubkey) {
    return client.peer_connection_mode(bitsToBytes(peerPubkey));
  }
  ```

- [ ] **Step 3:** In `web/src/sunset_web.gleam`, add a small per-peer connection-mode tracking field to Model + render a tiny badge in the messages list / member rail. Minimal — just a "🔗 direct" or "via relay" indicator next to the author name on messages.

  This is a small, focused UI addition. Keep it tight.

- [ ] **Step 4:** Verify build:
  ```
  cd web && nix develop ../.. --command gleam build
  ```

- [ ] **Step 5:** Commit:
  ```
  git add web/src/sunset_web/sunset.gleam web/src/sunset_web/sunset.ffi.mjs web/src/sunset_web.gleam
  git commit -m "Wire connect_direct + connection-mode indicator into Gleam app"
  ```

---

### Task 11: Playwright kill-relay e2e test

**Files:**
- Create: `web/e2e/kill_relay.spec.js`

The headline acceptance test. Mirrors the existing `two_browser_chat.spec.js` but adds a relay-kill phase.

- [ ] **Step 1:** Create the test:
  - Spawn relay subprocess
  - Open two browser contexts
  - Chat normally first (verify relay-mediated works)
  - In each browser, call `client_connect_direct(other_browser_pubkey)` via injected JS
  - Wait for `peer_connection_mode` to read "direct" on both
  - **Kill the relay subprocess**
  - Send a new message in each direction
  - Assert both arrive (proving direct WebRTC works after relay death)

  ```javascript
  // ... spec skeleton mirroring two_browser_chat.spec.js ...

  test("chat survives relay death once direct WebRTC is up", async ({ browser }) => {
      // (setup same as two_browser_chat.spec.js)

      // ... initial chat to verify relay path works ...

      // Trigger direct connection from each side.
      const aPub = await pageA.evaluate(() => window.sunsetClient.public_key);
      const bPub = await pageB.evaluate(() => window.sunsetClient.public_key);

      await pageA.evaluate(async (pk) => {
          await window.sunsetClient.connect_direct(pk);
      }, bPub);

      // Wait for both sides to report "direct".
      await pageA.waitForFunction(
          (pk) => window.sunsetClient.peer_connection_mode(pk) === "direct",
          bPub, { timeout: 15_000 }
      );
      await pageB.waitForFunction(
          (pk) => window.sunsetClient.peer_connection_mode(pk) === "direct",
          aPub, { timeout: 15_000 }
      );

      // Kill the relay.
      relayProcess.kill("SIGTERM");
      await new Promise(r => setTimeout(r, 500));

      // Send a message in each direction; verify arrival.
      const msg1 = `post-relay-death from A — ${Date.now()}`;
      await inputA.fill(msg1);
      await inputA.press("Enter");
      await expect(pageB.getByText(msg1)).toBeVisible({ timeout: 15_000 });

      const msg2 = `post-relay-death from B — ${Date.now()}`;
      await inputB.fill(msg2);
      await inputB.press("Enter");
      await expect(pageA.getByText(msg2)).toBeVisible({ timeout: 15_000 });
  });
  ```

  The `window.sunsetClient` reference: requires the Gleam app to expose the client globally for tests, OR the test injects JS that pulls from the FFI shim's cached client. Add a small test-mode hook in sunset.ffi.mjs:
  ```javascript
  // Expose client for Playwright tests (no-op in production unless
  // window.SUNSET_TEST is set).
  if (window.SUNSET_TEST) window.sunsetClient = client;
  ```

  Set `SUNSET_TEST = true` via `pageA.addInitScript("window.SUNSET_TEST = true")` before navigation.

- [ ] **Step 2:** Run:
  ```
  nix run .#web-test -- --grep "relay death"
  ```
  Expect 1 passed.

- [ ] **Step 3:** Commit:
  ```
  git add web/e2e/kill_relay.spec.js web/src/sunset_web/sunset.ffi.mjs
  git commit -m "Add Playwright kill-relay e2e: WebRTC direct keeps chat alive after relay dies"
  ```

---

### Task 12: Final pass

- [ ] **Step 1:** Workspace-wide checks:
  ```
  nix develop --command cargo fmt --all --check
  nix develop --command cargo clippy --workspace --all-features --all-targets -- -D warnings
  nix develop --command cargo test --workspace --all-features
  ```

- [ ] **Step 2:** All wasm builds:
  ```
  nix develop --command cargo build -p sunset-noise --target wasm32-unknown-unknown --lib
  nix develop --command cargo build -p sunset-core --target wasm32-unknown-unknown --lib
  nix develop --command cargo build -p sunset-sync-ws-browser --target wasm32-unknown-unknown --lib
  nix develop --command cargo build -p sunset-sync-webrtc-browser --target wasm32-unknown-unknown --lib
  nix develop --command cargo build -p sunset-web-wasm --target wasm32-unknown-unknown --lib
  ```

- [ ] **Step 3:** All Nix derivations:
  ```
  nix build .#sunset-core-wasm --no-link
  nix build .#sunset-web-wasm --no-link
  nix build .#sunset-relay --no-link
  nix build .#sunset-relay-docker --no-link
  nix build .#web --no-link
  ```

- [ ] **Step 4:** Full Playwright suite:
  ```
  nix run .#web-test
  ```
  Expect: prior tests still pass; new `kill_relay.spec.js` passes; all 5 fixture-skipped tests still skip.

- [ ] **Step 5:** If any cleanup needed:
  ```
  git add -u
  git commit -m "Final fmt + clippy pass"
  ```

---

## Verification (end-state acceptance)

After all 12 tasks land:

- All cargo checks (fmt / clippy / test) green.
- All wasm builds succeed.
- All Nix derivations build.
- Playwright suite: `two_browser_chat.spec.js` still passes (relay-mediated chat still works); new `kill_relay.spec.js` passes (chat survives relay death once WebRTC is up).
- The Gleam UI shows a per-peer "direct" / "via relay" indicator that updates after `connect_direct` succeeds.
- Two browsers in the same room can be told to connect-direct, then continue chatting after the relay process is killed.
- `git log --oneline master..HEAD` — roughly 12 task-by-task commits.

---

## What this unlocks

After V1:

- **V1.5 — auto-upgrade.** SyncEngine speculatively dials WebRTC for every learned peer in the background. UI no longer needs explicit `connect_direct`.
- **V2 — voice signaling in sunset-core.** New op types (`voice-call-create`, etc.); same Noise_KK channel for call setup metadata.
- **V3 — audio capture + playback in browser.**
- **V4 — Opus codec.**
- **V5 — voice frame AEAD over the WebRTC datachannel's** (already-Noise-wrapped) **unreliable path.** This requires extending `WebRtcRawConnection`'s unreliable channel from "v1 not implemented" to a real RTCDataChannel-with-`ordered:false,maxRetransmits:0` second channel for voice frames.
- **Native WebRTC** for relay-side voice forwarding (needed once group voice exists).
