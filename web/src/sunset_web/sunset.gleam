//// FFI bindings around the sunset-web-wasm bundle: identity persistence,
//// engine lifecycle, message send/recv. Uses callback-based externals so
//// Lustre effects can wrap each async operation.

import gleam/option

pub type ClientHandle

pub type RoomHandle

pub type IncomingMessage

/// Read the persisted 32-byte secret seed from localStorage; if absent,
/// generate a fresh one via crypto.getRandomValues + persist it. Calls
/// `callback` with the seed once available.
@external(javascript, "./sunset.ffi.mjs", "loadOrCreateIdentity")
pub fn load_or_create_identity(callback: fn(BitArray) -> Nil) -> Nil

/// Construct a Client. Seed must be 32 bytes. `callback` is called with
/// the opaque ClientHandle once the wasm bundle is loaded + initialised.
@external(javascript, "./sunset.ffi.mjs", "createClient")
pub fn create_client(
  seed: BitArray,
  heartbeat_interval_ms: Int,
  callback: fn(ClientHandle) -> Nil,
) -> Nil

/// Open a room by name. Returns a RoomHandle via `callback` once the room
/// subscription is published and the wasm-side state is initialised.
@external(javascript, "./sunset.ffi.mjs", "clientOpenRoom")
pub fn open_room(
  client: ClientHandle,
  name: String,
  callback: fn(RoomHandle) -> Nil,
) -> Nil

/// Register a durable intent to keep connected to `url`. The
/// callback is fired with `Ok(intent_id)` once the intent is
/// recorded; `Error(msg)` is reserved for malformed input.
@external(javascript, "./sunset.ffi.mjs", "addRelay")
pub fn add_relay(
  client: ClientHandle,
  url: String,
  callback: fn(Result(Float, String)) -> Nil,
) -> Nil

/// Snapshot of one supervisor intent, mirrored from
/// `IntentSnapshotJs`. `kind` is `"primary"` / `"secondary"` / not
/// present.
pub type IntentSnapshot {
  IntentSnapshot(
    id: Float,
    state: String,
    label: String,
    peer_pubkey: option.Option(BitArray),
    kind: option.Option(String),
    attempt: Int,
    /// Wall-clock ms of the most recent Pong. None until the first
    /// Pong of the first connection lands; preserved across Backoff.
    last_pong_at_ms: option.Option(Int),
    /// Round-trip time of the most recent Pong, in milliseconds.
    last_rtt_ms: option.Option(Int),
  )
}

/// Register a callback fired for every intent (once on register,
/// then once per state transition).
@external(javascript, "./sunset.ffi.mjs", "onIntentChanged")
pub fn on_intent_changed(
  client: ClientHandle,
  callback: fn(IntentSnapshot) -> Nil,
) -> Nil

/// Compose + insert a message. `callback` receives the value-hash hex on
/// success.
@external(javascript, "./sunset.ffi.mjs", "sendMessage")
pub fn send_message(
  room: RoomHandle,
  body: String,
  sent_at_ms: Int,
  callback: fn(Result(String, String)) -> Nil,
) -> Nil

/// Register the per-message callback. Fires once per current + future
/// message in the room.
@external(javascript, "./sunset.ffi.mjs", "onMessage")
pub fn on_message(room: RoomHandle, callback: fn(IncomingMessage) -> Nil) -> Nil

/// Establish a direct WebRTC peer connection. Signaling rides over the
/// existing relay-mediated CRDT replication (Noise_KK encrypted, full
/// PFS). Calls `callback` with `Ok(Nil)` once the WebRTC datachannel +
/// Noise_IK handshake complete or `Error(msg)` on failure.
@external(javascript, "./sunset.ffi.mjs", "clientConnectDirect")
pub fn client_connect_direct(
  room: RoomHandle,
  peer_pubkey: BitArray,
  callback: fn(Result(Nil, String)) -> Nil,
) -> Nil

/// Synchronous accessor: returns "direct", "via_relay", or "unknown" for
/// the given remote peer's pubkey.
@external(javascript, "./sunset.ffi.mjs", "clientPeerConnectionMode")
pub fn client_peer_connection_mode(
  room: RoomHandle,
  peer_pubkey: BitArray,
) -> String

