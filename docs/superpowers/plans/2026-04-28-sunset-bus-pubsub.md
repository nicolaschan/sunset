# Bus pub/sub Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** add a `Bus` abstraction in sunset-core that unifies durable (CRDT-replicated) + ephemeral (real-time, fire-and-forget) message delivery under one filter-based pub/sub API.

**Architecture:** Add a `SignedDatagram` type + canonical signing payload in sunset-store. Add a `SyncMessage::EphemeralDelivery` variant in sunset-sync. Engine gains `publish_ephemeral` (sign-side) + `subscribe_ephemeral` (receive-side) using the existing subscription registry for routing. Per-peer task gains an unreliable recv loop and routes outbound `EphemeralDelivery` over the unreliable transport channel. sunset-core gets a `Bus` trait + `BusImpl` that wraps store + engine + identity, signs ephemeral payloads, and merges store + ephemeral streams into one `BusEvent` stream.

**Tech Stack:** Rust, postcard wire format, ed25519-dalek for signing, tokio mpsc, async-trait `?Send` (single-threaded WASM constraint), futures::stream.

**Spec:** `docs/superpowers/specs/2026-04-28-sunset-bus-pubsub-design.md`.

---

## File structure

```
crates/sunset-store/src/
├── types.rs              # MODIFY: + SignedDatagram struct
├── canonical.rs          # MODIFY: + datagram_signing_payload + frozen test vector
└── lib.rs                # MODIFY: + re-export SignedDatagram, datagram_signing_payload

crates/sunset-sync/src/
├── message.rs            # MODIFY: + SyncMessage::EphemeralDelivery + frozen test vector
├── peer.rs               # MODIFY: parallel unreliable recv loop; route ephemeral outbound
├── engine.rs             # MODIFY: ephemeral subscribers table; subscribe_ephemeral; publish_ephemeral; verify+dispatch
└── test_transport.rs     # MODIFY: real unreliable channel impl

crates/sunset-sync/tests/
└── ephemeral_two_peer.rs # NEW: two-peer integration test

crates/sunset-core/src/
├── bus.rs                # NEW: Bus trait + BusEvent + BusImpl
└── lib.rs                # MODIFY: + pub mod bus; pub use bus::{Bus, BusEvent, BusImpl}

crates/sunset-core/tests/
└── bus_integration.rs    # NEW: end-to-end Bus test using two engines + TestTransport
```

---

## Tasks

### Task 1: `SignedDatagram` type + canonical signing payload

**Files:**
- Modify: `crates/sunset-store/src/types.rs`
- Modify: `crates/sunset-store/src/canonical.rs`
- Modify: `crates/sunset-store/src/lib.rs`

- [ ] **Step 1:** Add `SignedDatagram` to `crates/sunset-store/src/types.rs`. Place immediately after `SignedKvEntry` (around line 75):

  ```rust
  /// A signed, fire-and-forget datagram. Same trust model as
  /// `SignedKvEntry` (sender-attributable via Ed25519 signature) but
  /// with no LWW, no priority, no expiry, and no content-addressed
  /// indirection. Used by the Bus's ephemeral delivery path; carried
  /// over an unreliable transport channel and never persisted.
  ///
  /// `signature` covers the canonical postcard encoding of
  /// `(verifying_key, name, payload)` — see
  /// `canonical::datagram_signing_payload`.
  #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
  pub struct SignedDatagram {
      pub verifying_key: VerifyingKey,
      pub name: bytes::Bytes,
      pub payload: bytes::Bytes,
      pub signature: bytes::Bytes,
  }
  ```

- [ ] **Step 2:** Add `datagram_signing_payload` to `crates/sunset-store/src/canonical.rs`. Place immediately after the existing `signing_payload` function:

  ```rust
  /// Canonical bytes covered by `SignedDatagram::signature`. Postcard
  /// encoding of `(verifying_key, name, payload)`. Frozen by the
  /// `datagram_payload_frozen_vector` test below.
  pub fn datagram_signing_payload(d: &SignedDatagram) -> Vec<u8> {
      #[derive(Serialize)]
      struct UnsignedDatagramRef<'a> {
          verifying_key: &'a VerifyingKey,
          name: &'a Bytes,
          payload: &'a Bytes,
      }
      let unsigned = UnsignedDatagramRef {
          verifying_key: &d.verifying_key,
          name: &d.name,
          payload: &d.payload,
      };
      postcard::to_stdvec(&unsigned)
          .expect("postcard encoding of UnsignedDatagramRef is infallible")
  }
  ```

  Update the import line at the top of `canonical.rs` from:
  ```rust
  use crate::{Hash, SignedKvEntry, VerifyingKey};
  ```
  to:
  ```rust
  use crate::{Hash, SignedDatagram, SignedKvEntry, VerifyingKey};
  ```

- [ ] **Step 3:** Add the frozen test vector at the bottom of `crates/sunset-store/src/canonical.rs`'s `mod tests` block:

  ```rust
  fn sample_datagram() -> SignedDatagram {
      SignedDatagram {
          verifying_key: VerifyingKey::new(Bytes::from_static(
              b"sample-vk-32-bytes-aaaaaaaaaaaaa",
          )),
          name: Bytes::from_static(b"room/general/voice/alice/0042"),
          payload: Bytes::from_static(b"opaque-payload-bytes"),
          signature: Bytes::from_static(b"ignored"),
      }
  }

  #[test]
  fn datagram_payload_excludes_signature_field() {
      let mut a = sample_datagram();
      let mut b = sample_datagram();
      b.signature = Bytes::from_static(b"completely different");
      assert_eq!(datagram_signing_payload(&a), datagram_signing_payload(&b));
      a.payload = Bytes::from_static(b"different payload");
      assert_ne!(datagram_signing_payload(&a), datagram_signing_payload(&b));
  }

  /// Frozen wire-format vector. If this hex changes, every existing
  /// SignedDatagram signature in the wild becomes invalid — bump the
  /// wire-format version before updating the constant.
  #[test]
  fn datagram_payload_frozen_vector() {
      let d = sample_datagram();
      let payload = datagram_signing_payload(&d);
      let digest = blake3::hash(&payload);
      // Run the test once with a placeholder hex to capture the actual
      // value; replace this constant with the real digest the first
      // time the test runs (and never change it again without bumping
      // the wire-format version).
      assert_eq!(
          digest.to_hex().as_str(),
          "PLACEHOLDER_REPLACE_AFTER_FIRST_RUN",
          "If this fails the canonical signing encoding has drifted — DO NOT update this hex without bumping the wire-format version.",
      );
  }
  ```

- [ ] **Step 4:** Run the frozen-vector test to capture the real hex. The test will fail; the failure message includes the actual digest. Replace `PLACEHOLDER_REPLACE_AFTER_FIRST_RUN` with the actual hex value the test prints:

  ```
  nix develop --command cargo test -p sunset-store --all-features canonical::tests::datagram_payload_frozen_vector
  ```

  Expect: FAIL with diff showing actual hex. Copy that hex into the constant.

  Re-run; expect PASS.

- [ ] **Step 5:** Update `crates/sunset-store/src/lib.rs` re-exports. Find the existing line:

  ```rust
  pub use canonical::signing_payload;
  ```
  and change it to:
  ```rust
  pub use canonical::{datagram_signing_payload, signing_payload};
  ```

  Find the existing line:
  ```rust
  pub use types::{ContentBlock, Cursor, Hash, SignedKvEntry, VerifyingKey};
  ```
  and change it to:
  ```rust
  pub use types::{ContentBlock, Cursor, Hash, SignedDatagram, SignedKvEntry, VerifyingKey};
  ```

- [ ] **Step 6:** Verify:

  ```
  nix develop --command cargo fmt -p sunset-store
  nix develop --command cargo test -p sunset-store --all-features
  nix develop --command cargo clippy -p sunset-store --all-features --all-targets -- -D warnings
  ```

  Expect: 2 new tests pass (`datagram_payload_excludes_signature_field`, `datagram_payload_frozen_vector`); existing tests still pass; clippy clean.

- [ ] **Step 7:** Commit:

  ```
  git add crates/sunset-store/src/types.rs crates/sunset-store/src/canonical.rs crates/sunset-store/src/lib.rs
  git commit -m "Add SignedDatagram + datagram_signing_payload (frozen wire format)"
  ```

---

### Task 2: `SyncMessage::EphemeralDelivery` variant

**Files:**
- Modify: `crates/sunset-sync/src/message.rs`

