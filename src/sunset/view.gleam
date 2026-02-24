import gleam/int
import gleam/list
import gleam/option.{None, Some}
import gleam/string
import lustre/attribute.{
  autofocus, class, classes, href, placeholder, title, type_, value,
}
import lustre/element.{type Element, text}
import lustre/element/html.{
  a, button, div, form, h1, h2, hr, input, li, p, section, span, ul,
}
import lustre/event.{on_click, on_input, on_submit}
import sunset/libp2p
import sunset/model.{
  type Model, type Msg, Dev, Home, RelayConnected, RelayConnecting,
  RelayDisconnected, RelayFailed, Room, UserClickedConnect, UserClickedJoinAudio,
  UserClickedJoinRoom, UserClickedLeaveAudio, UserClickedLeaveRoom,
  UserClickedPeer, UserClickedSend, UserClickedStartAudio, UserClickedStopAudio,
  UserClosedPeerModal, UserToggledNodeInfo, UserUpdatedChatInput,
  UserUpdatedMultiaddr, UserUpdatedRoomInput,
}

pub fn view(model: Model) -> Element(Msg) {
  case model.route {
    Home -> view_home(model)
    Room(_) -> view_room(model)
    Dev -> view_dev(model)
  }
}

// -- Home page (landing) --

fn view_home(model: Model) -> Element(Msg) {
  div([class("landing")], [
    div([class("landing-glow")], []),
    div([class("landing-content")], [
      div([class("landing-sun")], []),
      h1([class("landing-title")], [text("Sunset Chat")]),
      div([class("landing-horizon")], []),
      form([on_submit(fn(_) { UserClickedJoinRoom }), class("room-form")], [
        input([
          type_("text"),
          placeholder("Room name"),
          value(model.room_input),
          on_input(UserUpdatedRoomInput),
          class("room-input"),
          autofocus(True),
        ]),
        button([type_("submit"), class("room-button")], [
          text("Join"),
        ]),
      ]),
      a([href("/dev"), class("landing-link")], [text("Dev dashboard")]),
    ]),
  ])
}

// -- Chat room page --

fn view_room(model: Model) -> Element(Msg) {
  div([class("room")], [
    // Sidebar
    div([class("room-sidebar")], [
      // Room header (pinned top)
      div([class("room-sidebar-header")], [
        button([on_click(UserClickedLeaveRoom), class("room-back")], [
          text("\u{2190}"),
        ]),
        div([class("room-name")], [text(model.room_name)]),
      ]),
      // Scrollable middle section
      div([class("room-sidebar-body")], [
        // Connection status (clickable to toggle node info)
        button([on_click(UserToggledNodeInfo), class("room-status")], [
          span(
            [
              classes([
                #("room-status-dot", True),
                #("room-status-online", model.relay_status == RelayConnected),
                #(
                  "room-status-connecting",
                  model.relay_status == RelayConnecting
                    || model.status != "Online",
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
        ]),
        // Node info panel (collapsible)
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
                span([class("room-node-info-label")], [
                  text(
                    "Addresses ("
                    <> int.to_string(list.length(model.addresses))
                    <> ")",
                  ),
                ]),
              ]),
              case model.addresses {
                [] ->
                  div([class("room-node-info-empty")], [
                    text("No addresses yet"),
                  ])
                addrs ->
                  ul(
                    [class("room-node-info-addrs")],
                    list.map(addrs, fn(addr) {
                      li([class("room-node-info-addr")], [text(addr)])
                    }),
                  )
              },
            ])
        },
        // Peers list
        div([class("room-peers")], [
          div([class("room-peers-title")], [
            text("Connected (" <> int.to_string(model.connection_count) <> ")"),
          ]),
          case model.peers {
            [] ->
              case model.disconnected_peers {
                [] -> div([class("room-peers-empty")], [text("No peers yet")])
                _ -> text("")
              }
            peers ->
              ul(
                [class("room-peers-list")],
                list.map(peers, fn(peer_id) {
                  view_peer_item(model, peer_id, False)
                }),
              )
          },
          // Recently-disconnected peers (shown with red dot)
          case
            list.filter(model.disconnected_peers, fn(pid) {
              !list.contains(model.peers, pid)
            })
          {
            [] -> text("")
            disconnected ->
              ul(
                [class("room-peers-list room-peers-list-disconnected")],
                list.map(disconnected, fn(peer_id) {
                  view_peer_item(model, peer_id, True)
                }),
              )
          },
        ]),
      ]),
      // Voice call controls (pinned bottom)
      div([class("room-voice")], [
        div([class("room-voice-row")], [
          // Join / Leave audio listening
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
              button(
                [
                  on_click(UserClickedJoinAudio),
                  class("room-voice-btn"),
                ],
                [text("Join audio")],
              )
          },
          // Mic on/off icon (only shown when joined)
          case model.audio_joined {
            True ->
              case model.audio_sending {
                True ->
                  button(
                    [
                      on_click(UserClickedStopAudio),
                      class("room-voice-icon-btn room-voice-icon-btn-active"),
                      title("Turn off mic"),
                    ],
                    [text("\u{1F3A4}")],
                  )
                False ->
                  button(
                    [
                      on_click(UserClickedStartAudio),
                      class("room-voice-icon-btn"),
                      title("Turn on mic"),
                    ],
                    [text("\u{1F507}")],
                  )
              }
            False -> text("")
          },
        ]),
        case model.audio_error {
          "" -> text("")
          err -> div([class("room-voice-error")], [text(err)])
        },
      ]),
    ]),
    // Main chat area
    div([class("room-main")], [
      // Messages
      div([class("room-messages")], case model.messages {
        [] -> [
          div([class("room-messages-empty")], [
            div([class("room-messages-empty-icon")], [text("\u{2600}")]),
            div([], [text("No messages yet")]),
            div([class("room-messages-empty-sub")], [
              text("Say something to get the conversation started"),
            ]),
          ]),
        ]
        msgs ->
          list.map(list.reverse(msgs), fn(msg) {
            div(
              [
                classes([
                  #("room-msg", True),
                  #("room-msg-self", msg.sender == "You"),
                  #("room-msg-system", msg.sender == "System"),
                ]),
              ],
              [
                case msg.sender {
                  "You" -> text("")
                  _ -> span([class("room-msg-sender")], [text(msg.sender)])
                },
                div([class("room-msg-body")], [text(msg.body)]),
              ],
            )
          })
      }),
      // Chat input
      form([on_submit(fn(_) { UserClickedSend }), class("room-input-bar")], [
        input([
          type_("text"),
          placeholder("Type a message..."),
          value(model.chat_input),
          on_input(UserUpdatedChatInput),
          class("room-chat-input"),
          autofocus(True),
        ]),
        button([type_("submit"), class("room-send-button")], [
          text("Send"),
        ]),
      ]),
    ]),
    // Peer detail modal
    view_peer_modal(model),
  ])
}