/// Read the `?relay=<url-encoded>` query parameter from the current URL.
/// Returns `Ok(url)` if present, `Error(Nil)` otherwise.
@external(javascript, "./sunset.ffi.mjs", "relayUrlParam")
pub fn relay_url_param() -> Result(String, Nil)

// -- IncomingMessage field accessors --

@external(javascript, "./sunset.ffi.mjs", "incAuthorPubkey")
pub fn inc_author_pubkey(msg: IncomingMessage) -> BitArray

@external(javascript, "./sunset.ffi.mjs", "incEpochId")
pub fn inc_epoch_id(msg: IncomingMessage) -> Int

@external(javascript, "./sunset.ffi.mjs", "incSentAtMs")
pub fn inc_sent_at_ms(msg: IncomingMessage) -> Int

@external(javascript, "./sunset.ffi.mjs", "incBody")
pub fn inc_body(msg: IncomingMessage) -> String

@external(javascript, "./sunset.ffi.mjs", "incValueHashHex")
pub fn inc_value_hash_hex(msg: IncomingMessage) -> String

@external(javascript, "./sunset.ffi.mjs", "incIsSelf")
pub fn inc_is_self(msg: IncomingMessage) -> Bool

pub type MemberJs

/// Start the heartbeat publisher + membership tracker. Idempotent.
@external(javascript, "./sunset.ffi.mjs", "startPresence")
pub fn start_presence(
  room: RoomHandle,
  interval_ms: Int,
  ttl_ms: Int,
  refresh_ms: Int,
) -> Nil

@external(javascript, "./sunset.ffi.mjs", "onMembersChanged")
pub fn on_members_changed(
  room: RoomHandle,
  callback: fn(List(MemberJs)) -> Nil,
) -> Nil

@external(javascript, "./sunset.ffi.mjs", "memPubkey")
pub fn mem_pubkey(m: MemberJs) -> BitArray

@external(javascript, "./sunset.ffi.mjs", "memPresence")
pub fn mem_presence(m: MemberJs) -> String

@external(javascript, "./sunset.ffi.mjs", "memConnectionMode")
pub fn mem_connection_mode(m: MemberJs) -> String

@external(javascript, "./sunset.ffi.mjs", "memIsSelf")
pub fn mem_is_self(m: MemberJs) -> Bool

@external(javascript, "./sunset.ffi.mjs", "memLastHeartbeatMs")
pub fn mem_last_heartbeat_ms(m: MemberJs) -> option.Option(Int)

/// Read presence-cadence params from `?presence_interval=&presence_ttl=&presence_refresh=`.
/// Returns `#(interval_ms, ttl_ms, refresh_ms)`. Defaults: 30000/60000/5000.
@external(javascript, "./sunset.ffi.mjs", "presenceParamsFromUrl")
pub fn presence_params_from_url() -> #(Int, Int, Int)

/// Read `?heartbeat_interval_ms=NNN` from the URL. Returns 0 when
/// absent or unparseable. e2e-only knob.
@external(javascript, "./sunset.ffi.mjs", "heartbeatIntervalMsFromUrl")
pub fn heartbeat_interval_ms_from_url() -> Int

/// Schedule a recurring callback every `ms` milliseconds. Used by the
/// popover ticker; runs for the page lifetime, no cancel handle in v1.
@external(javascript, "./sunset.ffi.mjs", "setIntervalMs")
pub fn set_interval_ms(ms: Int, callback: fn() -> Nil) -> Nil

/// Schedule a one-shot callback after `ms` milliseconds. Used to stagger
/// room-open calls at startup so the Argon2id KDF cost doesn't block.
@external(javascript, "./sunset.ffi.mjs", "setTimeoutMs")
pub fn set_timeout_ms(ms: Int, callback: fn() -> Nil) -> Nil

/// Wall-clock unix-ms snapshot via JS `Date.now()`.
@external(javascript, "./sunset.ffi.mjs", "nowMs")
pub fn now_ms() -> Int