- [ ] **Step 1:** Add the variant to `SyncMessage` in `crates/sunset-sync/src/message.rs`. Insert between `Fetch` and `Goodbye` so the postcard variant tag stays at the end of the existing list:

  ```rust
  pub enum SyncMessage {
      Hello {
          protocol_version: u32,
          peer_id: PeerId,
      },
      EventDelivery {
          entries: Vec<SignedKvEntry>,
          blobs: Vec<ContentBlock>,
      },
      BlobRequest {
          hash: Hash,
      },
      BlobResponse {
          block: ContentBlock,
      },
      DigestExchange {
          filter: Filter,
          range: DigestRange,
          bloom: Bytes,
      },
      Fetch {
          entries: Vec<(VerifyingKey, Bytes)>,
      },
      EphemeralDelivery {
          datagram: sunset_store::SignedDatagram,
      },
      Goodbye {},
  }
  ```

  Update the `use sunset_store::...` line at the top to include `SignedDatagram`:

  ```rust
  use sunset_store::{ContentBlock, Filter, Hash, SignedKvEntry, VerifyingKey};
  ```

  becomes (just leave the `SignedDatagram` reference fully-qualified inline; no import change needed since the variant uses `sunset_store::SignedDatagram` qualified path).

- [ ] **Step 2:** Add a postcard round-trip test in the existing `mod tests` block:

  ```rust
  #[test]
  fn ephemeral_delivery_postcard_roundtrip() {
      use sunset_store::SignedDatagram;
      let m = SyncMessage::EphemeralDelivery {
          datagram: SignedDatagram {
              verifying_key: vk(b"alice"),
              name: Bytes::from_static(b"room/voice/alice/0042"),
              payload: Bytes::from_static(b"opus-frame-bytes"),
              signature: Bytes::from_static(&[0xab; 64]),
          },
      };
      let encoded = m.encode().unwrap();
      let decoded = SyncMessage::decode(&encoded).unwrap();
      assert_eq!(m, decoded);
  }
  ```

- [ ] **Step 3:** Add a frozen wire-format test vector for `EphemeralDelivery`. Append to the same `mod tests` block:

  ```rust
  /// Frozen wire-format vector for SyncMessage::EphemeralDelivery.
  /// If this hex changes, every existing peer breaks — bump the wire
  /// format version, don't fix the test.
  #[test]
  fn ephemeral_delivery_frozen_vector() {
      use sunset_store::SignedDatagram;
      let m = SyncMessage::EphemeralDelivery {
          datagram: SignedDatagram {
              verifying_key: vk(b"alice"),
              name: Bytes::from_static(b"room/voice/alice/0042"),
              payload: Bytes::from_static(b"opus-frame-bytes"),
              signature: Bytes::from_static(&[0xab; 64]),
          },
      };
      let encoded = m.encode().unwrap();
      let digest = blake3::hash(&encoded);
      assert_eq!(
          digest.to_hex().as_str(),
          "PLACEHOLDER_REPLACE_AFTER_FIRST_RUN",
          "If this fails the EphemeralDelivery wire format has drifted — DO NOT update this hex without bumping the wire-format version.",
      );
  }
  ```

  Add `blake3` to `crates/sunset-sync/Cargo.toml`'s `[dev-dependencies]` if not already present:

  ```toml
  blake3 = { workspace = true }
  ```

  Check first with:
  ```
  grep -n "blake3" crates/sunset-sync/Cargo.toml
  ```

- [ ] **Step 4:** Run, capture the placeholder hex, replace, run again to pass:

  ```
  nix develop --command cargo test -p sunset-sync --all-features ephemeral_delivery_frozen_vector
  ```

  Expect FAIL once → copy actual hex → PASS.

- [ ] **Step 5:** Verify:

  ```
  nix develop --command cargo fmt -p sunset-sync
  nix develop --command cargo test -p sunset-sync --all-features message::tests
  nix develop --command cargo clippy -p sunset-sync --all-features --all-targets -- -D warnings
  ```

- [ ] **Step 6:** Commit:

  ```
  git add crates/sunset-sync/src/message.rs crates/sunset-sync/Cargo.toml
  git commit -m "Add SyncMessage::EphemeralDelivery variant + frozen wire vector"
  ```

---

### Task 3: TestTransport real unreliable channel

**Files:**
- Modify: `crates/sunset-sync/src/test_transport.rs`

The current `TestConnection::send_unreliable` is a no-op and `recv_unreliable` returns pending. Wire a real bytes pipe so the integration test in Task 8 can exchange ephemeral packets.

- [ ] **Step 1:** Modify `ConnectRequest` to carry a second channel pair for the unreliable side. In `crates/sunset-sync/src/test_transport.rs`:

  ```rust
  struct ConnectRequest {
      from_peer: PeerId,
      tx_to_initiator: mpsc::UnboundedSender<Bytes>,
      rx_from_initiator: mpsc::UnboundedReceiver<Bytes>,
      // NEW: parallel unreliable channel pair.
      tx_to_initiator_unrel: mpsc::UnboundedSender<Bytes>,
      rx_from_initiator_unrel: mpsc::UnboundedReceiver<Bytes>,
      ready: oneshot::Sender<()>,
  }
  ```

- [ ] **Step 2:** Update `TestTransport::connect` to build the unreliable channel pair alongside the reliable one. In the body:

  ```rust
  // Build the channel pair (reliable).
  let (tx_initiator_to_acceptor, rx_initiator_to_acceptor) =
      mpsc::unbounded_channel::<Bytes>();
  let (tx_acceptor_to_initiator, rx_acceptor_to_initiator) =
      mpsc::unbounded_channel::<Bytes>();
  // Build the channel pair (unreliable).
  let (tx_initiator_to_acceptor_unrel, rx_initiator_to_acceptor_unrel) =
      mpsc::unbounded_channel::<Bytes>();
  let (tx_acceptor_to_initiator_unrel, rx_acceptor_to_initiator_unrel) =
      mpsc::unbounded_channel::<Bytes>();
  let (ready_tx, ready_rx) = oneshot::channel::<()>();

  inbox
      .send(ConnectRequest {
          from_peer: self.peer_id.clone(),
          tx_to_initiator: tx_acceptor_to_initiator,
          rx_from_initiator: rx_initiator_to_acceptor,
          tx_to_initiator_unrel: tx_acceptor_to_initiator_unrel,
          rx_from_initiator_unrel: rx_initiator_to_acceptor_unrel,
          ready: ready_tx,
      })
      .map_err(|_| Error::Transport("acceptor inbox closed".into()))?;

  ready_rx
      .await
      .map_err(|_| Error::Transport("acceptor dropped without accepting".into()))?;

  Ok(TestConnection::new(
      target_peer_id,
      tx_initiator_to_acceptor,
      rx_acceptor_to_initiator,
      tx_initiator_to_acceptor_unrel,
      rx_acceptor_to_initiator_unrel,
  ))
  ```

  Update `accept` similarly to forward the unreliable pair into `TestConnection::new`:

  ```rust
  Ok(TestConnection::new(
      req.from_peer,
      req.tx_to_initiator,
      req.rx_from_initiator,
      req.tx_to_initiator_unrel,
      req.rx_from_initiator_unrel,
  ))
  ```

- [ ] **Step 3:** Update `TestConnection`:

  ```rust
  #[derive(Debug)]
  pub struct TestConnection {
      peer_id: PeerId,
      tx: mpsc::UnboundedSender<Bytes>,
      rx: RefCell<mpsc::UnboundedReceiver<Bytes>>,
      tx_unrel: mpsc::UnboundedSender<Bytes>,
      rx_unrel: RefCell<mpsc::UnboundedReceiver<Bytes>>,
  }

  impl TestConnection {
      fn new(
          peer_id: PeerId,
          tx: mpsc::UnboundedSender<Bytes>,
          rx: mpsc::UnboundedReceiver<Bytes>,
          tx_unrel: mpsc::UnboundedSender<Bytes>,
          rx_unrel: mpsc::UnboundedReceiver<Bytes>,
      ) -> Self {
          Self {
              peer_id,
              tx,
              rx: RefCell::new(rx),
              tx_unrel,
              rx_unrel: RefCell::new(rx_unrel),
          }
      }
  }
  ```

  And replace the no-op `send_unreliable`/`recv_unreliable`:

  ```rust
  async fn send_unreliable(&self, bytes: Bytes) -> Result<()> {
      self.tx_unrel
          .send(bytes)
          .map_err(|_| Error::Transport("connection closed".into()))
  }

  #[allow(clippy::await_holding_refcell_ref)]
  async fn recv_unreliable(&self) -> Result<Bytes> {
      self.rx_unrel
          .borrow_mut()
          .recv()
          .await
          .ok_or_else(|| Error::Transport("connection closed".into()))
  }
  ```

