pub type Route {
  Home
  Dev
}

pub type ChatMessage {
  ChatMessage(sender: String, body: String)
}

pub type Model {
  Model(
    route: Route,
    peer_id: String,
    status: String,
    multiaddr_input: String,
    addresses: List(String),
    peers: List(String),
    connection_count: Int,
    error: String,
    chat_input: String,
    messages: List(ChatMessage),
    audio_sending: Bool,
    audio_receiving: Bool,
    audio_error: String,
  )
}

pub type Msg {
  RouteChanged(route: Route)
  Libp2pInitialised(peer_id: String)
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
  AudioStarted
  AudioFailed(error: String)
}
