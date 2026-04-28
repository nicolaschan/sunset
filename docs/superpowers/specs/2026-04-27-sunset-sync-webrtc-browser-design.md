# sunset-sync browser WebRTC transport (V1) — Subsystem design

- **Date:** 2026-04-27
- **Status:** Draft (subsystem-level)
- **Scope:** Browser-side WebRTC `RawTransport` implementation. Lets two browsers establish a direct peer-to-peer datachannel through relay-mediated signaling, then chat directly over that datachannel without further relay involvement. First step toward voice (which will reuse this transport's unreliable channel) and a real chat win on its own (lower latency, relay can go down without disconnecting active conversations).

## Non-negotiable goals

1. **Two browsers establish direct WebRTC and chat over it.** Once the peer connection is up, sync messages route over the datachannel, bypassing the relay entirely for direct-peer traffic.
2. **Signaling rides the existing stack.** SDP offers/answers and ICE candidates exchange as signed + encrypted CRDT entries through the existing relay-mediated SyncEngine. No new infrastructure. Every existing crypto + replay + offline guarantee applies for free.
3. **Relay can die mid-conversation without breaking ongoing chat.** Headline acceptance: the Playwright test spawns a relay, opens two browsers, waits for direct WebRTC to establish, then **kills the relay subprocess**, then proves a new message sent in each browser still arrives at the other — purely through WebRTC datachannels.
4. **WebRTC is opt-in for v1.** Gleam app calls `client.connect_direct(pubkey)` explicitly. UI surfaces "direct" / "via relay" connection state per peer. Auto-upgrade to direct (the better UX) is V1.5.

## Non-goals (deferred)

- **Native WebRTC transport** (`webrtc-rs`). The relay continues using WebSocket. Native↔native peers (relay↔relay federation) continue using `sunset-sync-ws-native`. WebRTC native is a voice prerequisite (relays forwarding voice frames in mesh-fallback) and lives in V1.5 or the voice subsystem.
- **TURN servers.** Hardcoded STUN config (`stun:stun.l.google.com:19302`) only. NAT traversal works for typical home routers; symmetric NATs / corporate firewalls fail and fall back to relay-only chat. Real TURN config is operational concern.
- **Auto-upgrade.** SyncEngine doesn't speculatively dial WebRTC for every learned peer. Explicit `connect_direct` only.
- **Voice frames** ride this transport's *unreliable* channel later — but the unreliable channel is wired in v1 only as a stub (returns Unsupported). Voice work fills it in.
- **Reconnection** after datachannel drop: if WebRTC disconnects, peer goes back to "via relay" until the user explicitly re-dials. Auto-reconnect is V1.5.
- **Renegotiation** (changing media tracks mid-call). Not needed for chat; voice will need it later.

## Threat model additions

The Plan C threat model already covered Noise-over-Bytes-channel guarantees. WebRTC adds:

- **Datachannel uses DTLS-SRTP** at the WebRTC layer (browser-mandated). The Noise tunnel still wraps the bytes inside, so even if DTLS were compromised, payloads remain Noise-protected.
- **STUN/TURN servers see connection metadata** (peer IPs, timing). This is the standard WebRTC tradeoff. Operators concerned about IP exposure can run their own STUN/TURN.
- **Browser's WebRTC implementation is large attack surface.** We defer to the browser's hardening; sunset.chat doesn't try to harden WebRTC further.
- **The ICE candidate exchange leaks peer IPs to room members** (via the encrypted signaling entries — only room members can decrypt). This is acceptable for the chat threat model: room members already know each other's pubkeys and can correlate.

## Architecture

### Crate layout

```
crates/sunset-sync-webrtc-browser/    # NEW
├── Cargo.toml
└── src/
    ├── lib.rs              # cfg-gated re-exports (wasm impl OR native stub)
    ├── stub.rs             # native fallback for cargo build --workspace on Linux dev hosts
    ├── wasm/
    │   ├── mod.rs
    │   ├── transport.rs    # WebRtcRawTransport
    │   ├── connection.rs   # WebRtcRawConnection (datachannel wrapper)
    │   ├── handshake.rs    # SDP/ICE handshake state machine
    │   └── signal.rs       # SignalMessage + Signaler trait usage
```

### Signaler trait — lives in sunset-sync

Where: `crates/sunset-sync/src/signaler.rs` (NEW). The trait belongs in sunset-sync because it's a generic "send/recv signaling messages" abstraction; multiple transports could use it (WebRTC today, future ones too). Keeping it in the sunset-sync layer means the WebRTC crate doesn't need to know about stores or engines.

```rust
use async_trait::async_trait;
use bytes::Bytes;

use crate::types::PeerId;

/// Opaque per-peer signaling message. The transport that uses a Signaler
/// defines its own wire format inside `payload` (e.g., postcard-encoded
/// SDP/ICE for WebRTC); the Signaler doesn't interpret it.
pub struct SignalMessage {
    pub from: PeerId,
    pub to:   PeerId,
    pub seq:  u64,
    pub payload: Bytes,
}

/// Side-channel for transports that need an out-of-band exchange before
/// data flow can begin (WebRTC SDP/ICE, future Noise-over-multiple-relays
/// connection state, etc.).
#[async_trait(?Send)]
pub trait Signaler: 'static {
    /// Send a signaling message to a remote peer.
    async fn send(&self, message: SignalMessage) -> Result<(), crate::Error>;

    /// Wait for the next inbound signaling message addressed to us.
    async fn recv(&self) -> Result<SignalMessage, crate::Error>;
}
```

### `RelaySignaler` impl — lives in sunset-web-wasm

Where: `crates/sunset-web-wasm/src/relay_signaler.rs` (NEW). Wraps an existing `Arc<MemoryStore>` + `Rc<SyncEngine>` to produce signaling entries. The Signaler trait is in sunset-sync; the impl is in the consumer crate (sunset-web-wasm) because it needs concrete types from elsewhere in the stack.

**Wire format for signaling entries:**

- `name`: `<room_fingerprint_hex>/webrtc/<from_pubkey_hex>/<to_pubkey_hex>/<seq_hex>`
- `verifying_key`: sender's Ed25519 pubkey (already enforced by Plan 6's outer signature)
- `value_hash`: hash of a `ContentBlock` containing postcard-encoded `WebRtcSignalPayload`
- `priority`: monotonic per-(from,to) counter (so receiver can dedupe + order)
- `expires_at`: now + 60 seconds (signaling is short-lived; expired entries get pruned)
- `signature`: Ed25519 by sender (Ed25519Verifier accepts at insert)