/// JS-side IncomingReceipt object, opaque to Gleam.
pub type IncomingReceipt

/// Subscribe to delivery receipts. The callback fires once per receipt
/// authored by a peer other than us; self-receipts are dropped at the
/// bridge layer.
@external(javascript, "./sunset.ffi.mjs", "onReceipt")
pub fn on_receipt(room: RoomHandle, callback: fn(IncomingReceipt) -> Nil) -> Nil

/// Hex-encoded value_hash of the Text that this Receipt acknowledges.
@external(javascript, "./sunset.ffi.mjs", "recForValueHashHex")
pub fn rec_for_value_hash_hex(r: IncomingReceipt) -> String

/// Verifying key of the peer who authored this Receipt.
@external(javascript, "./sunset.ffi.mjs", "recFromPubkey")
pub fn rec_from_pubkey(r: IncomingReceipt) -> BitArray

/// Wall-clock unix-ms when the acknowledging peer composed this Receipt.
/// Surfaced in the message-details panel as the per-recipient delivered-at
/// stamp.
@external(javascript, "./sunset.ffi.mjs", "recSentAtMs")
pub fn rec_sent_at_ms(r: IncomingReceipt) -> Int

/// Snapshot payload delivered to `on_reactions_changed`. Opaque on the
/// Gleam side; accessors below extract the concrete fields.
pub type IncomingReactionsSnapshot

@external(javascript, "./sunset.ffi.mjs", "reactionsSnapshotTargetHex")
pub fn reactions_snapshot_target_hex(
  snapshot: IncomingReactionsSnapshot,
) -> String

/// Returns the snapshot as a `List(#(emoji, List(#(author_pubkey_hex, sent_at_ms))))`.
/// The FFI side flattens the JS Map<emoji, Map<author_hex, sent_at_ms>> into
/// this shape so Gleam doesn't need to interop with Map/Set directly. The
/// `sent_at_ms` is the unix-ms timestamp of the LWW-winning Add entry — it's
/// what the message-details panel renders next to each reactor.
@external(javascript, "./sunset.ffi.mjs", "reactionsSnapshotEntries")
pub fn reactions_snapshot_entries(
  snapshot: IncomingReactionsSnapshot,
) -> List(#(String, List(#(String, Int))))

/// Register the per-target snapshot callback. Fires on initial replay
/// and again whenever the target's reaction state changes.
@external(javascript, "./sunset.ffi.mjs", "onReactionsChanged")
pub fn on_reactions_changed(
  room: RoomHandle,
  callback: fn(IncomingReactionsSnapshot) -> Nil,
) -> Nil

/// Send a reaction event. `action` is "add" or "remove". The wasm
/// side generates the entry's nonce and sent_at_ms internally.
@external(javascript, "./sunset.ffi.mjs", "sendReaction")
pub fn send_reaction(
  room: RoomHandle,
  target_hex: String,
  emoji: String,
  action: String,
  callback: fn(Result(Nil, String)) -> Nil,
) -> Nil

@external(javascript, "./sunset.ffi.mjs", "clientPublicKeyHex")
pub fn client_public_key_hex(client: ClientHandle) -> String

/// Encode a BitArray as lowercase hex. Used by the Relays popover
/// to render the relay's peer_id.
@external(javascript, "./sunset.ffi.mjs", "bitsToHex")
pub fn bits_to_hex(bits: BitArray) -> String

/// HH:MM:SS local time of a unix-ms timestamp. The exact-seconds form
/// is what the message-details panel renders for delivery acks and
/// reaction timestamps.
@external(javascript, "./sunset.ffi.mjs", "formatTimeMsExact")
pub fn format_time_ms_exact(ms: Int) -> String

/// Lazily registers the `emoji-picker-element` web component. Idempotent;
/// safe to call on every picker open. Resolves the dynamic import on
/// first call, caches the promise on subsequent calls.
@external(javascript, "./sunset.ffi.mjs", "registerEmojiPicker")
pub fn register_emoji_picker() -> Nil
