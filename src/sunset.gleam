import gleam/int
import gleam/list
import gleam/option.{None, Some}
import gleam/order
import gleam/string
import lustre
import lustre/effect.{type Effect}
import sunset/libp2p/dial
import sunset/libp2p/discovery
import sunset/libp2p/node
import sunset/libp2p/protocol
import sunset/libp2p/query
import sunset/libp2p/timer
import sunset/model.{
  type Model, type Msg, type PeerPresence, AudioConnected, AudioFailed,
  AudioPcStateChanged, ChatMessage, ChatMessageReceived, DialFailed,
  DialSucceeded, HashChanged, Libp2pInitialised, Model, PeerConnected,
  PeerDialFailed, PeerDialSucceeded, PeerDisconnected, PeerDiscovered,
  PeerPresence, PresenceReceived, RelayConnected, RelayConnecting,
  RelayDialFailed, RelayDialSucceeded, RelayDisconnected, Room, RouteChanged,
  ScheduledReconnect, SendFailed, SendSucceeded, Tick, UserClickedCancelEditName,
  UserClickedConnect, UserClickedEditName, UserClickedJoinAudio,
  UserClickedJoinRoom, UserClickedLeaveAudio, UserClickedLeaveRoom,
  UserClickedPeer, UserClickedSaveName, UserClickedSend, UserClosedPeerModal,
  UserToggledNodeInfo, UserUpdatedChatInput, UserUpdatedMultiaddr,
  UserUpdatedNameInput, UserUpdatedRoomInput, client_version, peer_display_name,
}
import sunset/nav
import sunset/router
import sunset/view
import sunset/webrtc/call
import sunset/webrtc/debug
import sunset/webrtc/dispatch

const default_relay = "/dns/relay.sunset.chat/tcp/443/wss/p2p/12D3KooWAvzBJHKbkWkn3qVH7DdhyJCNFLxQFUrpUFWYueVKzrNY"

fn relay_addr() -> String {
  case nav.get_query_param("relay") {
    "" -> default_relay
    addr -> addr
  }
}

pub fn main() {
  let app = lustre.application(init, update, view.view)
  let assert Ok(_) = lustre.start(app, "#app", Nil)

  Nil
}

// -- MODEL --

fn init(_flags) -> #(Model, Effect(Msg)) {
  let route = router.init_route()
  let saved_name = nav.get_saved_display_name()
  let room = case route {
    Room(name) -> name
    _ -> ""
  }

  let model =
    Model(
      route: route,
      room_input: "",
      room_name: room,
      peer_id: "",
      status: "Initialising...",
      relay_status: RelayDisconnected,
      relay_peer_id: "",
      show_node_info: False,
      multiaddr_input: "",
      addresses: [],
      peers: [],
      connections: [],
      error: "",
      chat_input: "",
      messages: [],
      audio_joined: False,
      audio_error: "",
      audio_connections: [],
      selected_peer: None,
      peer_presence: [],
      audio_pc_states: [],
      disconnected_peers: [],
      display_name: saved_name,
      editing_name: False,
      name_input: "",
      reconnect_attempts: [],
      audio_connecting: [],
    )

  #(
    model,
    effect.batch([init_libp2p_effect(), router.init(), init_hash_listener()]),
  )
}

// -- UPDATE --