Inside the `ContentBlock.data` (encrypted? — see below):

```rust
#[derive(Serialize, Deserialize)]
pub enum WebRtcSignalPayload {
    Offer(String),       // SDP offer
    Answer(String),      // SDP answer
    IceCandidate(String), // serialized ICE candidate
}
```

**Encryption layer:** SDP/ICE payloads contain peer IP addresses (sensitive metadata). They should be AEAD-encrypted under K_room so eavesdroppers without the room name can't see them. v1: encrypt with the same per-message AEAD scheme from sunset-core (HKDF from K_room → message key, XChaCha20-Poly1305). The signaling content block uses the same `EncryptedMessage` shape as chat messages, just with a different inner enum.

Actually — to keep v1 small, **defer the AEAD encryption to V1.5**. v1 ships SDP/ICE as plaintext inside the ContentBlock. The outer signature still authenticates; only confidentiality of SDP is missing. Document the gap and close it before voice ships (because voice signaling will absolutely need encrypted SDP).

### `WebRtcRawTransport` — over web-sys::RtcPeerConnection

```rust
pub struct WebRtcRawTransport {
    signaler: Rc<dyn Signaler>,
    local_peer: PeerId,
    ice_servers: Vec<RtcIceServer>,
    // Map of pending dials, keyed by remote PeerId
    pending: RefCell<HashMap<PeerId, oneshot::Sender<RawConnection>>>,
}

impl WebRtcRawTransport {
    pub fn new(signaler: Rc<dyn Signaler>, local_peer: PeerId) -> Self;
}

#[async_trait(?Send)]
impl RawTransport for WebRtcRawTransport {
    type Connection = WebRtcRawConnection;

    /// Dial a specific PeerId. Creates an RtcPeerConnection, sends an SDP
    /// offer via the signaler, awaits the answer + ICE candidates,
    /// returns once the datachannel is open.
    ///
    /// PeerAddr encoding: postcard of the target PeerId (we're dialing by
    /// pubkey, not URL; the Signaler is what resolves "where to send").
    async fn connect(&self, addr: PeerAddr) -> Result<Self::Connection>;

    /// Listen for inbound dial requests (offers arriving via the
    /// signaler). Returns the next ready connection.
    async fn accept(&self) -> Result<Self::Connection>;
}
```