// -- Dev page (original index) --

fn view_dev(model: Model) -> Element(Msg) {
  div([class("page-dev")], [
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
            "My Addresses ("
            <> int.to_string(list.length(model.addresses))
            <> ")",
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
          let is_relay = peer_id == model.relay_peer_id
          let addr = peer_addr(model, peer_id)
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
    // Join / Leave audio listening
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
    // Start / Stop mic
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
            #("audio-indicator-active", model.audio_joined),
          ]),
        ],
        [
          text(case model.audio_joined {
            True -> "Listening"
            False -> "Audio off"
          }),
        ],
      ),
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

// -- Helpers --

/// Render a single peer list item. `is_disconnected` controls whether
/// this peer is shown with the red disconnected dot.
fn view_peer_item(
  model: Model,
  peer_id: String,
  is_disconnected: Bool,
) -> Element(Msg) {
  let is_relay = peer_id == model.relay_peer_id
  let transport = peer_transport(model, peer_id)
  let is_circuit = transport == "Circuit Relay"
  let addr = peer_addr(model, peer_id)
  let #(audio_joined, audio_muted) = peer_audio_state(model, peer_id)
  let rtc_state = peer_audio_pc_state(model, peer_id)
  li(
    [
      classes([
        #("room-peer", True),
        #("room-peer-relay", is_relay),
        #("room-peer-disconnected", is_disconnected),
      ]),
      on_click(UserClickedPeer(peer_id)),
    ],
    [
      span(
        [
          classes([
            #("room-peer-dot", True),
            #("room-peer-dot-relay", is_relay),
            #("room-peer-dot-circuit", is_circuit && !is_relay),
            #("room-peer-dot-disconnected", is_disconnected),
            #(
              "room-peer-dot-rtc-connected",
              rtc_state == "connected" && !is_disconnected,
            ),
            #(
              "room-peer-dot-rtc-connecting",
              { rtc_state == "new" || rtc_state == "connecting" }
                && !is_disconnected,
            ),
          ]),
        ],
        [],
      ),
      div([class("room-peer-info")], [
        div([class("room-peer-name")], [
          span([class("room-peer-id")], [
            case is_relay {
              True -> text(relay_display_name(addr))
              False -> text(short_peer_id(peer_id))
            },
          ]),
          case is_relay {
            True -> span([class("room-peer-badge")], [text("relay")])
            False -> text("")
          },
          case is_disconnected {
            True ->
              span([class("room-peer-badge room-peer-badge-disconnected")], [
                text("reconnecting"),
              ])
            False -> text("")
          },
          // Audio presence indicator
          case is_relay, audio_joined {
            True, _ -> text("")
            _, False -> text("")
            _, True ->
              span(
                [
                  classes([
                    #("room-peer-audio", True),
                    #("room-peer-audio-muted", audio_muted),
                  ]),
                  title(case audio_muted {
                    True -> "In audio (muted)"
                    False -> "In audio"
                  }),
                ],
                [
                  text(case audio_muted {
                    True -> "\u{1F507}"
                    False -> "\u{1F3A4}"
                  }),
                ],
              )
          },
        ]),
        case is_relay {
          True -> text("")
          False ->
            case addr {
              "" -> text("")
              a ->
                div([class("room-peer-addr"), title(a)], [
                  text(a),
                ])
            }
        },
      ]),
    ],
  )
}