- [ ] **Step 4:** Add a unit test inside the existing `mod tests` block of `test_transport.rs`:

  ```rust
  #[tokio::test(flavor = "current_thread")]
  async fn pair_can_send_and_recv_unreliable() {
      let local = tokio::task::LocalSet::new();
      local
          .run_until(async {
              let net = TestNetwork::new();
              let alice_addr = PeerAddr::new("alice");
              let bob_addr = PeerAddr::new("bob");
              let alice = net.transport(PeerId(vk(b"alice")), alice_addr.clone());
              let bob = net.transport(PeerId(vk(b"bob")), bob_addr.clone());

              let bob_accept =
                  tokio::task::spawn_local(async move { bob.accept().await.unwrap() });

              let alice_conn = alice.connect(bob_addr).await.unwrap();
              let bob_conn = bob_accept.await.unwrap();

              alice_conn
                  .send_unreliable(Bytes::from_static(b"datagram"))
                  .await
                  .unwrap();
              let got = bob_conn.recv_unreliable().await.unwrap();
              assert_eq!(got, Bytes::from_static(b"datagram"));

              bob_conn
                  .send_unreliable(Bytes::from_static(b"reply"))
                  .await
                  .unwrap();
              let got = alice_conn.recv_unreliable().await.unwrap();
              assert_eq!(got, Bytes::from_static(b"reply"));
          })
          .await;
  }
  ```

- [ ] **Step 5:** Verify:

  ```
  nix develop --command cargo test -p sunset-sync --all-features test_transport::tests
  nix develop --command cargo clippy -p sunset-sync --all-features --all-targets -- -D warnings
  ```

  Expect: new test passes; existing two tests still pass.

- [ ] **Step 6:** Commit:

  ```
  git add crates/sunset-sync/src/test_transport.rs
  git commit -m "TestTransport: real unreliable channel pair"
  ```

---

### Task 4: Per-peer task — parallel unreliable recv loop + outbound routing

**Files:**
- Modify: `crates/sunset-sync/src/peer.rs`

- [ ] **Step 1:** Replace the recv_task in `run_peer` with a fan-in over both reliable and unreliable channels. Find the existing `let recv_task = { ... };` block (around line 109) and replace it with two parallel recv tasks merging into the same inbound queue:

  ```rust
  // Concurrent recv loops — reliable and unreliable channels are
  // independent; each drains its own physical channel and routes the
  // decoded SyncMessage into the same `inbound_tx`. The engine's
  // dispatch is channel-agnostic; only the per-peer task knows which
  // wire carried the message.
  let recv_reliable_task = {
      let conn = conn.clone();
      let inbound_tx = inbound_tx.clone();
      let peer_id = peer_id.clone();
      async move {
          loop {
              match recv_reliable_message(&*conn).await {
                  Ok(SyncMessage::Goodbye {}) => {
                      let _ = inbound_tx.send(InboundEvent::Disconnected {
                          peer_id: peer_id.clone(),
                          reason: "peer goodbye".into(),
                      });
                      break;
                  }
                  Ok(message) => {
                      if inbound_tx
                          .send(InboundEvent::Message {
                              from: peer_id.clone(),
                              message,
                          })
                          .is_err()
                      {
                          break;
                      }
                  }
                  Err(e) => {
                      let _ = inbound_tx.send(InboundEvent::Disconnected {
                          peer_id: peer_id.clone(),
                          reason: format!("recv reliable: {e}"),
                      });
                      break;
                  }
              }
          }
      }
  };

  let recv_unreliable_task = {
      let conn = conn.clone();
      let inbound_tx = inbound_tx.clone();
      let peer_id = peer_id.clone();
      async move {
          loop {
              match recv_unreliable_message(&*conn).await {
                  Ok(message) => {
                      if inbound_tx
                          .send(InboundEvent::Message {
                              from: peer_id.clone(),
                              message,
                          })
                          .is_err()
                      {
                          break;
                      }
                  }
                  Err(_) => {
                      // Unreliable recv error: log + continue.
                      // Disconnection is reported by the reliable
                      // recv task only — unreliable can fail
                      // independently without tearing down the peer.
                      // In practice the underlying channel is paired
                      // with reliable, so a real disconnect will
                      // surface there too.
                      break;
                  }
              }
          }
      }
  };
  ```

  Replace the existing `tokio::join!(recv_task, send_task);` with:

  ```rust
  tokio::join!(recv_reliable_task, recv_unreliable_task, send_task);
  ```

- [ ] **Step 2:** Update the send_task to route based on message type. Replace the existing send_task (around line 146) with:

  ```rust
  let send_task = {
      let conn = conn.clone();
      async move {
          while let Some(msg) = outbound_rx.recv().await {
              let result = match outbound_kind(&msg) {
                  ChannelKind::Reliable => send_reliable_message(&*conn, &msg).await,
                  ChannelKind::Unreliable => send_unreliable_message(&*conn, &msg).await,
              };
              if result.is_err() {
                  break;
              }
          }
          let _ = send_reliable_message(&*conn, &SyncMessage::Goodbye {}).await;
          let _ = conn.close().await;
      }
  };
  ```

- [ ] **Step 3:** Replace the existing helper functions `send_message` and `recv_message` at the bottom of `peer.rs` with the four split helpers + the routing enum:

  ```rust
  /// Which physical channel a SyncMessage flows over.
  enum ChannelKind {
      Reliable,
      Unreliable,
  }

  fn outbound_kind(msg: &SyncMessage) -> ChannelKind {
      match msg {
          SyncMessage::EphemeralDelivery { .. } => ChannelKind::Unreliable,
          _ => ChannelKind::Reliable,
      }
  }

  async fn send_reliable_message<C: TransportConnection + ?Sized>(
      conn: &C,
      msg: &SyncMessage,
  ) -> Result<()> {
      let bytes = msg.encode()?;
      conn.send_reliable(bytes).await
  }

  async fn send_unreliable_message<C: TransportConnection + ?Sized>(
      conn: &C,
      msg: &SyncMessage,
  ) -> Result<()> {
      let bytes = msg.encode()?;
      conn.send_unreliable(bytes).await
  }

  async fn recv_reliable_message<C: TransportConnection + ?Sized>(
      conn: &C,
  ) -> Result<SyncMessage> {
      let bytes: Bytes = conn.recv_reliable().await?;
      SyncMessage::decode(&bytes)
  }

  async fn recv_unreliable_message<C: TransportConnection + ?Sized>(
      conn: &C,
  ) -> Result<SyncMessage> {
      let bytes: Bytes = conn.recv_unreliable().await?;
      SyncMessage::decode(&bytes)
  }
  ```

  Delete the old `send_message` and `recv_message` functions — they're replaced by the four typed helpers.

- [ ] **Step 4:** Verify the existing peer tests still pass (they only use the reliable channel, but the new structure must not regress them):

  ```
  nix develop --command cargo test -p sunset-sync --all-features peer::tests
  nix develop --command cargo clippy -p sunset-sync --all-features --all-targets -- -D warnings
  ```

  Expect existing tests pass. If any existing test in peer.rs's tests references `send_message` or `recv_message` directly, update it to call the appropriate `send_reliable_message` / `recv_reliable_message`.

- [ ] **Step 5:** Commit:

  ```
  git add crates/sunset-sync/src/peer.rs
  git commit -m "Per-peer task: parallel unreliable recv + outbound channel routing"
  ```

---

### Task 5: Engine — ephemeral subscriber dispatch table + `subscribe_ephemeral`

**Files:**
- Modify: `crates/sunset-sync/src/engine.rs`

- [ ] **Step 1:** Add the subscriber list to `EngineState`. Find the existing `EngineState` struct (around line 70-80) and add:

  ```rust
  pub(crate) struct EngineState {
      pub trust: TrustSet,
      pub registry: SubscriptionRegistry,
      pub peer_outbound: HashMap<PeerId, mpsc::UnboundedSender<SyncMessage>>,
      pub peer_kinds: HashMap<PeerId, crate::transport::TransportKind>,
      pub event_subs: Vec<mpsc::UnboundedSender<EngineEvent>>,
      /// Active in-process ephemeral subscribers. Each is a (filter,
      /// sender) pair; the engine dispatches a `SignedDatagram` to
      /// every subscriber whose filter matches the datagram's name.
      /// Dead senders (closed receivers) are evicted lazily on the
      /// next dispatch.
      pub ephemeral_subs: Vec<(Filter, mpsc::UnboundedSender<sunset_store::SignedDatagram>)>,
  }
  ```

  Initialize `ephemeral_subs: Vec::new()` in `SyncEngine::new` (find the existing initializer around line 120):

  ```rust
  state: Arc::new(Mutex::new(EngineState {
      trust: TrustSet::default(),
      registry: SubscriptionRegistry::new(),
      peer_outbound: HashMap::new(),
      peer_kinds: HashMap::new(),
      event_subs: Vec::new(),
      ephemeral_subs: Vec::new(),
  })),
  ```