The accept-side runs a long-lived task that drains `signaler.recv()` and dispatches inbound offers to new RtcPeerConnections.

### Multi-transport adapter

`crates/sunset-sync/src/multi_transport.rs` (NEW). A composable wrapper that lets `SyncEngine` use two transports at once. The engine stays single-transport-shaped; multiplexing is invisible to it.

```rust
pub struct MultiTransport<T1: Transport, T2: Transport> {
    primary: T1,
    secondary: T2,
}

impl<T1: Transport, T2: Transport> MultiTransport<T1, T2>
where
    T1::Connection: 'static,
    T2::Connection: 'static,
{
    pub fn new(primary: T1, secondary: T2) -> Self;
}

#[async_trait(?Send)]
impl<T1: Transport, T2: Transport> Transport for MultiTransport<T1, T2>
where
    T1::Connection: 'static,
    T2::Connection: 'static,
{
    type Connection = MultiConnection<T1::Connection, T2::Connection>;

    async fn connect(&self, addr: PeerAddr) -> Result<Self::Connection> {
        // Caller must indicate which transport via a prefix on PeerAddr,
        // OR we route by trial: try secondary first (WebRTC), fall back
        // to primary (relay) on error. v1: explicit prefix.
        // PeerAddr starts with "ws://..." → primary; "webrtc://..." →
        // secondary. Other prefixes → error.
    }

    async fn accept(&self) -> Result<Self::Connection> {
        // Race both accept loops; whichever yields first.
        tokio::select! {
            r = self.primary.accept() => Ok(MultiConnection::Primary(r?)),
            r = self.secondary.accept() => Ok(MultiConnection::Secondary(r?)),
        }
    }
}

// Sum-type connection wrapping either transport's Connection.
pub enum MultiConnection<C1, C2> { Primary(C1), Secondary(C2) }

#[async_trait(?Send)]
impl<C1: TransportConnection, C2: TransportConnection> TransportConnection
for MultiConnection<C1, C2> {
    async fn send_reliable(&self, b: Bytes) -> Result<()> {
        match self {
            MultiConnection::Primary(c) => c.send_reliable(b).await,
            MultiConnection::Secondary(c) => c.send_reliable(b).await,
        }
    }
    // ... etc for recv_reliable, peer_id, close, unreliable
}
```

PeerAddr scheme: `ws://...#x25519=...` selects primary (WebSocket). New `webrtc://<base64-peer-id>` scheme selects secondary (WebRTC). The MultiTransport routes based on URL prefix.

### Sunset-web-wasm Client integration

New methods on the JS-exported `Client`:

