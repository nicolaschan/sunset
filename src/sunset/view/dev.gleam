import gleam/int
import gleam/list
import lustre/attribute.{class, classes, href, placeholder, type_, value}
import lustre/element.{type Element, text}
import lustre/element/html.{
  a, button, div, h1, h2, hr, input, li, p, section, span, ul,
}
import lustre/event.{on_click, on_input}
import sunset/model.{
  type Model, type Msg, UserClickedConnect, UserClickedJoinAudio,
  UserClickedLeaveAudio, UserClickedSend, UserUpdatedChatInput,
  UserUpdatedMultiaddr,
}
import sunset/view/peers

pub fn view(model: Model) -> Element(Msg) {
  div([class("page-dev")], [
    div([class("app-container")], [
      h1([class("app-title")], [text("Dev Dashboard")]),
      p([], [a([href("/")], [text("Back to home")])]),
      section([class("app-section")], [
        h2([class("section-title")], [text("Node")]),
        view_node_info(model),
      ]),
      hr([class("app-divider")]),
      section([class("app-section")], [
        h2([class("section-title")], [text("Connect to Peer")]),
        view_connect_form(model),
      ]),
      hr([class("app-divider")]),
      section([class("app-section")], [
        h2([class("section-title")], [
          text(
            "My Addresses ("
            <> int.to_string(list.length(model.addresses))
            <> ")",
          ),
        ]),
        view_addresses(model),
      ]),
      hr([class("app-divider")]),
      section([class("app-section")], [
        h2([class("section-title")], [
          text(
            "Connected Peers ("
            <> int.to_string(peers.connection_count(model))
            <> ")",
          ),
        ]),
        view_peers(model),
      ]),
      hr([class("app-divider")]),
      section([], [
        h2([class("section-title")], [text("Chat")]),
        view_chat(model),
      ]),
      hr([class("app-divider")]),
      section([class("app-section")], [
        h2([class("section-title")], [text("Audio")]),
        view_audio(model),
      ]),
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
    peer_list ->
      ul(
        [class("peer-list")],
        list.map(peer_list, fn(peer_id) {
          let is_relay = peer_id == model.relay_peer_id
          let addr = peers.peer_addr(model, peer_id)
          li([class("peer-item")], [
            text(peer_id),
            case is_relay {
              True -> span([class("room-peer-badge")], [text("relay")])
              False -> text("")
            },
            case addr {
              "" -> text("")
              a -> div([class("room-peer-addr")], [text(a)])
            },
          ])
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
    case model.audio_joined {
      True ->
        button(
          [on_click(UserClickedLeaveAudio), class("audio-button audio-stop")],
          [text("Leave Audio")],
        )
      False ->
        button(
          [on_click(UserClickedJoinAudio), class("audio-button audio-start")],
          [text("Join Audio")],
        )
    },
    div([class("audio-status")], [
      span(
        [
          classes([
            #("audio-indicator", True),
            #("audio-indicator-active", model.audio_joined),
          ]),
        ],
        [
          text(case model.audio_joined {
            True ->
              "Joined ("
              <> int.to_string(list.length(model.audio_connections))
              <> " connections)"
            False -> "Not joined"
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
