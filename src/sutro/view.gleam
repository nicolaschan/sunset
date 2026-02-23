import gleam/int
import gleam/list
import lustre/attribute.{class, classes, href, placeholder, type_, value}
import lustre/element.{type Element, text}
import lustre/element/html.{
  a, button, div, h1, h2, hr, input, li, p, section, span, ul,
}
import lustre/event.{on_click, on_input}
import sutro/model.{
  type Model, type Msg, Dev, Home, UserClickedConnect, UserClickedSend,
  UserClickedStartAudio, UserClickedStopAudio, UserUpdatedChatInput,
  UserUpdatedMultiaddr,
}

pub fn view(model: Model) -> Element(Msg) {
  case model.route {
    Home -> view_home(model)
    Dev -> view_dev(model)
  }
}

// -- Home page (new index) --

fn view_home(_model: Model) -> Element(Msg) {
  div([class("app-container")], [
    h1([class("app-title")], [text("Sutro")]),
    p([], [text("Welcome to Sutro -- a peer-to-peer chat app.")]),
    p([], [
      a([href("/dev")], [text("Dev dashboard")]),
    ]),
  ])
}

// -- Dev page (original index) --

fn view_dev(model: Model) -> Element(Msg) {
  div([class("app-container")], [
    h1([class("app-title")], [text("Dev Dashboard")]),
    p([], [a([href("/")], [text("Back to home")])]),
    // Node info section
    section([class("app-section")], [
      h2([class("section-title")], [text("Node")]),
      view_node_info(model),
    ]),
    hr([class("app-divider")]),
    // Connect section
    section([class("app-section")], [
      h2([class("section-title")], [text("Connect to Peer")]),
      view_connect_form(model),
    ]),
    hr([class("app-divider")]),
    // Addresses section
    section([class("app-section")], [
      h2([class("section-title")], [
        text(
          "My Addresses (" <> int.to_string(list.length(model.addresses)) <> ")",
        ),
      ]),
      view_addresses(model),
    ]),
    hr([class("app-divider")]),
    // Connected peers section
    section([class("app-section")], [
      h2([class("section-title")], [
        text(
          "Connected Peers (" <> int.to_string(model.connection_count) <> ")",
        ),
      ]),
      view_peers(model),
    ]),
    hr([class("app-divider")]),
    // Chat section
    section([], [
      h2([class("section-title")], [text("Chat")]),
      view_chat(model),
    ]),
    hr([class("app-divider")]),
    // Audio section
    section([class("app-section")], [
      h2([class("section-title")], [text("Audio")]),
      view_audio(model),
    ]),
  ])
}

fn view_node_info(model: Model) -> Element(Msg) {
  ul([class("node-info-list")], [
    li([], [
      text("Peer ID: "),
      span([class("info-value")], [
        text(case model.peer_id {
          "" -> "..."
          pid -> pid
        }),
      ]),
    ]),
    li([], [
      text("Status: "),
      span(
        [
          classes([
            #("status-online", model.status == "Online"),
            #("status-pending", model.status != "Online"),
          ]),
        ],
        [text(model.status)],
      ),
    ]),
  ])
}

fn view_connect_form(model: Model) -> Element(Msg) {
  div([], [
    div([class("connect-form")], [
      input([
        type_("text"),
        placeholder("/ip4/... or /dns4/..."),
        value(model.multiaddr_input),
        on_input(UserUpdatedMultiaddr),
        class("connect-input"),
      ]),
      button([on_click(UserClickedConnect), class("connect-button")], [
        text("Connect"),
      ]),
    ]),
    case model.error {
      "" -> text("")
      err -> p([class("error-text")], [text(err)])
    },
  ])
}

fn view_addresses(model: Model) -> Element(Msg) {
  case model.addresses {
    [] ->
      p([class("empty-text")], [
        text("No addresses yet. Connect to a relay to get addresses."),
      ])
    addrs ->
      ul(
        [class("address-list")],
        list.map(addrs, fn(addr) { li([class("address-item")], [text(addr)]) }),
      )
  }
}

fn view_peers(model: Model) -> Element(Msg) {
  case model.peers {
    [] -> p([class("empty-text")], [text("No peers connected.")])
    peers ->
      ul(
        [class("peer-list")],
        list.map(peers, fn(peer_id) {
          li([class("peer-item")], [text(peer_id)])
        }),
      )
  }
}

fn view_chat(model: Model) -> Element(Msg) {
  div([], [
    div([class("chat-messages")], case model.messages {
      [] -> [p([class("empty-text")], [text("No messages yet.")])]
      msgs ->
        list.map(list.reverse(msgs), fn(msg) {
          div([class("chat-message")], [
            span([class("chat-sender")], [text(msg.sender <> ": ")]),
            span([], [text(msg.body)]),
          ])
        })
    }),
    div([class("chat-form")], [
      input([
        type_("text"),
        placeholder("Type a message..."),
        value(model.chat_input),
        on_input(UserUpdatedChatInput),
        class("chat-input"),
      ]),
      button([on_click(UserClickedSend), class("chat-send-button")], [
        text("Send"),
      ]),
    ]),
  ])
}

fn view_audio(model: Model) -> Element(Msg) {
  div([class("audio-controls")], [
    case model.audio_sending {
      True ->
        button(
          [on_click(UserClickedStopAudio), class("audio-button audio-stop")],
          [text("Stop Mic")],
        )
      False ->
        button(
          [on_click(UserClickedStartAudio), class("audio-button audio-start")],
          [text("Start Mic")],
        )
    },
    div([class("audio-status")], [
      span(
        [
          classes([
            #("audio-indicator", True),
            #("audio-indicator-active", model.audio_sending),
          ]),
        ],
        [
          text(case model.audio_sending {
            True -> "Sending"
            False -> "Mic off"
          }),
        ],
      ),
      span(
        [
          classes([
            #("audio-indicator", True),
            #("audio-indicator-active", model.audio_receiving),
          ]),
        ],
        [
          text(case model.audio_receiving {
            True -> "Receiving"
            False -> "No incoming audio"
          }),
        ],
      ),
    ]),
    case model.audio_error {
      "" -> text("")
      err -> p([class("error-text")], [text(err)])
    },
  ])
}