fn short_peer_id(peer_id: String) -> String {
  let len = string.length(peer_id)
  case len > 12 {
    True ->
      string.slice(peer_id, 0, 6) <> ".." <> string.slice(peer_id, len - 4, 4)
    False -> peer_id
  }
}

fn relay_display_name(addr: String) -> String {
  case string.split(addr, "/") {
    ["", "dns", hostname, ..] -> hostname
    _ -> "relay"
  }
}

fn peer_addr(model: Model, peer_id: String) -> String {
  case list.find(model.peer_addrs, fn(pair) { pair.0 == peer_id }) {
    Ok(#(_, addr, _)) -> addr
    Error(_) -> ""
  }
}

fn peer_transport(model: Model, peer_id: String) -> String {
  case list.find(model.peer_addrs, fn(pair) { pair.0 == peer_id }) {
    Ok(#(_, _, transport)) -> transport
    Error(_) -> ""
  }
}

/// Look up a peer's audio presence.  Returns #(joined, muted).
fn peer_audio_state(model: Model, peer_id: String) -> #(Bool, Bool) {
  case list.find(model.peer_audio_states, fn(entry) { entry.0 == peer_id }) {
    Ok(#(_, joined, muted)) -> #(joined, muted)
    Error(_) -> #(False, False)
  }
}

/// Look up a peer's audio PC connection state.
/// Returns "" if no audio PC, or the connectionState string.
fn peer_audio_pc_state(model: Model, peer_id: String) -> String {
  case list.find(model.audio_pc_states, fn(entry) { entry.0 == peer_id }) {
    Ok(#(_, state)) -> state
    Error(_) -> ""
  }
}

fn view_peer_modal(model: Model) -> Element(Msg) {
  case model.selected_peer {
    None -> text("")
    Some(peer_id) -> {
      let is_relay = peer_id == model.relay_peer_id
      let raw_addrs = libp2p.get_peer_addrs(peer_id)
      let addrs =
        list.filter_map(raw_addrs, fn(pair) {
          case pair {
            [transport, addr] -> Ok(#(transport, addr))
            _ -> Error(Nil)
          }
        })
      div([class("modal-overlay")], [
        div([class("modal-backdrop"), on_click(UserClosedPeerModal)], []),
        div([class("modal-card")], [
          div([class("modal-header")], [
            div([class("modal-title-row")], [
              span(
                [
                  classes([
                    #("room-peer-dot", True),
                    #("room-peer-dot-relay", is_relay),
                  ]),
                ],
                [],
              ),
              span([class("modal-title")], [text(short_peer_id(peer_id))]),
              case is_relay {
                True -> span([class("room-peer-badge")], [text("relay")])
                False -> text("")
              },
            ]),
            button([on_click(UserClosedPeerModal), class("modal-close")], [
              text("\u{00D7}"),
            ]),
          ]),
          div([class("modal-section")], [
            div([class("modal-section-label")], [text("Peer ID")]),
            div([class("modal-mono")], [text(peer_id)]),
          ]),
          div([class("modal-section")], [
            div([class("modal-section-label")], [
              text("Addresses (" <> int.to_string(list.length(addrs)) <> ")"),
            ]),
            case addrs {
              [] -> div([class("modal-empty")], [text("No connections")])
              _ ->
                ul(
                  [class("modal-addr-list")],
                  list.map(addrs, fn(pair) {
                    li([class("modal-addr-item")], [
                      span([class("modal-addr-transport")], [text(pair.0)]),
                      span([class("modal-addr-value")], [text(pair.1)]),
                    ])
                  }),
                )
            },
          ]),
        ]),
      ])
    }
  }
}