fn update(model: Model, msg: Msg) -> #(Model, Effect(Msg)) {
  case msg {
    RouteChanged(route) -> {
      #(Model(..model, route: route), effect.none())
    }

    HashChanged(hash) -> {
      case hash {
        "" -> #(Model(..model, route: model.Home, room_name: ""), effect.none())
        room -> {
          let new_model = Model(..model, route: Room(room), room_name: room)
          case model.peer_id, model.relay_status {
            "", _ -> #(new_model, effect.none())
            _, RelayDisconnected -> #(
              Model(..new_model, relay_status: RelayConnecting),
              dial_relay_effect(),
            )
            _, RelayConnected -> #(new_model, effect.none())
            _, _ -> #(new_model, effect.none())
          }
        }
      }
    }

    UserUpdatedRoomInput(val) -> {
      #(Model(..model, room_input: val), effect.none())
    }

    UserClickedJoinRoom -> {
      let room = string.trim(model.room_input)
      case room {
        "" -> #(model, effect.none())
        _ -> {
          let new_model = Model(..model, route: Room(room), room_name: room)
          case model.peer_id, model.relay_status {
            "", _ -> #(new_model, set_hash_effect(room))
            _, RelayDisconnected -> #(
              Model(..new_model, relay_status: RelayConnecting),
              effect.batch([set_hash_effect(room), dial_relay_effect()]),
            )
            _, RelayConnected -> #(new_model, set_hash_effect(room))
            _, _ -> #(new_model, set_hash_effect(room))
          }
        }
      }
    }

    UserClickedLeaveRoom -> {
      #(
        Model(..model, route: model.Home, room_name: "", room_input: ""),
        clear_hash_effect(),
      )
    }

    UserToggledNodeInfo -> {
      #(Model(..model, show_node_info: !model.show_node_info), effect.none())
    }

    Libp2pInitialised(peer_id) -> {
      let new_model =
        Model(..model, peer_id: peer_id, status: "Online", error: "")
      let effects = [
        start_polling(),
        register_chat_effect(),
        register_presence_handler_effect(),
        register_signaling_handler_effect(),
      ]
      case new_model.room_name {
        "" -> #(new_model, effect.batch(effects))
        _ -> #(
          Model(..new_model, relay_status: RelayConnecting),
          effect.batch([dial_relay_effect(), ..effects]),
        )
      }
    }

    PeerConnected(peer_id) -> {
      let disconnected =
        list.filter(model.disconnected_peers, fn(entry) { entry.0 != peer_id })
      #(Model(..model, disconnected_peers: disconnected), effect.none())
    }

    PeerDisconnected(peer_id) -> {
      case peer_id == model.relay_peer_id {
        True -> #(model, effect.none())
        False -> {
          case list.key_find(model.audio_connections, peer_id) {
            Ok(conn) -> call.hangup(conn)
            Error(_) -> Nil
          }
          dispatch.remove_handler(peer_id)
          let audio_connections =
            list.filter(model.audio_connections, fn(e) { e.0 != peer_id })
          let audio_pc_states =
            list.filter(model.audio_pc_states, fn(e) { e.0 != peer_id })
          let reconnect_attempts =
            list.filter(model.reconnect_attempts, fn(e) { e.0 != peer_id })
          let audio_connecting =
            list.filter(model.audio_connecting, fn(id) { id != peer_id })
          let entry = #(peer_id, now_ms())
          let disconnected = [entry, ..model.disconnected_peers]
          #(
            Model(
              ..model,
              disconnected_peers: disconnected,
              audio_connections: audio_connections,
              audio_pc_states: audio_pc_states,
              reconnect_attempts: reconnect_attempts,
              audio_connecting: audio_connecting,
            ),
            effect.none(),
          )
        }
      }
    }

    RelayDialSucceeded -> {
      let relay_peer_id = extract_peer_id_from_multiaddr(relay_addr())
      #(
        Model(
          ..model,
          relay_status: RelayConnected,
          relay_peer_id: relay_peer_id,
          error: "",
        ),
        effect.none(),
      )
    }

    RelayDialFailed(err) -> {
      #(Model(..model, relay_status: model.RelayFailed(err)), effect.none())
    }

    UserUpdatedMultiaddr(val) -> {
      #(Model(..model, multiaddr_input: val), effect.none())
    }

    UserClickedConnect -> {
      case model.multiaddr_input {
        "" -> #(
          Model(..model, error: "Please enter a multiaddr"),
          effect.none(),
        )
        addr -> #(Model(..model, error: ""), dial_effect(addr))
      }
    }

    DialSucceeded -> {
      #(Model(..model, multiaddr_input: "", error: ""), effect.none())
    }

    DialFailed(err) -> {
      #(Model(..model, error: "Dial failed: " <> err), effect.none())
    }

    Tick -> {
      let addrs = query.get_multiaddrs()
      let peers = query.get_connected_peers()
      let raw_connections = query.get_all_connections()
      let connections =
        list.filter_map(raw_connections, fn(pair) {
          case pair {
            [pid, addr] -> Ok(#(pid, addr))
            _ -> Error(Nil)
          }
        })
      let now = now_ms()
      let grace = int.to_float(model.disconnect_grace_ms)
      let disconnected =
        list.filter(model.disconnected_peers, fn(entry) {
          now -. entry.1 <. grace
        })
      let disconnected_ids = list.map(disconnected, fn(e) { e.0 })
      let peer_presence =
        list.filter(model.peer_presence, fn(entry) {
          list.contains(peers, entry.0)
          || list.contains(disconnected_ids, entry.0)
        })

      let broadcast_effect = broadcast_presence_effect(model, peers)

      let discovery_effect = case model.room_name, model.relay_peer_id {
        "", _ -> effect.none()
        _, "" -> effect.none()
        room, relay_id -> poll_discovery_effect(relay_id, room)
      }

      #(
        Model(
          ..model,
          addresses: addrs,
          peers: peers,
          connections: connections,
          peer_presence: peer_presence,
          disconnected_peers: disconnected,
        ),
        effect.batch([schedule_tick(), broadcast_effect, discovery_effect]),
      )
    }

    UserUpdatedChatInput(val) -> {
      #(Model(..model, chat_input: val), effect.none())
    }

    UserClickedSend -> {
      case model.chat_input {
        "" -> #(model, effect.none())
        msg_text -> {
          let message = ChatMessage(sender: "You", body: msg_text)
          let peers_to_send =
            list.filter(model.peers, fn(pid) {
              pid != model.peer_id && pid != model.relay_peer_id
            })
          #(
            Model(..model, chat_input: "", messages: [message, ..model.messages]),
            broadcast_chat_effect(msg_text, peers_to_send),
          )
        }
      }
    }

    SendSucceeded -> #(model, effect.none())

    SendFailed(err) -> {
      let message = ChatMessage(sender: "System", body: "Send failed: " <> err)
      #(Model(..model, messages: [message, ..model.messages]), effect.none())
    }

    ChatMessageReceived(sender, body) -> {
      let display_sender = peer_display_name(model, sender)
      let message = ChatMessage(sender: display_sender, body: body)
      #(Model(..model, messages: [message, ..model.messages]), effect.none())
    }

    // Audio messages — no-ops for now, will be wired up next
    UserClickedJoinAudio -> {
      debug.log("app", "UserClickedJoinAudio")
      let new_model = Model(..model, audio_joined: True, audio_error: "")
      let joined_peers =
        list.filter_map(model.peer_presence, fn(entry) {
          case { entry.1 }.joined {
            True -> Ok(entry.0)
            False -> Error(Nil)
          }
        })
      debug.log(
        "app",
        "joining audio, peers with joined=true: "
          <> int.to_string(list.length(joined_peers)),
      )
      let new_model = Model(..new_model, audio_connecting: joined_peers)
      let connect_effects =
        list.map(joined_peers, fn(peer_id) { connect_to_peer_effect(peer_id) })
      #(
        new_model,
        effect.batch([
          broadcast_presence_now_effect(new_model),
          ..connect_effects
        ]),
      )
    }

    UserClickedLeaveAudio -> {
      list.each(model.audio_connections, fn(entry) { call.hangup(entry.1) })
      list.each(model.audio_connections, fn(entry) {
        dispatch.remove_handler(entry.0)
      })
      let new_model =
        Model(
          ..model,
          audio_joined: False,
          audio_error: "",
          audio_connections: [],
          audio_pc_states: [],
          reconnect_attempts: [],
          audio_connecting: [],
        )
      #(new_model, broadcast_presence_now_effect(new_model))
    }
    AudioConnected(peer_id, conn) -> {
      debug.log("app", "AudioConnected peer=" <> peer_id)
      let audio_connections =
        list.key_set(model.audio_connections, peer_id, conn)
      let reconnect_attempts =
        list.filter(model.reconnect_attempts, fn(e) { e.0 != peer_id })
      let audio_connecting =
        list.filter(model.audio_connecting, fn(id) { id != peer_id })
      #(
        Model(
          ..model,
          audio_connections: audio_connections,
          audio_error: "",
          reconnect_attempts: reconnect_attempts,
          audio_connecting: audio_connecting,
        ),
        effect.none(),
      )
    }

    AudioFailed(peer_id, err) -> {
      debug.log("app", "AudioFailed peer=" <> peer_id <> " err=" <> err)
      dispatch.remove_handler(peer_id)
      let audio_connections =
        list.filter(model.audio_connections, fn(e) { e.0 != peer_id })
      let audio_connecting =
        list.filter(model.audio_connecting, fn(id) { id != peer_id })
      case model.audio_joined {
        False -> #(
          Model(
            ..model,
            audio_connections: audio_connections,
            audio_connecting: audio_connecting,
            audio_error: err,
          ),
          effect.none(),
        )
        True -> {
          let attempts = get_reconnect_attempts(model, peer_id)
          let reconnect_attempts =
            list.key_set(model.reconnect_attempts, peer_id, attempts + 1)
          let delay = reconnect_delay(attempts)
          #(
            Model(
              ..model,
              audio_connections: audio_connections,
              audio_connecting: audio_connecting,
              audio_error: err,
              reconnect_attempts: reconnect_attempts,
            ),
            schedule_reconnect_effect(peer_id, delay),
          )
        }
      }
    }

    AudioPcStateChanged(peer_id, state) -> {
      debug.log(
        "app",
        "AudioPcStateChanged peer=" <> peer_id <> " state=" <> state,
      )
      let audio_pc_states = list.key_set(model.audio_pc_states, peer_id, state)
      case state {
        "failed" | "closed" -> {
          case list.key_find(model.audio_connections, peer_id) {
            Ok(conn) -> call.hangup(conn)
            Error(_) -> Nil
          }
          dispatch.remove_handler(peer_id)
          let audio_connections =
            list.filter(model.audio_connections, fn(e) { e.0 != peer_id })
          case model.audio_joined {
            False -> #(
              Model(
                ..model,
                audio_connections: audio_connections,
                audio_pc_states: audio_pc_states,
              ),
              effect.none(),
            )
            True -> {
              let attempts = get_reconnect_attempts(model, peer_id)
              let reconnect_attempts =
                list.key_set(model.reconnect_attempts, peer_id, attempts + 1)
              let delay = reconnect_delay(attempts)
              #(
                Model(
                  ..model,
                  audio_connections: audio_connections,
                  audio_pc_states: audio_pc_states,
                  reconnect_attempts: reconnect_attempts,
                ),
                schedule_reconnect_effect(peer_id, delay),
              )
            }
          }
        }
        _ -> #(Model(..model, audio_pc_states: audio_pc_states), effect.none())
      }
    }

    ScheduledReconnect(peer_id) -> {
      case model.audio_joined {
        False -> #(model, effect.none())
        True ->
          case list.key_find(model.audio_connections, peer_id) {
            Ok(_) -> #(model, effect.none())
            Error(_) ->
              case list.contains(model.audio_connecting, peer_id) {
                True -> #(model, effect.none())
                False ->
                  case list.contains(model.peers, peer_id) {
                    False -> #(model, effect.none())
                    True -> #(
                      Model(..model, audio_connecting: [
                        peer_id,
                        ..model.audio_connecting
                      ]),
                      connect_to_peer_effect(peer_id),
                    )
                  }
              }
          }
      }
    }

    PeerDiscovered(peer_id, addrs) -> {
      case list.contains(model.peers, peer_id) {
        True -> #(model, effect.none())
        False -> {
          let sorted =
            list.sort(addrs, fn(a, b) {
              let a_circuit = string.contains(a, "/p2p-circuit")
              let b_circuit = string.contains(b, "/p2p-circuit")
              case a_circuit, b_circuit {
                True, False -> order.Gt
                False, True -> order.Lt
                _, _ -> order.Eq
              }
            })
          #(model, dial_addrs_sequentially(sorted))
        }
      }
    }

    PeerDialSucceeded -> {
      #(model, effect.none())
    }

    PeerDialFailed(_err) -> {
      #(model, effect.none())
    }

    UserClickedPeer(peer_id) -> {
      #(Model(..model, selected_peer: Some(peer_id)), effect.none())
    }

    UserClosedPeerModal -> {
      #(Model(..model, selected_peer: None), effect.none())
    }

    UserClickedEditName -> {
      #(
        Model(..model, editing_name: True, name_input: model.display_name),
        effect.none(),
      )
    }

    UserUpdatedNameInput(val) -> {
      #(Model(..model, name_input: val), effect.none())
    }

    UserClickedSaveName -> {
      let name = string.trim(model.name_input)
      nav.save_display_name(name)
      let new_model =
        Model(..model, display_name: name, editing_name: False, name_input: "")
      #(new_model, broadcast_presence_now_effect(new_model))
    }

    UserClickedCancelEditName -> {
      #(Model(..model, editing_name: False, name_input: ""), effect.none())
    }

    PresenceReceived(peer_id, message) -> {
      case parse_presence(message) {
        Ok(presence) -> {
          let peer_presence =
            list.key_set(model.peer_presence, peer_id, presence)
          let has_connection =
            list.any(model.audio_connections, fn(e) { e.0 == peer_id })
          case !presence.joined && has_connection {
            True -> {
              debug.log(
                "app",
                "PresenceReceived: peer left audio, tearing down peer="
                  <> peer_id,
              )
              case list.key_find(model.audio_connections, peer_id) {
                Ok(conn) -> call.hangup(conn)
                Error(_) -> Nil
              }
              dispatch.remove_handler(peer_id)
              #(
                Model(
                  ..model,
                  peer_presence: peer_presence,
                  audio_connections: list.filter(model.audio_connections, fn(e) {
                    e.0 != peer_id
                  }),
                  audio_pc_states: list.filter(model.audio_pc_states, fn(e) {
                    e.0 != peer_id
                  }),
                  reconnect_attempts: list.filter(
                    model.reconnect_attempts,
                    fn(e) { e.0 != peer_id },
                  ),
                  audio_connecting: list.filter(model.audio_connecting, fn(p) {
                    p != peer_id
                  }),
                ),
                effect.none(),
              )
            }
            False -> {
              let should_connect =
                model.audio_joined
                && presence.joined
                && !has_connection
                && !list.contains(model.audio_connecting, peer_id)
              case should_connect {
                True ->
                  debug.log(
                    "app",
                    "PresenceReceived: auto-connecting to peer=" <> peer_id,
                  )
                False -> Nil
              }
              let connect_effect = case should_connect {
                True -> connect_to_peer_effect(peer_id)
                False -> effect.none()
              }
              let audio_connecting = case should_connect {
                True -> [peer_id, ..model.audio_connecting]
                False -> model.audio_connecting
              }
              #(
                Model(
                  ..model,
                  peer_presence: peer_presence,
                  audio_connecting: audio_connecting,
                ),
                connect_effect,
              )
            }
          }
        }
        Error(_) -> #(model, effect.none())
      }
    }
  }
}

