import gleam/option.{type Option}

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
    peer_addrs: List(#(String, String, String)),
    connection_count: Int,
    error: String,
    chat_input: String,
    messages: List(ChatMessage),
    audio_sending: Bool,
    audio_receiving: Bool,
    audio_joined: Bool,
    audio_error: String,
    selected_peer: Option(String),
    peer_audio_states: List(#(String, Bool, Bool)),
    audio_pc_states: List(#(String, String)),
    disconnected_peers: List(String),
    display_name: String,
    editing_name: Bool,
    name_input: String,
    peer_names: List(#(String, String)),
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
  UserClickedStartAudio
  UserClickedStopAudio
  UserClickedJoinAudio
  UserClickedLeaveAudio
  AudioStarted
  AudioFailed(error: String)
  PeerDiscovered(peer_id: String, addrs: List(String))
  PeerDialSucceeded
  PeerDialFailed(error: String)
  UserClickedPeer(peer_id: String)
  UserClosedPeerModal
  UserClickedEditName
  UserUpdatedNameInput(value: String)
  UserClickedSaveName
  UserClickedCancelEditName
}