- [ ] **Step 2:** Add the public `subscribe_ephemeral` API. Place it next to the existing `subscribe_engine_events` method (around line 146):

  ```rust
  /// Subscribe to ephemeral datagrams matching `filter`. Returns a
  /// fresh receiver. The engine dispatches a clone of every received
  /// `SignedDatagram` whose `(verifying_key, name)` matches the
  /// filter to this receiver. Subscription is in-process only; for
  /// remote peers to route ephemeral traffic to us, the caller must
  /// also publish the filter via `publish_subscription` (the Bus
  /// layer does this transparently in `bus.subscribe`).
  pub async fn subscribe_ephemeral(
      &self,
      filter: Filter,
  ) -> mpsc::UnboundedReceiver<sunset_store::SignedDatagram> {
      let (tx, rx) = mpsc::unbounded_channel::<sunset_store::SignedDatagram>();
      self.state.lock().await.ephemeral_subs.push((filter, tx));
      rx
  }
  ```

- [ ] **Step 3:** Add a private fan-out helper near `emit_engine_event`. Place it adjacent:

  ```rust
  /// Fan-out a datagram to every in-process subscriber whose filter
  /// matches `(datagram.verifying_key, datagram.name)`. Drops dead
  /// senders (closed receivers) lazily.
  async fn dispatch_ephemeral_local(&self, datagram: &sunset_store::SignedDatagram) {
      let mut state = self.state.lock().await;
      state.ephemeral_subs.retain(|(filter, tx)| {
          if filter.matches(&datagram.verifying_key, &datagram.name) {
              tx.send(datagram.clone()).is_ok()
          } else {
              // Keep mismatched subscribers; only drop dead ones.
              !tx.is_closed()
          }
      });
  }
  ```