// -- HELPERS --

fn extract_peer_id_from_multiaddr(addr: String) -> String {
  case string.split(addr, "/p2p/") {
    [_, peer_id] -> peer_id
    _ -> ""
  }
}

fn get_reconnect_attempts(model: Model, peer_id: String) -> Int {
  case list.key_find(model.reconnect_attempts, peer_id) {
    Ok(n) -> n
    Error(_) -> 0
  }
}

fn reconnect_delay(attempts: Int) -> Int {
  let delay = model.reconnect_base_delay_ms * pow2(attempts)
  case delay > model.reconnect_max_delay_ms {
    True -> model.reconnect_max_delay_ms
    False -> delay
  }
}

fn pow2(n: Int) -> Int {
  case n <= 0 {
    True -> 1
    False -> 2 * pow2(n - 1)
  }
}

fn parse_presence(json_str: String) -> Result(PeerPresence, Nil) {
  case
    string.contains(json_str, "\"joined\"")
    && string.contains(json_str, "\"muted\"")
  {
    False -> Error(Nil)
    True -> {
      let joined = string.contains(json_str, "\"joined\":true")
      let muted = string.contains(json_str, "\"muted\":true")
      let name = extract_json_string(json_str, "name")
      let version = extract_json_string(json_str, "version")
      Ok(PeerPresence(
        joined: joined,
        muted: muted,
        name: name,
        version: version,
      ))
    }
  }
}