```rust
#[wasm_bindgen]
impl Client {
    /// Establish a direct WebRTC connection to the peer with the given
    /// Ed25519 pubkey. Requires the relay connection to already be up
    /// (signaling rides through it).
    pub async fn connect_direct(&self, peer_pubkey: &[u8]) -> Result<(), JsError>;

    /// Get the current connection mode for a peer: "via_relay" |
    /// "direct" | "unknown".
    pub fn peer_connection_mode(&self, peer_pubkey: &[u8]) -> String;
}
```

The Gleam UI exposes these via the `sunset.gleam` externals, with a small UI affordance (the existing connection-status badge gains a per-peer "direct" indicator).

### Connection establishment flow (full picture)

```text
Browser A (initiator)                    Browser B (responder)

  client.connect_direct(B_pubkey)
        │
        ▼
  WebRtcRawTransport::connect(B_pubkey)
        │
        ▼
  Create RtcPeerConnection + datachannel
        │
        ▼
  Create offer SDP
        │
        ▼
  signaler.send(SignalMessage{
      from: A, to: B, seq: 1,
      payload: postcard(Offer(sdp))
  })
        │
        ▼ [via SyncEngine → relay → SyncEngine]
                                              │
                                              ▼
                                        signaler.recv()
                                              │
                                              ▼
                                        RtcPeerConnection.setRemoteDescription(offer)
                                              │
                                              ▼
                                        Create answer SDP
                                              │
                                              ▼
                                        signaler.send(Answer(sdp))
        │
        ▼
  signaler.recv() → Answer(sdp)
        │
        ▼
  RtcPeerConnection.setRemoteDescription(answer)
        │
        ▼
  ICE candidates exchanged via signaler in both directions
        │
        ▼
  Datachannel onopen fires
        │
        ▼
  WebRtcRawConnection ready; SyncEngine starts using it
```

After establishment, sync messages flow over the datachannel. The relay still gets messages too (because the SyncEngine pushes to all known subscriptions including the relay's) — but the latency of the direct path is 1 hop instead of 2.

For the kill-relay test specifically: once the WebRTC connection is up, the SyncEngine has the WebRTC peer in its peer list. When the WebSocket connection to the relay drops, that peer in the list is gone but the WebRTC peer remains. New messages get pushed only to the WebRTC peer. Test passes.

## Tests + verification

- **Native build**: `cargo build -p sunset-sync-webrtc-browser` (uses stub).
- **Wasm build**: `cargo build -p sunset-sync-webrtc-browser --target wasm32-unknown-unknown`.
- **wasm-bindgen-test** (compile + construct): `WebRtcRawTransport::new(...)` produces a value.
- **Headline Playwright e2e**: kill-relay scenario described above.
- **Workspace tests**: continue to pass.
- **All Nix builds**: continue to pass.

## Items deferred

- Native WebRTC transport (`sunset-sync-webrtc-native`) — relays don't need it for chat. Voice will.
- TURN servers / configurable ICE config.
- Auto-upgrade to WebRTC when both peers learn each other's pubkeys.
- Reconnection after WebRTC drops.
- AEAD encryption of SDP/ICE signaling content (signed but plaintext in v1; encrypted in V1.5 before voice).
- Renegotiation for adding/removing media tracks.
- DataChannel-based reliable channel as the relay's main transport (would let relays use WebRTC for federation too — voice prereq).
- Browser detection / fallback messaging for browsers without WebRTC support (every modern browser supports it; no real-world concern).

## Self-review checklist

- [x] Four non-negotiables (direct WebRTC works, signaling reuses existing stack, kill-relay test passes, opt-in via connect_direct) are met by named mechanisms.
- [x] Signaling wire format (name + payload + crypto) is concrete.
- [x] Multi-transport approach (MultiTransport adapter) doesn't change SyncEngine's surface — composable.
- [x] V1.5 gap (encryption of SDP/ICE signaling content) is acknowledged and tied to voice prerequisite.
- [x] Out-of-scope items prevent scope creep.
- [x] Threat-model additions for WebRTC are explicit.
- [x] Headline acceptance test (kill-relay) is concrete enough to plan against.
