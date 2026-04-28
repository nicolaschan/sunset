//// FFI bindings around the sunset-web-wasm bundle: identity persistence,
//// engine lifecycle, message send/recv. Uses callback-based externals so
//// Lustre effects can wrap each async operation.

pub type ClientHandle

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
  room_name: String,
  callback: fn(ClientHandle) -> Nil,
) -> Nil

/// Open a connection to a relay. URL must include the `#x25519=<hex>`
/// fragment. Calls `callback` with `Ok(Nil)` on success or `Error(msg)`.
@external(javascript, "./sunset.ffi.mjs", "addRelay")
pub fn add_relay(
  client: ClientHandle,
  url: String,
  callback: fn(Result(Nil, String)) -> Nil,
) -> Nil

/// Subscribe the engine to "all messages in this room".
@external(javascript, "./sunset.ffi.mjs", "publishRoomSubscription")
pub fn publish_room_subscription(
  client: ClientHandle,
  callback: fn(Result(Nil, String)) -> Nil,
) -> Nil

/// Compose + insert a message. `callback` receives the value-hash hex on
/// success.
@external(javascript, "./sunset.ffi.mjs", "sendMessage")
pub fn send_message(
  client: ClientHandle,
  body: String,
  sent_at_ms: Int,
  callback: fn(Result(String, String)) -> Nil,
) -> Nil

/// Register the per-message callback. Fires once per current + future
/// message in the room.
@external(javascript, "./sunset.ffi.mjs", "onMessage")
pub fn on_message(
  client: ClientHandle,
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
  client: ClientHandle,
  peer_pubkey: BitArray,
  callback: fn(Result(Nil, String)) -> Nil,
) -> Nil

/// Synchronous accessor: returns "direct", "via_relay", or "unknown" for
/// the given remote peer's pubkey.
@external(javascript, "./sunset.ffi.mjs", "clientPeerConnectionMode")
pub fn client_peer_connection_mode(
  client: ClientHandle,
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