fn extract_json_string(json_str: String, key: String) -> String {
  let search = "\"" <> key <> "\":\""
  case string.split(json_str, search) {
    [_, rest] ->
      case string.split(rest, "\"") {
        [value, ..] -> value
        _ -> ""
      }
    _ -> ""
  }
}

fn build_presence_json(model: Model) -> String {
  let joined = case model.audio_joined {
    True -> "true"
    False -> "false"
  }
  "{\"joined\":"
  <> joined
  <> ",\"muted\":false"
  <> ",\"name\":\""
  <> escape_json_string(model.display_name)
  <> "\",\"version\":\""
  <> escape_json_string(client_version)
  <> "\"}"
}

fn escape_json_string(s: String) -> String {
  s
  |> string.replace("\\", "\\\\")
  |> string.replace("\"", "\\\"")
  |> string.replace("\n", "\\n")
  |> string.replace("\r", "\\r")
  |> string.replace("\t", "\\t")
}

// -- EFFECTS --

fn init_libp2p_effect() -> Effect(Msg) {
  use dispatch <- effect.from
  node.init_libp2p(
    fn(peer_id) { dispatch(Libp2pInitialised(peer_id)) },
    fn(peer_id) { dispatch(PeerConnected(peer_id)) },
    fn(peer_id) { dispatch(PeerDisconnected(peer_id)) },
  )
}