- [ ] **Step 4:** Wire inbound `EphemeralDelivery` handling. Find `handle_peer_message` (search for `fn handle_peer_message`). Add a new arm to the match:

  ```rust
  SyncMessage::EphemeralDelivery { datagram } => {
      self.handle_ephemeral_delivery(from, datagram).await;
  }
  ```

  Add the helper next to it:

  ```rust
  async fn handle_ephemeral_delivery(
      &self,
      from: PeerId,
      datagram: sunset_store::SignedDatagram,
  ) {
      // Verify signature against the configured verifier (typically
      // the same Ed25519Verifier used by the store). Drop on failure.
      let payload = sunset_store::canonical::datagram_signing_payload(&datagram);
      let verifier = self.store.verifier();
      if verifier
          .verify(&datagram.verifying_key, &payload, &datagram.signature)
          .is_err()
      {
          eprintln!(
              "sunset-sync: dropping ephemeral datagram from {from:?} — bad signature"
          );
          return;
      }
      self.dispatch_ephemeral_local(&datagram).await;
  }
  ```

  This requires `Store` to expose its verifier. Check whether `Store` already has a `verifier()` method:

  ```
  grep -n "fn verifier\|pub fn verifier\|fn signature_verifier" crates/sunset-store/src/store.rs
  ```

  If not present, add one to `crates/sunset-store/src/store.rs` on the `Store` trait. Find the trait body and add (alongside the existing `insert` / `subscribe` methods):

  ```rust
  /// The signature verifier this store was constructed with.
  /// Engines reuse this for verifying messages outside the store
  /// itself (e.g. ephemeral datagrams).
  fn verifier(&self) -> std::sync::Arc<dyn SignatureVerifier>;
  ```

  Add the import at the top of `store.rs` if `Arc` / `SignatureVerifier` aren't already in scope:

  ```rust
  use std::sync::Arc;
  use crate::verifier::SignatureVerifier;
  ```

  Implement on `MemoryStore` (`crates/sunset-store-memory/src/store.rs`). Find the verifier field (run `grep -n "verifier" crates/sunset-store-memory/src/store.rs` — it's typically `verifier: Arc<dyn SignatureVerifier>` on `Inner` or top-level struct). Add to the `impl Store for MemoryStore` block:

  ```rust
  fn verifier(&self) -> std::sync::Arc<dyn SignatureVerifier> {
      // Adapt the field path to wherever the verifier lives in
      // MemoryStore — likely self.inner.lock().unwrap().verifier.clone()
      // or self.verifier.clone() depending on the impl.
      self.verifier.clone()
  }
  ```

  Implement on `FsStore` (`crates/sunset-store-fs/src/store.rs`) the same way. If you find any other `impl Store for X` in the workspace (search: `grep -rn "impl.*Store for" crates/`), add the method there too.

  Update the conformance test suite (`crates/sunset-store/src/test_helpers.rs`) only if it explicitly mocks the trait (look for `impl Store for ...` inside the test_helpers module). If absent, no change needed.

- [ ] **Step 5:** Add a unit test in the engine's `mod tests` block that verifies bad signatures are dropped. Find the existing `engine_event_fan_out_to_multiple_subscribers` test for the `make_engine` + `vk` pattern:

  ```rust
  #[tokio::test(flavor = "current_thread")]
  async fn handle_ephemeral_delivery_drops_bad_signature() {
      use sunset_store::SignedDatagram;

      let local = tokio::task::LocalSet::new();
      local
          .run_until(async {
              // Use a verifier that rejects everything to simulate
              // signature failure deterministically.
              let store = std::sync::Arc::new(
                  sunset_store_memory::MemoryStore::new(
                      std::sync::Arc::new(RejectAllVerifier),
                  ),
              );
              let engine = Rc::new(make_engine_with_store("alice", b"alice", store));

              let mut sub = engine
                  .subscribe_ephemeral(Filter::NamePrefix(Bytes::from_static(b"voice/")))
                  .await;

              // Inject a bad-signature datagram via the same path
              // handle_ephemeral_delivery would receive.
              engine
                  .handle_ephemeral_delivery(
                      PeerId(vk(b"bob")),
                      SignedDatagram {
                          verifying_key: vk(b"bob"),
                          name: Bytes::from_static(b"voice/bob/0001"),
                          payload: Bytes::from_static(b"forged"),
                          signature: Bytes::from_static(&[0u8; 64]),
                      },
                  )
                  .await;

              // Subscriber must NOT receive — verifier rejected.
              let got = tokio::time::timeout(
                  std::time::Duration::from_millis(50),
                  sub.recv(),
              )
              .await;
              assert!(got.is_err(), "bad-signature datagram must be dropped");
          })
          .await;
  }

  /// Verifier that rejects every signature. Used to test the
  /// "drop on bad signature" path deterministically.
  struct RejectAllVerifier;

  impl sunset_store::SignatureVerifier for RejectAllVerifier {
      fn verify(
          &self,
          _vk: &sunset_store::VerifyingKey,
          _payload: &[u8],
          _sig: &[u8],
      ) -> std::result::Result<(), sunset_store::Error> {
          Err(sunset_store::Error::BadSignature)
      }
  }
  ```

  This test calls `engine.handle_ephemeral_delivery` directly — it's `async fn` on `&self` and `pub(crate)`, accessible from the same crate's tests. The `make_engine_with_store` helper may not yet exist; if not, add it next to the existing `make_engine` helper in the test module:

  ```rust
  fn make_engine_with_store(
      addr: &str,
      seed: &[u8],
      store: std::sync::Arc<sunset_store_memory::MemoryStore>,
  ) -> SyncEngine<sunset_store_memory::MemoryStore, crate::test_transport::TestTransport> {
      let signer = std::sync::Arc::new(StubSigner::new(seed));
      let local_peer = PeerId(vk(seed));
      let net = crate::test_transport::TestNetwork::new();
      let transport = net.transport(local_peer.clone(), PeerAddr::new(addr));
      SyncEngine::new(
          store,
          transport,
          SyncConfig::default(),
          local_peer,
          signer,
      )
  }
  ```

  If `make_engine` doesn't already use the same shape, copy from there and parameterize on `store`.

  Verify `sunset_store::Error::BadSignature` exists:
  ```
  grep -n "BadSignature" crates/sunset-store/src/error.rs
  ```
  Use whatever error variant the existing verifier uses. If `BadSignature` doesn't exist, look at `Ed25519Verifier::verify` for the actual variant name and use that.

- [ ] **Step 6:** Verify build + new unit test passes:

  ```
  nix develop --command cargo test -p sunset-sync --all-features handle_ephemeral_delivery_drops_bad_signature
  nix develop --command cargo build -p sunset-sync --all-features
  nix develop --command cargo clippy -p sunset-sync --all-features --all-targets -- -D warnings
  ```

  Expect 1 new test pass. Task 6 adds publish_ephemeral; Task 7 adds the two-peer integration test.

- [ ] **Step 7:** Commit:

  ```
  git add crates/sunset-sync/src/engine.rs crates/sunset-store/src/store.rs crates/sunset-store-memory/src/store.rs crates/sunset-store-fs/src/store.rs
  git commit -m "Engine: ephemeral subscriber table + subscribe_ephemeral + inbound verify+dispatch"
  ```

---

### Task 6: Engine — `publish_ephemeral` (outbound fan-out)

**Files:**
- Modify: `crates/sunset-sync/src/engine.rs`

- [ ] **Step 1:** Add the public `publish_ephemeral` API. Place it next to `subscribe_ephemeral` from Task 5:

  ```rust
  /// Publish a signed ephemeral datagram. Routes via the subscription
  /// registry: every peer whose filter matches receives the datagram
  /// over the unreliable channel. Locally, in-process subscribers
  /// whose filter matches also receive a copy. Fire-and-forget — does
  /// NOT verify the signature on send (the caller is the signer); does
  /// NOT persist; does NOT retry. Returns `Ok(())` even if no peers
  /// match.
  pub async fn publish_ephemeral(
      &self,
      datagram: sunset_store::SignedDatagram,
  ) -> Result<()> {
      // Loopback: deliver to local subscribers first.
      self.dispatch_ephemeral_local(&datagram).await;

      // Fan-out to remote peers whose subscription filter matches.
      let msg = SyncMessage::EphemeralDelivery {
          datagram: datagram.clone(),
      };
      let state = self.state.lock().await;
      for peer in state
          .registry
          .peers_matching(&datagram.verifying_key, &datagram.name)
      {
          if let Some(tx) = state.peer_outbound.get(&peer) {
              let _ = tx.send(msg.clone());
          }
      }
      Ok(())
  }
  ```

- [ ] **Step 2:** Add a unit test in the engine's `mod tests` block (gate it the same way the existing tests are gated — typically `#[cfg(test)]` + `tokio::task::LocalSet`). Find an existing test like `engine_event_fan_out_to_multiple_subscribers` for the pattern:

  ```rust
  #[tokio::test(flavor = "current_thread")]
  async fn publish_ephemeral_loopback_delivers_to_local_subscriber() {
      let local = tokio::task::LocalSet::new();
      local
          .run_until(async {
              let engine = Rc::new(make_engine("alice", b"alice"));

              let mut sub = engine
                  .subscribe_ephemeral(Filter::NamePrefix(Bytes::from_static(b"voice/")))
                  .await;

              let datagram = sunset_store::SignedDatagram {
                  verifying_key: vk(b"alice"),
                  name: Bytes::from_static(b"voice/alice/0001"),
                  payload: Bytes::from_static(b"frame"),
                  signature: Bytes::from_static(&[0u8; 64]),
              };

              engine.publish_ephemeral(datagram.clone()).await.unwrap();

              let got = sub.recv().await.expect("loopback delivery");
              assert_eq!(got, datagram);
          })
          .await;
  }

  #[tokio::test(flavor = "current_thread")]
  async fn publish_ephemeral_skips_subscriber_whose_filter_does_not_match() {
      let local = tokio::task::LocalSet::new();
      local
          .run_until(async {
              let engine = Rc::new(make_engine("alice", b"alice"));

              let mut sub = engine
                  .subscribe_ephemeral(Filter::NamePrefix(Bytes::from_static(b"voice/")))
                  .await;

              let datagram = sunset_store::SignedDatagram {
                  verifying_key: vk(b"alice"),
                  name: Bytes::from_static(b"chat/alice/0001"),
                  payload: Bytes::from_static(b"frame"),
                  signature: Bytes::from_static(&[0u8; 64]),
              };

              engine.publish_ephemeral(datagram).await.unwrap();

              // Use try_recv via a 50ms sleep+poll; we expect Pending.
              let got = tokio::time::timeout(
                  std::time::Duration::from_millis(50),
                  sub.recv(),
              )
              .await;
              assert!(got.is_err(), "subscriber must NOT receive a non-matching datagram");
          })
          .await;
  }
  ```

- [ ] **Step 3:** Verify:

  ```
  nix develop --command cargo test -p sunset-sync --all-features publish_ephemeral
  nix develop --command cargo clippy -p sunset-sync --all-features --all-targets -- -D warnings
  nix develop --command cargo fmt -p sunset-sync
  ```

  Expect 2 new tests pass.

- [ ] **Step 4:** Commit:

  ```
  git add crates/sunset-sync/src/engine.rs
  git commit -m "Engine: publish_ephemeral with loopback + remote fan-out"
  ```

---

### Task 7: Two-peer integration test for ephemeral routing

**Files:**
- Create: `crates/sunset-sync/tests/ephemeral_two_peer.rs`

- [ ] **Step 1:** Create `crates/sunset-sync/tests/ephemeral_two_peer.rs`:

  ```rust
  //! End-to-end ephemeral delivery between two real engines connected
  //! via TestTransport. Verifies the wire path: subscriber publishes
  //! filter → publisher's engine routes EphemeralDelivery via
  //! unreliable channel → subscriber's engine verifies signature +
  //! dispatches to local subscribe_ephemeral receiver.

  #![cfg(feature = "test-helpers")]

  use std::rc::Rc;
  use std::sync::Arc;
  use std::time::Duration;

  use bytes::Bytes;
  use ed25519_dalek::{Signer as _, SigningKey};
  use rand::rngs::OsRng;
  use rand_core::SeedableRng;

  use sunset_store::{
      AcceptAllVerifier, Filter, SignedDatagram, VerifyingKey,
      canonical::datagram_signing_payload,
  };
  use sunset_store_memory::MemoryStore;
  use sunset_sync::test_transport::TestNetwork;
  use sunset_sync::{
      PeerAddr, PeerId, Signer, SyncConfig, SyncEngine, TrustSet,
  };

  /// Test signer: stub Ed25519 signer using a fixed seed so the test
  /// is deterministic.
  struct StubSigner {
      key: SigningKey,
  }

  impl StubSigner {
      fn new(seed: [u8; 32]) -> Self {
          Self {
              key: SigningKey::from_bytes(&seed),
          }
      }
      fn vk(&self) -> VerifyingKey {
          VerifyingKey::new(Bytes::copy_from_slice(self.key.verifying_key().as_bytes()))
      }
      fn sign_payload(&self, payload: &[u8]) -> Bytes {
          let sig = self.key.sign(payload);
          Bytes::copy_from_slice(&sig.to_bytes())
      }
  }

  impl Signer for StubSigner {
      fn verifying_key(&self) -> VerifyingKey {
          self.vk()
      }
      fn sign(&self, payload: &[u8]) -> Bytes {
          self.sign_payload(payload)
      }
  }

  fn build_engine(
      net: &TestNetwork,
      seed: [u8; 32],
      addr: &str,
  ) -> (Rc<SyncEngine<MemoryStore, sunset_sync::test_transport::TestTransport>>, Arc<StubSigner>) {
      let signer = Arc::new(StubSigner::new(seed));
      let local_peer = PeerId(signer.vk());
      let store = Arc::new(MemoryStore::new(Arc::new(AcceptAllVerifier)));
      let transport = net.transport(local_peer.clone(), PeerAddr::new(addr));
      let engine = Rc::new(SyncEngine::new(
          store,
          transport,
          SyncConfig::default(),
          local_peer,
          signer.clone() as Arc<dyn Signer>,
      ));
      (engine, signer)
  }

  #[tokio::test(flavor = "current_thread")]
  async fn ephemeral_routes_subscriber_match() {
      let local = tokio::task::LocalSet::new();
      local
          .run_until(async {
              let net = TestNetwork::new();
              let (alice, alice_signer) = build_engine(&net, [1u8; 32], "alice");
              let (bob, _bob_signer) = build_engine(&net, [2u8; 32], "bob");

              // Trust everyone in the test.
              alice.set_trust(TrustSet::All).await.unwrap();
              bob.set_trust(TrustSet::All).await.unwrap();

              // Run both engines.
              let alice_run = {
                  let alice = alice.clone();
                  tokio::task::spawn_local(async move { alice.run().await })
              };
              let bob_run = {
                  let bob = bob.clone();
                  tokio::task::spawn_local(async move { bob.run().await })
              };

              // Connect alice → bob.
              alice.add_peer(PeerAddr::new("bob")).await.unwrap();

              // Bob subscribes to voice/.
              bob.publish_subscription(
                  Filter::NamePrefix(Bytes::from_static(b"voice/")),
                  Duration::from_secs(60),
              )
              .await
              .unwrap();
              let mut bob_sub = bob
                  .subscribe_ephemeral(Filter::NamePrefix(Bytes::from_static(b"voice/")))
                  .await;

              // Wait for the subscription registry to propagate (durable
              // CRDT replication is async — give it a moment).
              tokio::time::sleep(Duration::from_millis(100)).await;

              // Alice publishes a signed ephemeral datagram on voice/.
              let name = Bytes::from_static(b"voice/alice/0001");
              let payload = Bytes::from_static(b"opus-frame-bytes");
              let unsigned = SignedDatagram {
                  verifying_key: alice_signer.vk(),
                  name: name.clone(),
                  payload: payload.clone(),
                  signature: Bytes::new(),
              };
              let sig = alice_signer.sign_payload(&datagram_signing_payload(&unsigned));
              let datagram = SignedDatagram {
                  verifying_key: alice_signer.vk(),
                  name,
                  payload,
                  signature: sig,
              };
              alice.publish_ephemeral(datagram.clone()).await.unwrap();

              // Bob's subscriber should receive within a reasonable window.
              let got = tokio::time::timeout(Duration::from_millis(500), bob_sub.recv())
                  .await
                  .expect("ephemeral arrived in time")
                  .expect("subscription open");
              assert_eq!(got, datagram);

              // Cleanup.
              alice_run.abort();
              bob_run.abort();
          })
          .await;
  }

  ```

  The signature-drop path is already covered by the unit test added in Task 5 (using a `RejectAllVerifier`); no need to duplicate it at the integration layer.

- [ ] **Step 2:** Add `rand` and `rand_core` to `crates/sunset-sync/Cargo.toml`'s `[dev-dependencies]`:

  Check first:
  ```
  grep -n "rand\b\|rand_core" crates/sunset-sync/Cargo.toml
  ```

  Add what's missing:
  ```toml
  rand = { workspace = true, optional = false }
  rand_core = { workspace = true, features = ["getrandom"] }
  ```

  Same for `ed25519-dalek`:
  ```toml
  ed25519-dalek = { workspace = true }
  ```

  Verify the workspace Cargo.toml has these (`grep -n "ed25519-dalek\|rand\b" Cargo.toml`); add them under `[workspace.dependencies]` if not.

- [ ] **Step 3:** Run the test:

  ```
  nix develop --command cargo test -p sunset-sync --all-features --test ephemeral_two_peer
  ```

  Expect 1 test pass (after deleting the second documentation-only test).

- [ ] **Step 4:** Commit:

  ```
  git add crates/sunset-sync/tests/ephemeral_two_peer.rs crates/sunset-sync/Cargo.toml
  git commit -m "Two-peer integration test: ephemeral routes from publisher to subscriber"
  ```

---

### Task 8: sunset-core `Bus` trait + `BusEvent`

**Files:**
- Create: `crates/sunset-core/src/bus.rs`
- Modify: `crates/sunset-core/src/lib.rs`

- [ ] **Step 1:** Create `crates/sunset-core/src/bus.rs` with the trait + types only:

  ```rust
  //! Pub/sub abstraction over both durable (CRDT-replicated) and
  //! ephemeral (real-time, fire-and-forget) message delivery. Same
  //! filter system, same signing model; different persistence + transport.
  //!
  //! See `docs/superpowers/specs/2026-04-28-sunset-bus-pubsub-design.md`
  //! for the architecture.

  use async_trait::async_trait;
  use bytes::Bytes;
  use futures::stream::LocalBoxStream;

  use sunset_store::{ContentBlock, Filter, SignedDatagram, SignedKvEntry};

  use crate::error::Result;

  /// A message delivered to a Bus subscriber. Tagged by delivery mode
  /// so consumers can act differently (e.g. voice consumes Ephemeral,
  /// chat consumes Durable).
  #[derive(Clone, Debug)]
  pub enum BusEvent {
      Durable {
          entry: SignedKvEntry,
          block: Option<ContentBlock>,
      },
      Ephemeral(SignedDatagram),
  }

  /// Unified pub/sub interface. `publish_durable` writes a signed KV
  /// entry to the local store and lets the engine fan out via CRDT
  /// replication. `publish_ephemeral` signs the payload, hands it to
  /// the engine for unreliable fan-out, and dispatches a loopback copy
  /// to local subscribers. `subscribe` opens a single stream that
  /// merges both delivery modes.
  #[async_trait(?Send)]
  pub trait Bus {
      async fn publish_durable(
          &self,
          entry: SignedKvEntry,
          block: Option<ContentBlock>,
      ) -> Result<()>;

      async fn publish_ephemeral(
          &self,
          name: Bytes,
          payload: Bytes,
      ) -> Result<()>;

      async fn subscribe(
          &self,
          filter: Filter,
      ) -> Result<LocalBoxStream<'static, BusEvent>>;
  }
  ```

- [ ] **Step 2:** Wire the new module into `crates/sunset-core/src/lib.rs`. Add to the `pub mod` list (alphabetical):

  ```rust
  pub mod bus;
  ```

  And add to the re-export block (alphabetical):

  ```rust
  pub use bus::{Bus, BusEvent};
  ```

  Add `futures` to `crates/sunset-core/Cargo.toml`'s `[dependencies]` if not already present:

  ```
  grep -n "^futures\b" crates/sunset-core/Cargo.toml
  ```

  If missing:
  ```toml
  futures = { workspace = true }
  ```

  And `async-trait`:
  ```
  grep -n "async-trait" crates/sunset-core/Cargo.toml
  ```

  If missing:
  ```toml
  async-trait = { workspace = true }
  ```

- [ ] **Step 3:** Verify build:

  ```
  nix develop --command cargo build -p sunset-core --all-features
  nix develop --command cargo clippy -p sunset-core --all-features --all-targets -- -D warnings
  ```

  Expect compile-clean. No new tests yet — Task 9 adds the impl + tests.

- [ ] **Step 4:** Commit:

  ```
  git add crates/sunset-core/src/bus.rs crates/sunset-core/src/lib.rs crates/sunset-core/Cargo.toml
  git commit -m "Add Bus trait + BusEvent"
  ```

---

### Task 9: `BusImpl` — publish_durable + publish_ephemeral

**Files:**
- Modify: `crates/sunset-core/src/bus.rs`

- [ ] **Step 1:** Append the impl to `crates/sunset-core/src/bus.rs` (after the `Bus` trait definition):

  ```rust
  use std::rc::Rc;
  use std::sync::Arc;

  use sunset_store::{Replay, Store, canonical::datagram_signing_payload};
  use sunset_sync::{SyncEngine, Transport};

  use crate::identity::Identity;

  /// Concrete `Bus` impl wrapping the engine + store + identity.
  /// Generic over the same `Store` and `Transport` types the engine
  /// uses. Cheap to clone (Rc + Arc internally).
  #[derive(Clone)]
  pub struct BusImpl<S: Store + 'static, T: Transport + 'static>
  where
      T::Connection: 'static,
  {
      store: Arc<S>,
      engine: Rc<SyncEngine<S, T>>,
      identity: Identity,
  }

  impl<S: Store + 'static, T: Transport + 'static> BusImpl<S, T>
  where
      T::Connection: 'static,
  {
      pub fn new(store: Arc<S>, engine: Rc<SyncEngine<S, T>>, identity: Identity) -> Self {
          Self { store, engine, identity }
      }
  }

  #[async_trait(?Send)]
  impl<S: Store + 'static, T: Transport + 'static> Bus for BusImpl<S, T>
  where
      T::Connection: 'static,
  {
      async fn publish_durable(
          &self,
          entry: SignedKvEntry,
          block: Option<ContentBlock>,
      ) -> Result<()> {
          self.store
              .insert(entry, block)
              .await
              .map_err(|e| crate::Error::Store(format!("{e}")))
      }

      async fn publish_ephemeral(
          &self,
          name: Bytes,
          payload: Bytes,
      ) -> Result<()> {
          // Build the unsigned shape, sign the canonical bytes, and
          // assemble the final SignedDatagram.
          let unsigned = SignedDatagram {
              verifying_key: self.identity.store_verifying_key(),
              name: name.clone(),
              payload: payload.clone(),
              signature: Bytes::new(),
          };
          let payload_bytes = datagram_signing_payload(&unsigned);
          let signature = Bytes::copy_from_slice(&self.identity.sign(&payload_bytes).to_bytes());
          let datagram = SignedDatagram {
              verifying_key: unsigned.verifying_key,
              name: unsigned.name,
              payload: unsigned.payload,
              signature,
          };
          self.engine
              .publish_ephemeral(datagram)
              .await
              .map_err(|e| crate::Error::Sync(format!("{e}")))
      }

      async fn subscribe(
          &self,
          filter: Filter,
      ) -> Result<LocalBoxStream<'static, BusEvent>> {
          // Implementation in Task 10.
          let _ = filter;
          unimplemented!("subscribe lands in Task 10")
      }
  }
  ```

- [ ] **Step 2:** The `crate::Error` enum needs `Store` and `Sync` variants. Open `crates/sunset-core/src/error.rs` and ensure they exist. Search:

  ```
  grep -n "Store\b\|Sync\b" crates/sunset-core/src/error.rs
  ```

  If absent, add:

  ```rust
  // In crates/sunset-core/src/error.rs, in the Error enum:
  #[error("store: {0}")]
  Store(String),
  #[error("sync: {0}")]
  Sync(String),
  ```

  Adjust import if needed.

- [ ] **Step 3:** Add the unit test for `publish_ephemeral` loopback. In `crates/sunset-core/src/bus.rs`, add at the bottom:

  ```rust
  #[cfg(test)]
  mod tests {
      use super::*;
      use std::sync::Arc;
      use std::time::Duration;

      use futures::StreamExt as _;
      use sunset_store::{AcceptAllVerifier, Filter};
      use sunset_store_memory::MemoryStore;
      use sunset_sync::test_transport::{TestNetwork};
      use sunset_sync::{PeerAddr, PeerId, Signer, SyncConfig, SyncEngine};

      use crate::identity::Identity;
      use rand_core::OsRng;

      fn make_bus() -> (
          BusImpl<MemoryStore, sunset_sync::test_transport::TestTransport>,
          Identity,
      ) {
          let net = TestNetwork::new();
          let identity = Identity::generate(&mut OsRng);
          let local_peer = PeerId(identity.store_verifying_key());
          let store = Arc::new(MemoryStore::new(Arc::new(AcceptAllVerifier)));
          let transport = net.transport(local_peer.clone(), PeerAddr::new("self"));
          let engine = std::rc::Rc::new(SyncEngine::new(
              store.clone(),
              transport,
              SyncConfig::default(),
              local_peer,
              Arc::new(identity.clone()) as Arc<dyn Signer>,
          ));
          let bus = BusImpl::new(store, engine, identity.clone());
          (bus, identity)
      }

      #[tokio::test(flavor = "current_thread")]
      async fn publish_ephemeral_loopback_via_engine() {
          let local = tokio::task::LocalSet::new();
          local
              .run_until(async {
                  let (bus, _identity) = make_bus();
                  let mut sub = bus
                      .engine
                      .subscribe_ephemeral(Filter::NamePrefix(Bytes::from_static(
                          b"voice/",
                      )))
                      .await;
                  bus.publish_ephemeral(
                      Bytes::from_static(b"voice/me/0001"),
                      Bytes::from_static(b"frame"),
                  )
                  .await
                  .unwrap();
                  let got = tokio::time::timeout(Duration::from_millis(50), sub.recv())
                      .await
                      .expect("loopback fired in time")
                      .expect("subscription open");
                  assert_eq!(&got.name, &Bytes::from_static(b"voice/me/0001"));
                  assert_eq!(&got.payload, &Bytes::from_static(b"frame"));
              })
              .await;
      }
  }
  ```

- [ ] **Step 4:** Verify:

  ```
  nix develop --command cargo build -p sunset-core --all-features
  nix develop --command cargo test -p sunset-core --all-features bus::tests
  nix develop --command cargo clippy -p sunset-core --all-features --all-targets -- -D warnings
  ```

  Expect: 1 new test pass; subscribe panics (`unimplemented!`) which is intentional — Task 10 wires it.

- [ ] **Step 5:** Commit:

  ```
  git add crates/sunset-core/src/bus.rs crates/sunset-core/src/error.rs
  git commit -m "BusImpl: publish_durable + publish_ephemeral"
  ```

---

### Task 10: `BusImpl::subscribe` — merged stream

**Files:**
- Modify: `crates/sunset-core/src/bus.rs`

- [ ] **Step 1:** Replace the `unimplemented!` body of `subscribe` in `BusImpl`:

  ```rust
  async fn subscribe(
      &self,
      filter: Filter,
  ) -> Result<LocalBoxStream<'static, BusEvent>> {
      use futures::stream::StreamExt as _;

      // Publish our subscription so peers learn what we want. TTL is
      // 1 hour; consumers that need a different lifetime can call
      // engine.publish_subscription directly.
      self.engine
          .publish_subscription(filter.clone(), std::time::Duration::from_secs(3600))
          .await
          .map_err(|e| crate::Error::Sync(format!("{e}")))?;

      // Durable side: existing store subscription. Replay::All so
      // late-joining subscribers see history.
      let durable_stream = self
          .store
          .subscribe(filter.clone(), Replay::All)
          .await
          .map_err(|e| crate::Error::Store(format!("{e}")))?;

      // Ephemeral side: in-process dispatch from the engine.
      let ephemeral_rx = self.engine.subscribe_ephemeral(filter).await;

      // Map each side into BusEvent and merge. Use UnboundedReceiverStream
      // for the ephemeral side; the durable side is already a stream.
      let store_for_block_fetch = self.store.clone();
      let durable_mapped = durable_stream.filter_map(move |ev| {
          let store = store_for_block_fetch.clone();
          async move {
              let entry = match ev {
                  Ok(sunset_store::Event::Inserted(e)) => e,
                  Ok(sunset_store::Event::Replaced { new, .. }) => new,
                  // Expired / BlobAdded / BlobRemoved are not
                  // application-relevant for the bus.
                  Ok(_) => return None,
                  Err(_) => return None,
              };
              // Lazily fetch the block. None if not yet local
              // (dangling-ref allowed per store contract).
              let block = store.get_content(&entry.value_hash).await.ok().flatten();
              Some(BusEvent::Durable { entry, block })
          }
      });

      let ephemeral_mapped =
          tokio_stream::wrappers::UnboundedReceiverStream::new(ephemeral_rx)
              .map(BusEvent::Ephemeral);

      let merged = futures::stream::select(durable_mapped, ephemeral_mapped);
      Ok(Box::pin(merged))
  }
  ```

  Add `tokio-stream` to `crates/sunset-core/Cargo.toml`'s `[dependencies]`:

  ```
  grep -n "tokio-stream" crates/sunset-core/Cargo.toml
  ```

  If missing:
  ```toml
  tokio-stream = { workspace = true }
  ```

  And ensure the workspace Cargo.toml has:
  ```
  grep -n "tokio-stream" Cargo.toml
  ```

  If missing, add to `[workspace.dependencies]`:
  ```toml
  tokio-stream = "0.1"
  ```

- [ ] **Step 2:** Add an integration test that verifies the merged stream yields both kinds. In the existing `mod tests` block in `bus.rs`:

  ```rust
  #[tokio::test(flavor = "current_thread")]
  async fn subscribe_merges_durable_and_ephemeral() {
      use bytes::Bytes;
      use sunset_store::{ContentBlock, SignedKvEntry, canonical::signing_payload};

      let local = tokio::task::LocalSet::new();
      local
          .run_until(async {
              let (bus, identity) = make_bus();
              let mut stream = bus
                  .subscribe(Filter::NamePrefix(Bytes::from_static(b"chat/")))
                  .await
                  .unwrap();

              // Publish a durable entry under chat/ — should arrive as
              // Durable on the merged stream.
              let block = ContentBlock {
                  data: Bytes::from_static(b"hello"),
                  references: vec![],
              };
              let value_hash = block.hash();
              let mut entry = SignedKvEntry {
                  verifying_key: identity.store_verifying_key(),
                  name: Bytes::from_static(b"chat/me/abc"),
                  value_hash,
                  priority: 1,
                  expires_at: None,
                  signature: Bytes::new(),
              };
              let sig = identity.sign(&signing_payload(&entry));
              entry.signature = Bytes::copy_from_slice(&sig.to_bytes());

              bus.publish_durable(entry, Some(block.clone())).await.unwrap();

              // Publish an ephemeral on chat/ — should arrive as Ephemeral.
              bus.publish_ephemeral(
                  Bytes::from_static(b"chat/me/eph"),
                  Bytes::from_static(b"now"),
              )
              .await
              .unwrap();

              // Read first two events from the merged stream. Order
              // is unspecified; assert the SET of (kind, name) pairs.
              use futures::StreamExt as _;
              let mut got = Vec::new();
              for _ in 0..2 {
                  let ev = tokio::time::timeout(
                      std::time::Duration::from_millis(200),
                      stream.next(),
                  )
                  .await
                  .expect("event arrived")
                  .expect("stream open");
                  got.push(match ev {
                      BusEvent::Durable { entry, .. } => ("durable", entry.name.to_vec()),
                      BusEvent::Ephemeral(d) => ("ephemeral", d.name.to_vec()),
                  });
              }
              got.sort();
              assert_eq!(
                  got,
                  vec![
                      ("durable", b"chat/me/abc".to_vec()),
                      ("ephemeral", b"chat/me/eph".to_vec()),
                  ],
              );
          })
          .await;
  }
  ```

- [ ] **Step 3:** Verify:

  ```
  nix develop --command cargo build -p sunset-core --all-features
  nix develop --command cargo test -p sunset-core --all-features bus::tests
  nix develop --command cargo clippy -p sunset-core --all-features --all-targets -- -D warnings
  nix develop --command cargo fmt -p sunset-core
  ```

  Expect: 2 tests in bus::tests pass.

- [ ] **Step 4:** Commit:

  ```
  git add crates/sunset-core/src/bus.rs crates/sunset-core/Cargo.toml Cargo.toml
  git commit -m "BusImpl::subscribe: merged stream of durable + ephemeral"
  ```

---

### Task 11: Two-engine integration test for Bus end-to-end

**Files:**
- Create: `crates/sunset-core/tests/bus_integration.rs`

- [ ] **Step 1:** Create `crates/sunset-core/tests/bus_integration.rs`:

  ```rust
  //! End-to-end Bus test: two engines connected via TestTransport,
  //! one publishes ephemeral, the other receives via subscribe.

  #![cfg(feature = "test-helpers")]

  use std::rc::Rc;
  use std::sync::Arc;
  use std::time::Duration;

  use bytes::Bytes;
  use futures::StreamExt as _;
  use rand_core::OsRng;

  use sunset_core::{Bus, BusEvent, BusImpl, Identity};
  use sunset_store::{AcceptAllVerifier, Filter};
  use sunset_store_memory::MemoryStore;
  use sunset_sync::test_transport::TestNetwork;
  use sunset_sync::{
      PeerAddr, PeerId, Signer, SyncConfig, SyncEngine, TrustSet,
  };

  type TestEngine =
      SyncEngine<MemoryStore, sunset_sync::test_transport::TestTransport>;

  fn build(net: &TestNetwork, addr: &str)
      -> (BusImpl<MemoryStore, sunset_sync::test_transport::TestTransport>, Rc<TestEngine>, Identity)
  {
      let identity = Identity::generate(&mut OsRng);
      let local_peer = PeerId(identity.store_verifying_key());
      let store = Arc::new(MemoryStore::new(Arc::new(AcceptAllVerifier)));
      let transport = net.transport(local_peer.clone(), PeerAddr::new(addr));
      let engine = Rc::new(SyncEngine::new(
          store.clone(),
          transport,
          SyncConfig::default(),
          local_peer,
          Arc::new(identity.clone()) as Arc<dyn Signer>,
      ));
      let bus = BusImpl::new(store, engine.clone(), identity.clone());
      (bus, engine, identity)
  }

  #[tokio::test(flavor = "current_thread")]
  async fn ephemeral_publish_arrives_at_remote_subscriber() {
      let local = tokio::task::LocalSet::new();
      local
          .run_until(async {
              let net = TestNetwork::new();
              let (alice_bus, alice_engine, _) = build(&net, "alice");
              let (bob_bus, bob_engine, _) = build(&net, "bob");

              alice_engine.set_trust(TrustSet::All).await.unwrap();
              bob_engine.set_trust(TrustSet::All).await.unwrap();

              let alice_run = {
                  let e = alice_engine.clone();
                  tokio::task::spawn_local(async move { e.run().await })
              };
              let bob_run = {
                  let e = bob_engine.clone();
                  tokio::task::spawn_local(async move { e.run().await })
              };

              alice_engine.add_peer(PeerAddr::new("bob")).await.unwrap();

              // Bob subscribes to voice/ via the bus surface.
              let mut bob_stream = bob_bus
                  .subscribe(Filter::NamePrefix(Bytes::from_static(b"voice/")))
                  .await
                  .unwrap();

              // Wait for subscription to propagate.
              tokio::time::sleep(Duration::from_millis(150)).await;

              // Alice publishes ephemeral.
              alice_bus
                  .publish_ephemeral(
                      Bytes::from_static(b"voice/alice/0001"),
                      Bytes::from_static(b"opus-frame"),
                  )
                  .await
                  .unwrap();

              // Bob's stream should yield an Ephemeral within a window.
              let ev = tokio::time::timeout(Duration::from_millis(500), bob_stream.next())
                  .await
                  .expect("event arrived")
                  .expect("stream open");
              match ev {
                  BusEvent::Ephemeral(d) => {
                      assert_eq!(&d.name, &Bytes::from_static(b"voice/alice/0001"));
                      assert_eq!(&d.payload, &Bytes::from_static(b"opus-frame"));
                  }
                  other => panic!("expected Ephemeral, got {other:?}"),
              }

              alice_run.abort();
              bob_run.abort();
          })
          .await;
  }
  ```

- [ ] **Step 2:** Verify the test wires up correctly. May need a `[features]` entry in `crates/sunset-core/Cargo.toml`:

  ```
  grep -n "test-helpers" crates/sunset-core/Cargo.toml
  ```

  If missing, add:
  ```toml
  [features]
  test-helpers = ["sunset-store/test-helpers", "sunset-sync/test-helpers"]
  ```

  And `sunset-sync = { workspace = true, features = ["test-helpers"] }` in dev-dependencies if not already.

- [ ] **Step 3:** Run:

  ```
  nix develop --command cargo test -p sunset-core --all-features --test bus_integration
  ```

  Expect 1 test pass.

- [ ] **Step 4:** Commit:

  ```
  git add crates/sunset-core/tests/bus_integration.rs crates/sunset-core/Cargo.toml
  git commit -m "Two-engine integration test: ephemeral via Bus.subscribe"
  ```

---

### Task 12: Final pass

- [ ] **Step 1:** Workspace-wide checks:

  ```
  nix develop --command cargo fmt --all --check
  nix develop --command cargo clippy --workspace --all-features --all-targets -- -D warnings
  nix develop --command cargo test --workspace --all-features
  ```

  All green.

- [ ] **Step 2:** Wasm builds (Bus is wasm-compatible since `?Send` and uses no native-only types):

  ```
  nix develop --command cargo build -p sunset-store --target wasm32-unknown-unknown --lib
  nix develop --command cargo build -p sunset-sync --target wasm32-unknown-unknown --lib
  nix develop --command cargo build -p sunset-core --target wasm32-unknown-unknown --lib
  nix develop --command cargo build -p sunset-web-wasm --target wasm32-unknown-unknown --lib
  ```

  All clean.

- [ ] **Step 3:** All Nix derivations build (sanity check that the new wire format hasn't broken downstream packaging):

  ```
  nix build .#sunset-core-wasm .#sunset-web-wasm .#sunset-relay .#sunset-relay-docker .#web --no-link
  ```

  All build.

- [ ] **Step 4:** Full Playwright suite (regression check that existing two_browser_chat / kill_relay / presence tests still pass — Bus is additive, none of these tests should be affected):

  ```
  nix run .#web-test
  ```

  All prior tests pass; no new failures.

- [ ] **Step 5:** If any cleanup needed:

  ```
  git add -u
  git commit -m "Final fmt + clippy pass"
  ```

---

## Verification (end-state acceptance)

After all 12 tasks land:

- All cargo checks (fmt / clippy / test) green workspace-wide.
- All wasm builds succeed.
- All Nix derivations build.
- Playwright regression: prior tests still pass; no new e2e tests added by this plan.
- New `cargo test -p sunset-core --all-features --test bus_integration` proves end-to-end Bus delivery between two engines.
- New `cargo test -p sunset-sync --all-features --test ephemeral_two_peer` proves engine-level ephemeral routing.
- Frozen wire-format vectors pinned for `SignedDatagram` (in sunset-store/canonical.rs) and `SyncMessage::EphemeralDelivery` (in sunset-sync/message.rs).
- `git log --oneline master..HEAD` — roughly 12 task-by-task commits.

---

## What this unlocks

- **Plan A (unreliable channel impl on `WebRtcRawConnection`)** — independent and can run in parallel with this plan. Once both land, ephemeral delivery actually flows over real WebRTC datachannels in the browser.
- **Plan C (voice end-to-end)** — depends on this Bus + Plan A. Voice publishes Opus frames on `<room_fp>/voice/<peer>/<seq>` via `bus.publish_ephemeral`; consumers subscribe to `<room_fp>/voice/` and decode.
- **Plan V_forwarding (peer-to-peer relay forwarding)** — composes naturally: a forwarding peer is just a Bus subscriber that re-publishes after verifying the signature. The signing model means forwarders are untrusted carriers, not sources.
