import gleam/list
import gleam/option.{type Option}
import gleam/string
import sunset/webrtc/session

pub const client_version = "0.1.1"

// Protocol identifiers
pub const chat_protocol = "/sunset/chat/1.0.0"

pub const audio_presence_protocol = "/sunset/audio-presence/1.0.0"

pub const audio_signaling_protocol = "/sunset/audio-signaling/1.0.0"

// Reconnect backoff constants
pub const reconnect_base_delay_ms = 1000

pub const reconnect_max_delay_ms = 30_000

// Recently-disconnected grace period
pub const disconnect_grace_ms = 30_000

// Discovery polling interval
pub const discovery_poll_ms = 2000

pub type Route {
  Home
  Room(name: String)
  Dev
}

pub type RelayStatus {
  RelayDisconnected
  RelayConnecting
  RelayConnected
  RelayFailed(error: String)
}

pub type ChatMessage {
  ChatMessage(sender: String, body: String)
}

/// Audio presence received from a remote peer.
pub type PeerPresence {
  PeerPresence(joined: Bool, muted: Bool, name: String, version: String)
}

/// A peer connection with its raw multiaddr string.
pub type PeerConnection {
  PeerConnection(peer_id: String, addr: String)
}

pub type Model {
  Model(
    route: Route,
    room_input: String,
    room_name: String,
    peer_id: String,
    status: String,
    relay_status: RelayStatus,
    relay_peer_id: String,
    show_node_info: Bool,
    multiaddr_input: String,
    addresses: List(String),
    peers: List(String),
    // Raw connection data: List of #(peer_id, addr_string)
    connections: List(#(String, String)),
    error: String,
    chat_input: String,
    messages: List(ChatMessage),
    audio_joined: Bool,
    audio_error: String,
    audio_connections: List(#(String, session.Connection)),
    audio_pc_states: List(#(String, String)),
    selected_peer: Option(String),
    peer_presence: List(#(String, PeerPresence)),
    // Recently disconnected peers: List of #(peer_id, disconnect_timestamp_ms)
    disconnected_peers: List(#(String, Float)),
    display_name: String,
    editing_name: Bool,
    name_input: String,
    // Audio reconnect state: List of #(peer_id, attempt_count)
    reconnect_attempts: List(#(String, Int)),
    // Peer IDs with in-progress connection attempts (not yet connected or failed)
    audio_connecting: List(String),
  )
}

pub type Msg {
  RouteChanged(route: Route)
  HashChanged(hash: String)
  UserUpdatedRoomInput(value: String)
  UserClickedJoinRoom
  UserClickedLeaveRoom
  UserToggledNodeInfo
  Libp2pInitialised(peer_id: String)
  PeerConnected(peer_id: String)
  PeerDisconnected(peer_id: String)
  RelayDialSucceeded
  RelayDialFailed(error: String)
  UserUpdatedMultiaddr(value: String)
  UserClickedConnect
  DialSucceeded
  DialFailed(error: String)
  Tick
  UserUpdatedChatInput(value: String)
  UserClickedSend
  SendSucceeded
  SendFailed(error: String)
  ChatMessageReceived(sender: String, body: String)
  UserClickedJoinAudio
  UserClickedLeaveAudio
  AudioConnected(peer_id: String, connection: session.Connection)
  AudioFailed(peer_id: String, error: String)
  PeerDiscovered(peer_id: String, addrs: List(String))
  PeerDialSucceeded
  PeerDialFailed(error: String)
  UserClickedPeer(peer_id: String)
  UserClosedPeerModal
  UserClickedEditName
  UserUpdatedNameInput(value: String)
  UserClickedSaveName
  UserClickedCancelEditName
  PresenceReceived(peer_id: String, message: String)
  AudioPcStateChanged(peer_id: String, state: String)
  ScheduledReconnect(peer_id: String)
}

// -- Helpers --

/// Classify a multiaddr string into a transport name.
pub fn classify_transport(addr: String) -> String {
  case string.contains(addr, "/webrtc/") {
    True -> "WebRTC"
    False ->
      case string.contains(addr, "/p2p-circuit") {
        True -> "Circuit Relay"
        False ->
          case string.contains(addr, "/wss/") {
            True -> "WebSockets (secure)"
            False ->
              case string.contains(addr, "/ws/") {
                True -> "WebSockets"
                False ->
                  case string.contains(addr, "/webtransport/") {
                    True -> "WebTransport"
                    False -> "Other"
                  }
              }
          }
      }
  }
}

/// Check if a multiaddr is a circuit relay connection.
pub fn is_circuit_addr(addr: String) -> Bool {
  string.contains(addr, "/p2p-circuit")
}

/// Get the display name for a peer. Falls back to short peer ID.
pub fn peer_display_name(model: Model, peer_id: String) -> String {
  case list.find(model.peer_presence, fn(entry) { entry.0 == peer_id }) {
    Ok(#(_, presence)) ->
      case presence.name {
        "" -> short_peer_id(peer_id)
        name -> name
      }
    Error(_) -> short_peer_id(peer_id)
  }
}

/// Shorten a peer ID to a displayable form.
pub fn short_peer_id(peer_id: String) -> String {
  let len = string.length(peer_id)
  case len > 12 {
    True ->
      string.slice(peer_id, 0, 6) <> ".." <> string.slice(peer_id, len - 4, 4)
    False -> peer_id
  }
}