fn init_hash_listener() -> Effect(Msg) {
  use dispatch <- effect.from
  nav.on_hash_change(fn(hash) { dispatch(HashChanged(hash)) })
}

fn set_hash_effect(room: String) -> Effect(Msg) {
  use _dispatch <- effect.from
  nav.set_hash(room)
}

fn clear_hash_effect() -> Effect(Msg) {
  use _dispatch <- effect.from
  nav.clear_hash()
}

fn dial_relay_effect() -> Effect(Msg) {
  use dispatch <- effect.from
  use result <- dial.dial_multiaddr(relay_addr())
  case result {
    Ok(_) -> dispatch(RelayDialSucceeded)
    Error(err) -> dispatch(RelayDialFailed(err))
  }
}

fn dial_effect(addr: String) -> Effect(Msg) {
  use dispatch <- effect.from
  use result <- dial.dial_multiaddr(addr)
  case result {
    Ok(_) -> dispatch(DialSucceeded)
    Error(err) -> dispatch(DialFailed(err))
  }
}

fn start_polling() -> Effect(Msg) {
  schedule_tick()
}

fn schedule_tick() -> Effect(Msg) {
  use dispatch <- effect.from
  timer.set_timeout(fn() { dispatch(Tick) }, 1000)
}

fn register_chat_effect() -> Effect(Msg) {
  use dispatch <- effect.from
  protocol.register_protocol_handler(model.chat_protocol, fn(sender, body) {
    dispatch(ChatMessageReceived(sender, body))
  })
}

