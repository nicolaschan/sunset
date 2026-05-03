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

/// Open a connection to a relay. URL must include the `#x25519=<hex>`
/// fragment. Calls `callback` with `Ok(Nil)` on success or `Error(msg)`.
@external(javascript, "./sunset.ffi.mjs", "addRelay")
pub fn add_relay(
  client: ClientHandle,
  url: String,
  callback: fn(Result(Nil, String)) -> Nil,
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
pub fn on_message(
  room: RoomHandle,
  callback: fn(IncomingMessage) -> Nil,
) -> Nil

/// Synchronous accessor for the current relay status. Returns one of
/// "disconnected", "connecting", "connected", "error".
@external(javascript, "./sunset.ffi.mjs", "relayStatus")
pub fn relay_status(client: ClientHandle) -> String

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

@external(javascript, "./sunset.ffi.mjs", "onRelayStatusChanged")
pub fn on_relay_status_changed(
  room: RoomHandle,
  callback: fn(String) -> Nil,
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

/// Schedule a recurring callback every `ms` milliseconds. Used by the
/// popover ticker; runs for the page lifetime, no cancel handle in v1.
@external(javascript, "./sunset.ffi.mjs", "setIntervalMs")
pub fn set_interval_ms(ms: Int, callback: fn() -> Nil) -> Nil

/// Wall-clock unix-ms snapshot via JS `Date.now()`.
@external(javascript, "./sunset.ffi.mjs", "nowMs")
pub fn now_ms() -> Int

/// JS-side IncomingReceipt object, opaque to Gleam.
pub type IncomingReceipt

/// Subscribe to delivery receipts. The callback fires once per receipt
/// authored by a peer other than us; self-receipts are dropped at the
/// bridge layer.
@external(javascript, "./sunset.ffi.mjs", "onReceipt")
pub fn on_receipt(
  room: RoomHandle,
  callback: fn(IncomingReceipt) -> Nil,
) -> Nil

/// Hex-encoded value_hash of the Text that this Receipt acknowledges.
@external(javascript, "./sunset.ffi.mjs", "recForValueHashHex")
pub fn rec_for_value_hash_hex(r: IncomingReceipt) -> String

/// Verifying key of the peer who authored this Receipt.
@external(javascript, "./sunset.ffi.mjs", "recFromPubkey")
pub fn rec_from_pubkey(r: IncomingReceipt) -> BitArray
