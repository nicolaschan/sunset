import gleam/int
import gleam/list
import lustre/attribute.{autofocus, class, classes, placeholder, type_, value}
import lustre/element.{type Element, text}
import lustre/element/html.{button, div, form, input, li, span, ul}
import lustre/event.{on_click, on_input, on_submit}
import sunset/model.{
  type Model, type Msg, RelayConnected, RelayConnecting, RelayDisconnected,
  RelayFailed, UserClickedCancelEditName, UserClickedEditName,
  UserClickedJoinAudio, UserClickedLeaveAudio, UserClickedLeaveRoom,
  UserClickedSaveName, UserToggledNodeInfo, UserUpdatedNameInput, client_version,
}
import sunset/view/chat
import sunset/view/peers

pub fn view(model: Model) -> Element(Msg) {
  div([class("room")], [
    // Sidebar
    div([class("room-sidebar")], [
      view_header(model),
      div([class("room-sidebar-body")], [
        view_status(model),
        view_node_info(model),
        view_peers(model),
      ]),
      view_voice(model),
    ]),
    // Main chat area
    div([class("room-main")], [
      chat.view_messages(model),
      chat.view_input(model),
    ]),
    // Peer detail modal
    peers.view_peer_modal(model),
  ])
}

// -- Sidebar sections --

fn view_header(model: Model) -> Element(Msg) {
  div([class("room-sidebar-header")], [
    button([on_click(UserClickedLeaveRoom), class("room-back")], [
      text("\u{2190}"),
    ]),
    div([class("room-name")], [text(model.room_name)]),
  ])
}

fn view_status(model: Model) -> Element(Msg) {
  button([on_click(UserToggledNodeInfo), class("room-status")], [
    span(
      [
        classes([
          #("room-status-dot", True),
          #("room-status-online", model.relay_status == RelayConnected),
          #(
            "room-status-connecting",
            model.relay_status == RelayConnecting || model.status != "Online",
          ),
        ]),
      ],
      [],
    ),
    span([class("room-status-text")], [
      text(case model.status, model.relay_status {
        "Online", RelayConnected -> "Connected"
        "Online", RelayConnecting -> "Connecting to relay..."
        "Online", RelayFailed(err) -> "Relay failed: " <> err
        "Online", RelayDisconnected -> "Ready"
        _, _ -> model.status
      }),
    ]),
    span([class("room-status-chevron")], [
      text(case model.show_node_info {
        True -> "\u{25B4}"
        False -> "\u{25BE}"
      }),
    ]),
  ])
}

fn view_node_info(model: Model) -> Element(Msg) {
  case model.show_node_info {
    False -> text("")
    True ->
      div([class("room-node-info")], [
        div([class("room-node-info-row")], [
          span([class("room-node-info-label")], [text("Peer ID")]),
          span([class("room-node-info-value")], [
            text(case model.peer_id {
              "" -> "..."
              pid -> pid
            }),
          ]),
        ]),
        div([class("room-node-info-row")], [
          span([class("room-node-info-label")], [text("Version")]),
          span([class("room-node-info-value")], [text(client_version)]),
        ]),
        div([class("room-node-info-row")], [
          span([class("room-node-info-label")], [
            text(
              "Addresses ("
              <> int.to_string(list.length(model.addresses))
              <> ")",
            ),
          ]),
        ]),
        case model.addresses {
          [] -> div([class("room-node-info-empty")], [text("No addresses yet")])
          addrs ->
            ul(
              [class("room-node-info-addrs")],
              list.map(addrs, fn(addr) {
                li([class("room-node-info-addr")], [text(addr)])
              }),
            )
        },
      ])
  }
}

fn view_peers(model: Model) -> Element(Msg) {
  div([class("room-peers")], [
    div([class("room-peers-header")], [
      div([class("room-peers-title")], [
        text(
          "Connected (" <> int.to_string(peers.connection_count(model)) <> ")",
        ),
      ]),
      view_name_editor(model),
    ]),
    case model.peers {
      [] ->
        case model.disconnected_peers {
          [] -> div([class("room-peers-empty")], [text("No peers yet")])
          _ -> text("")
        }
      peer_list ->
        ul(
          [class("room-peers-list")],
          list.map(peer_list, fn(peer_id) {
            peers.view_peer_item(model, peer_id, False)
          }),
        )
    },
    // Recently-disconnected peers (shown with red dot)
    case
      list.filter(model.disconnected_peers, fn(entry) {
        !list.contains(model.peers, entry.0)
      })
    {
      [] -> text("")
      disconnected ->
        ul(
          [class("room-peers-list room-peers-list-disconnected")],
          list.map(disconnected, fn(entry) {
            peers.view_peer_item(model, entry.0, True)
          }),
        )
    },
  ])
}

fn view_name_editor(model: Model) -> Element(Msg) {
  case model.editing_name {
    False ->
      button([on_click(UserClickedEditName), class("room-name-edit-btn")], [
        text(case model.display_name {
          "" -> "Set name"
          name -> name
        }),
        span([class("room-name-edit-icon")], [text("\u{270E}")]),
      ])
    True ->
      form([on_submit(fn(_) { UserClickedSaveName }), class("room-name-form")], [
        input([
          type_("text"),
          placeholder("Display name"),
          value(model.name_input),
          on_input(UserUpdatedNameInput),
          class("room-name-input"),
          autofocus(True),
        ]),
        button([type_("submit"), class("room-name-save-btn")], [
          text("\u{2713}"),
        ]),
        button(
          [
            type_("button"),
            on_click(UserClickedCancelEditName),
            class("room-name-cancel-btn"),
          ],
          [text("\u{00D7}")],
        ),
      ])
  }
}

// -- Voice controls --

fn view_voice(model: Model) -> Element(Msg) {
  div([class("room-voice")], [
    div([class("room-voice-row")], [
      case model.audio_joined {
        True ->
          button(
            [
              on_click(UserClickedLeaveAudio),
              class("room-voice-btn room-voice-btn-active"),
            ],
            [text("Leave audio")],
          )
        False ->
          button([on_click(UserClickedJoinAudio), class("room-voice-btn")], [
            text("Join audio"),
          ])
      },
    ]),
    case model.audio_error {
      "" -> text("")
      err -> div([class("room-voice-error")], [text(err)])
    },
  ])
}