fn register_presence_handler_effect() -> Effect(Msg) {
  use dispatch <- effect.from
  protocol.register_protocol_handler(
    model.audio_presence_protocol,
    fn(sender, message) { dispatch(PresenceReceived(sender, message)) },
  )
}

fn register_signaling_handler_effect() -> Effect(Msg) {
  use _dispatch <- effect.from
  protocol.register_protocol_handler(
    model.audio_signaling_protocol,
    fn(sender, body) {
      debug.log("app", "signaling received from=" <> sender)
      dispatch.dispatch(sender, body)
    },
  )
}

fn connect_to_peer_effect(peer_id: String) -> Effect(Msg) {
  debug.log("app", "connect_to_peer_effect peer=" <> peer_id)
  let channel =
    call.Channel(
      send: fn(msg, cb) {
        protocol.send_protocol_message(
          peer_id,
          model.audio_signaling_protocol,
          msg,
          cb,
        )
      },
      on_receive: fn(handler) { dispatch.set_handler(peer_id, handler) },
    )
  use dispatch <- effect.from
  call.connect(
    channel,
    timer.set_timeout,
    fn(state) { dispatch(AudioPcStateChanged(peer_id, state)) },
    fn(result) {
      case result {
        Ok(conn) -> dispatch(AudioConnected(peer_id, conn))
        Error(err) -> dispatch(AudioFailed(peer_id, err))
      }
    },
  )
}

fn schedule_reconnect_effect(peer_id: String, delay_ms: Int) -> Effect(Msg) {
  use dispatch <- effect.from
  timer.set_timeout(fn() { dispatch(ScheduledReconnect(peer_id)) }, delay_ms)
}

fn broadcast_presence_effect(model: Model, peers: List(String)) -> Effect(Msg) {
  let message = build_presence_json(model)
  let targets =
    list.filter(peers, fn(pid) {
      pid != model.peer_id && pid != model.relay_peer_id
    })
  use _dispatch <- effect.from
  list.each(targets, fn(pid) {
    protocol.send_protocol_message_fire(
      pid,
      model.audio_presence_protocol,
      message,
    )
  })
}

fn broadcast_presence_now_effect(model: Model) -> Effect(Msg) {
  let peers = query.get_connected_peers()
  broadcast_presence_effect(model, peers)
}

fn broadcast_chat_effect(text: String, peers: List(String)) -> Effect(Msg) {
  case peers {
    [] -> {
      use dispatch <- effect.from
      dispatch(SendFailed("No peers connected"))
    }
    _ -> {
      use dispatch <- effect.from
      list.each(peers, fn(pid) {
        protocol.send_protocol_message(
          pid,
          model.chat_protocol,
          text,
          fn(_result) { Nil },
        )
      })
      dispatch(SendSucceeded)
    }
  }
}

fn poll_discovery_effect(relay_peer_id: String, room: String) -> Effect(Msg) {
  use dispatch <- effect.from
  discovery.poll_discovery(relay_peer_id, room, fn(peer_id, addrs) {
    dispatch(PeerDiscovered(peer_id, addrs))
  })
}

fn dial_addrs_sequentially(addrs: List(String)) -> Effect(Msg) {
  case addrs {
    [] -> {
      use dispatch <- effect.from
      dispatch(PeerDialFailed("No addresses to dial"))
    }
    [first, ..rest] -> {
      use dispatch <- effect.from
      use result <- dial.dial_multiaddr(first)
      case result {
        Ok(_) -> dispatch(PeerDialSucceeded)
        Error(_) ->
          case rest {
            [] -> dispatch(PeerDialFailed("All addresses failed"))
            _ -> dial_remaining(rest, dispatch)
          }
      }
    }
  }
}

fn dial_remaining(addrs: List(String), dispatch: fn(Msg) -> Nil) -> Nil {
  case addrs {
    [] -> dispatch(PeerDialFailed("All addresses failed"))
    [first, ..rest] -> {
      use result <- dial.dial_multiaddr(first)
      case result {
        Ok(_) -> dispatch(PeerDialSucceeded)
        Error(_) -> dial_remaining(rest, dispatch)
      }
    }
  }
}

// -- Time --

@external(javascript, "./time.ffi.mjs", "now_ms")
fn now_ms() -> Float {
  0.0
}
