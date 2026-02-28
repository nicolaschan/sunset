import gleam/int
import gleam/list
import gleam/option.{None, Some}
import gleam/order
import gleam/string
import lustre
import lustre/effect.{type Effect}
import sunset/libp2p
import sunset/model.{
  type Model, type Msg, type PeerPresence, AudioByeReceived, AudioFailed,
  AudioPcStateChanged, AudioStarted, ChatMessage, ChatMessageReceived,
  DialFailed, DialSucceeded, DiscoveryResponse, HashChanged, Libp2pInitialised,
  Model, PeerConnected, PeerDialFailed, PeerDialSucceeded, PeerDisconnected,
  PeerDiscovered, PeerPresence, PresenceReceived, RelayConnected,
  RelayConnecting, RelayDialFailed, RelayDialSucceeded, RelayDisconnected, Room,
  RouteChanged, ScheduledReconnect, SendFailed, SendSucceeded, Tick,
  UserClickedCancelEditName, UserClickedConnect, UserClickedEditName,
  UserClickedJoinAudio, UserClickedJoinRoom, UserClickedLeaveAudio,
  UserClickedLeaveRoom, UserClickedPeer, UserClickedSaveName, UserClickedSend,
  UserClickedStartAudio, UserClickedStopAudio, UserClosedPeerModal,
  UserToggledNodeInfo, UserUpdatedChatInput, UserUpdatedMultiaddr,
  UserUpdatedNameInput, UserUpdatedRoomInput, client_version, peer_display_name,
}
import sunset/nav
import sunset/router
import sunset/view

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
      audio_sending: False,
      audio_receiving: False,
      audio_joined: False,
      audio_error: "",
      selected_peer: None,
      peer_presence: [],
      audio_pc_states: [],
      disconnected_peers: [],
      display_name: saved_name,
      editing_name: False,
      name_input: "",
      reconnect_attempts: [],
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
        register_audio_signaling_effect(new_model),
        register_presence_handler_effect(),
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
      // Remove from disconnected peers
      let disconnected =
        list.filter(model.disconnected_peers, fn(entry) { entry.0 != peer_id })
      let new_model = Model(..model, disconnected_peers: disconnected)
      // If we're in audio, create a PC for the new peer (unless it's the relay)
      case model.audio_joined, peer_id == model.relay_peer_id {
        True, False -> {
          let should_offer = string.compare(model.peer_id, peer_id) == order.Gt
          #(new_model, create_audio_pc_effect(peer_id, should_offer, model))
        }
        _, _ -> #(new_model, effect.none())
      }
    }

    PeerDisconnected(peer_id) -> {
      // Track as recently disconnected (unless it's the relay)
      case peer_id == model.relay_peer_id {
        True -> #(model, effect.none())
        False -> {
          let entry = #(peer_id, now_ms())
          let disconnected = [entry, ..model.disconnected_peers]
          #(Model(..model, disconnected_peers: disconnected), effect.none())
        }
      }
    }

    RelayDialSucceeded -> {
      // Extract relay peer ID from the known relay multiaddr
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
      let addrs = libp2p.get_multiaddrs()
      let peers = libp2p.get_connected_peers()
      let sending = libp2p.is_microphone_active()
      let receiving = libp2p.is_receiving_audio()
      let raw_connections = libp2p.get_all_connections()
      let connections =
        list.filter_map(raw_connections, fn(pair) {
          case pair {
            [pid, addr] -> Ok(#(pid, addr))
            _ -> Error(Nil)
          }
        })
      let raw_pc_states = libp2p.get_audio_pc_states()
      let audio_pc_states =
        list.filter_map(raw_pc_states, fn(entry) {
          case entry {
            [pid, state] -> Ok(#(pid, state))
            _ -> Error(Nil)
          }
        })
      // Prune expired disconnected peers
      let now = now_ms()
      let grace = int.to_float(model.disconnect_grace_ms)
      let disconnected =
        list.filter(model.disconnected_peers, fn(entry) {
          now -. entry.1 <. grace
        })
      // Prune presence for peers no longer connected and not recently disconnected
      let disconnected_ids = list.map(disconnected, fn(e) { e.0 })
      let peer_presence =
        list.filter(model.peer_presence, fn(entry) {
          list.contains(peers, entry.0)
          || list.contains(disconnected_ids, entry.0)
        })

      // Reconcile audio PCs: ensure we have a PC for every peer in audio
      let reconcile_effect = reconcile_audio_pcs(model, peers)

      // Broadcast our audio presence to all peers
      let broadcast_effect = broadcast_presence_effect(model, peers)

      // Poll discovery if in a room with a relay
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
          audio_sending: sending,
          audio_receiving: receiving,
          peer_presence: peer_presence,
          audio_pc_states: audio_pc_states,
          disconnected_peers: disconnected,
        ),
        effect.batch([
          schedule_tick(),
          reconcile_effect,
          broadcast_effect,
          discovery_effect,
        ]),
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

    UserClickedStartAudio -> {
      #(Model(..model, audio_error: ""), acquire_mic_effect())
    }

    UserClickedStopAudio -> {
      #(
        Model(..model, audio_sending: False, audio_error: ""),
        mute_mic_effect(),
      )
    }

    UserClickedJoinAudio -> {
      let new_model = Model(..model, audio_joined: True, audio_error: "")
      // Acquire mic + unmute remote audio + create PCs for all audio peers
      let peers_to_connect =
        list.filter(model.peers, fn(pid) {
          pid != model.peer_id && pid != model.relay_peer_id
        })
      #(
        new_model,
        effect.batch([
          acquire_mic_effect(),
          unmute_remote_audio_effect(),
          create_audio_pcs_for_peers(new_model, peers_to_connect),
          broadcast_presence_now_effect(new_model),
        ]),
      )
    }

    UserClickedLeaveAudio -> {
      let peers_in_audio =
        list.filter(model.peers, fn(pid) {
          pid != model.peer_id && pid != model.relay_peer_id
        })
      #(
        Model(
          ..model,
          audio_joined: False,
          audio_sending: False,
          reconnect_attempts: [],
        ),
        effect.batch([
          send_byes_and_close_effect(peers_in_audio),
          mute_remote_audio_effect(),
          broadcast_presence_left_effect(model),
        ]),
      )
    }

    AudioStarted -> {
      #(Model(..model, audio_sending: True, audio_error: ""), effect.none())
    }

    AudioFailed(err) -> {
      #(Model(..model, audio_sending: False, audio_error: err), effect.none())
    }

    PeerDiscovered(peer_id, addrs) -> {
      case list.contains(model.peers, peer_id) {
        True -> #(model, effect.none())
        False -> {
          // Sort: prefer direct over circuit relay
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
          #(Model(..model, peer_presence: peer_presence), effect.none())
        }
        Error(_) -> #(model, effect.none())
      }
    }

    AudioPcStateChanged(peer_id, state) -> {
      case state {
        "connected" -> {
          // Success — reset reconnect attempts for this peer
          let attempts =
            list.filter(model.reconnect_attempts, fn(e) { e.0 != peer_id })
          #(Model(..model, reconnect_attempts: attempts), effect.none())
        }
        "disconnected" -> {
          // ICE disconnected — may recover. Schedule reconnect as backup
          // if we're the offerer.
          let should_offer = string.compare(model.peer_id, peer_id) == order.Gt
          case model.audio_joined, should_offer {
            True, True -> #(model, schedule_reconnect_effect(model, peer_id))
            _, _ -> #(model, effect.none())
          }
        }
        "failed" -> {
          // ICE failed — schedule reconnect if we're the offerer
          let should_offer = string.compare(model.peer_id, peer_id) == order.Gt
          case model.audio_joined, should_offer {
            True, True -> #(model, schedule_reconnect_effect(model, peer_id))
            _, _ -> #(model, effect.none())
          }
        }
        _ -> #(model, effect.none())
      }
    }

    AudioByeReceived(peer_id) -> {
      // Peer sent bye — schedule reconnect if we're still in audio
      case model.audio_joined {
        True -> #(model, schedule_reconnect_effect(model, peer_id))
        False -> #(model, effect.none())
      }
    }

    ScheduledReconnect(peer_id) -> {
      // Check preconditions before reconnecting
      case model.audio_joined {
        False -> #(model, effect.none())
        True -> {
          // Check if peer is still connected
          let peer_connected = list.contains(model.peers, peer_id)
          // Check if peer has joined audio
          let peer_in_audio = case
            list.find(model.peer_presence, fn(e) { e.0 == peer_id })
          {
            Ok(#(_, presence)) -> presence.joined
            Error(_) -> False
          }
          // Check if PC already exists and is healthy
          let has_healthy_pc = case
            list.find(model.audio_pc_states, fn(e) { e.0 == peer_id })
          {
            Ok(#(_, s)) -> s == "connected" || s == "connecting"
            Error(_) -> libp2p.has_audio_pc(peer_id)
          }
          case peer_connected, peer_in_audio, has_healthy_pc {
            True, True, False -> {
              // Create a new PC
              let should_offer =
                string.compare(model.peer_id, peer_id) == order.Gt
              #(model, create_audio_pc_effect(peer_id, should_offer, model))
            }
            False, _, _ -> {
              // Peer gone — schedule another attempt
              #(model, schedule_reconnect_effect(model, peer_id))
            }
            _, False, _ -> {
              // Peer not in audio — schedule another attempt
              #(model, schedule_reconnect_effect(model, peer_id))
            }
            _, _, True -> {
              // Already have a healthy PC — nothing to do
              #(model, effect.none())
            }
          }
        }
      }
    }

    DiscoveryResponse(response_json) -> {
      case parse_discovery_response(response_json) {
        Ok(peers_list) -> {
          let effects =
            list.filter_map(peers_list, fn(peer) {
              case peer.0 == model.peer_id {
                True -> Error(Nil)
                False -> {
                  case list.contains(model.peers, peer.0) {
                    True -> Error(Nil)
                    False -> {
                      // Sort addrs: prefer direct over circuit
                      let sorted =
                        list.sort(peer.1, fn(a, b) {
                          let a_circuit = string.contains(a, "/p2p-circuit")
                          let b_circuit = string.contains(b, "/p2p-circuit")
                          case a_circuit, b_circuit {
                            True, False -> order.Gt
                            False, True -> order.Lt
                            _, _ -> order.Eq
                          }
                        })
                      Ok(dial_addrs_sequentially(sorted))
                    }
                  }
                }
              }
            })
          #(model, effect.batch(effects))
        }
        Error(_) -> #(model, effect.none())
      }
    }
  }
}

// -- HELPERS --

/// Extract the peer ID from a multiaddr string like /dns/.../p2p/<peer_id>
fn extract_peer_id_from_multiaddr(addr: String) -> String {
  case string.split(addr, "/p2p/") {
    [_, peer_id] -> peer_id
    _ -> ""
  }
}

/// Parse a presence JSON message.
fn parse_presence(json_str: String) -> Result(PeerPresence, Nil) {
  // Simple JSON parsing using string matching since we don't have a JSON decoder
  // The message format is: {"joined":bool,"muted":bool,"name":"...","version":"..."}
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

/// Extract a string value from a JSON object by key.
/// Simple implementation for our known message format.
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

/// Build the presence JSON message to broadcast.
fn build_presence_json(model: Model) -> String {
  let joined = case model.audio_joined {
    True -> "true"
    False -> "false"
  }
  let muted = case model.audio_joined && !libp2p.is_microphone_active() {
    True -> "true"
    False -> "false"
  }
  "{\"joined\":"
  <> joined
  <> ",\"muted\":"
  <> muted
  <> ",\"name\":\""
  <> escape_json_string(model.display_name)
  <> "\",\"version\":\""
  <> escape_json_string(client_version)
  <> "\"}"
}

/// Escape a string for use in JSON.
fn escape_json_string(s: String) -> String {
  s
  |> string.replace("\\", "\\\\")
  |> string.replace("\"", "\\\"")
  |> string.replace("\n", "\\n")
  |> string.replace("\r", "\\r")
  |> string.replace("\t", "\\t")
}

/// Parse a discovery response JSON to get list of #(peer_id, addrs).
fn parse_discovery_response(
  json_str: String,
) -> Result(List(#(String, List(String))), Nil) {
  // The response format is: {"peers":[{"peer_id":"...","addrs":["...","..."]},...]}
  // We need to parse this manually since we don't have a JSON decoder.
  // For now, use a simple approach: split on peer objects.
  case string.contains(json_str, "\"peers\"") {
    False -> Error(Nil)
    True -> {
      // Extract the peers array content
      case string.split(json_str, "\"peers\":[") {
        [_, rest] -> {
          case string.split(rest, "]") {
            [peers_str, ..] -> {
              let peers = parse_peer_objects(peers_str)
              Ok(peers)
            }
            _ -> Ok([])
          }
        }
        _ -> Ok([])
      }
    }
  }
}

/// Parse individual peer objects from the peers array string.
fn parse_peer_objects(s: String) -> List(#(String, List(String))) {
  // Split on },{ to get individual peer objects
  case string.trim(s) {
    "" -> []
    trimmed -> {
      // Split by peer_id to find each peer
      let parts = string.split(trimmed, "{\"peer_id\":\"")
      list.filter_map(parts, fn(part) {
        case string.trim(part) {
          "" -> Error(Nil)
          p -> {
            case string.split(p, "\"") {
              [peer_id, ..rest] -> {
                let rest_str = string.join(rest, "\"")
                let addrs = parse_addrs_from_peer(rest_str)
                Ok(#(peer_id, addrs))
              }
              _ -> Error(Nil)
            }
          }
        }
      })
    }
  }
}

/// Parse the addrs array from a peer object fragment.
fn parse_addrs_from_peer(s: String) -> List(String) {
  case string.split(s, "\"addrs\":[") {
    [_, rest] -> {
      case string.split(rest, "]") {
        [addrs_str, ..] -> {
          // Parse comma-separated quoted strings
          let parts = string.split(addrs_str, "\"")
          list.filter(parts, fn(p) {
            let trimmed = string.trim(p)
            trimmed != "" && trimmed != "," && trimmed != ",,"
          })
        }
        _ -> []
      }
    }
    _ -> []
  }
}

/// Get the reconnect attempt count for a peer.
fn get_reconnect_count(model: Model, peer_id: String) -> Int {
  case list.find(model.reconnect_attempts, fn(e) { e.0 == peer_id }) {
    Ok(#(_, count)) -> count
    Error(_) -> 0
  }
}

// -- EFFECTS --

fn init_libp2p_effect() -> Effect(Msg) {
  effect.from(fn(dispatch) {
    libp2p.init_libp2p(
      fn(peer_id) { dispatch(Libp2pInitialised(peer_id)) },
      fn(peer_id) { dispatch(PeerConnected(peer_id)) },
      fn(peer_id) { dispatch(PeerDisconnected(peer_id)) },
    )
  })
}

fn init_hash_listener() -> Effect(Msg) {
  effect.from(fn(dispatch) {
    nav.on_hash_change(fn(hash) { dispatch(HashChanged(hash)) })
  })
}

fn set_hash_effect(room: String) -> Effect(Msg) {
  effect.from(fn(_dispatch) { nav.set_hash(room) })
}

fn clear_hash_effect() -> Effect(Msg) {
  effect.from(fn(_dispatch) { nav.clear_hash() })
}

fn dial_relay_effect() -> Effect(Msg) {
  effect.from(fn(dispatch) {
    libp2p.dial_multiaddr(
      relay_addr(),
      fn() { dispatch(RelayDialSucceeded) },
      fn(err) { dispatch(RelayDialFailed(err)) },
    )
  })
}

fn dial_effect(addr: String) -> Effect(Msg) {
  effect.from(fn(dispatch) {
    libp2p.dial_multiaddr(addr, fn() { dispatch(DialSucceeded) }, fn(err) {
      dispatch(DialFailed(err))
    })
  })
}

fn start_polling() -> Effect(Msg) {
  schedule_tick()
}

fn schedule_tick() -> Effect(Msg) {
  effect.from(fn(dispatch) { libp2p.set_timeout(fn() { dispatch(Tick) }, 1000) })
}

fn register_chat_effect() -> Effect(Msg) {
  effect.from(fn(dispatch) {
    libp2p.register_protocol_handler(model.chat_protocol, fn(sender, body) {
      dispatch(ChatMessageReceived(sender, body))
    })
  })
}

fn register_presence_handler_effect() -> Effect(Msg) {
  effect.from(fn(dispatch) {
    libp2p.register_protocol_handler(
      model.audio_presence_protocol,
      fn(sender, message) { dispatch(PresenceReceived(sender, message)) },
    )
  })
}

fn register_audio_signaling_effect(model: Model) -> Effect(Msg) {
  let audio_muted = !model.audio_joined
  effect.from(fn(dispatch) {
    libp2p.register_audio_signaling_handler(
      audio_muted,
      fn(peer_id, state) { dispatch(AudioPcStateChanged(peer_id, state)) },
      fn(peer_id) { dispatch(AudioByeReceived(peer_id)) },
    )
  })
}

/// Broadcast presence to all connected non-relay peers.
fn broadcast_presence_effect(model: Model, peers: List(String)) -> Effect(Msg) {
  let message = build_presence_json(model)
  let targets =
    list.filter(peers, fn(pid) {
      pid != model.peer_id && pid != model.relay_peer_id
    })
  effect.from(fn(_dispatch) {
    list.each(targets, fn(pid) {
      libp2p.send_protocol_message_fire(
        pid,
        model.audio_presence_protocol,
        message,
      )
    })
  })
}

/// Broadcast presence immediately (not waiting for tick).
fn broadcast_presence_now_effect(model: Model) -> Effect(Msg) {
  let peers = libp2p.get_connected_peers()
  broadcast_presence_effect(model, peers)
}

/// Broadcast a "left audio" presence message before we clear the state.
fn broadcast_presence_left_effect(model: Model) -> Effect(Msg) {
  let message =
    "{\"joined\":false,\"muted\":false,\"name\":\""
    <> escape_json_string(model.display_name)
    <> "\",\"version\":\""
    <> escape_json_string(client_version)
    <> "\"}"
  let peers = libp2p.get_connected_peers()
  let targets =
    list.filter(peers, fn(pid) {
      pid != model.peer_id && pid != model.relay_peer_id
    })
  effect.from(fn(_dispatch) {
    list.each(targets, fn(pid) {
      libp2p.send_protocol_message_fire(
        pid,
        model.audio_presence_protocol,
        message,
      )
    })
  })
}

/// Broadcast a chat message to specific peers.
fn broadcast_chat_effect(text: String, peers: List(String)) -> Effect(Msg) {
  case peers {
    [] ->
      effect.from(fn(dispatch) { dispatch(SendFailed("No peers connected")) })
    _ ->
      effect.from(fn(dispatch) {
        list.each(peers, fn(pid) {
          libp2p.send_protocol_message(
            pid,
            model.chat_protocol,
            text,
            fn() { Nil },
            fn(_err) { Nil },
          )
        })
        dispatch(SendSucceeded)
      })
  }
}

fn acquire_mic_effect() -> Effect(Msg) {
  effect.from(fn(dispatch) {
    libp2p.acquire_microphone(fn() { dispatch(AudioStarted) }, fn(err) {
      dispatch(AudioFailed(err))
    })
  })
}

fn mute_mic_effect() -> Effect(Msg) {
  effect.from(fn(_dispatch) { libp2p.mute_microphone() })
}

fn unmute_remote_audio_effect() -> Effect(Msg) {
  effect.from(fn(_dispatch) { libp2p.unmute_remote_audio() })
}

fn mute_remote_audio_effect() -> Effect(Msg) {
  effect.from(fn(_dispatch) { libp2p.mute_remote_audio() })
}

/// Create an audio PC for a single peer.
fn create_audio_pc_effect(
  peer_id: String,
  should_offer: Bool,
  model: Model,
) -> Effect(Msg) {
  let audio_muted = !model.audio_joined
  effect.from(fn(dispatch) {
    libp2p.create_audio_pc(peer_id, should_offer, audio_muted, fn(pid, state) {
      dispatch(AudioPcStateChanged(pid, state))
    })
  })
}

/// Create audio PCs for multiple peers.
fn create_audio_pcs_for_peers(model: Model, peers: List(String)) -> Effect(Msg) {
  let effects =
    list.map(peers, fn(pid) {
      let should_offer = string.compare(model.peer_id, pid) == order.Gt
      create_audio_pc_effect(pid, should_offer, model)
    })
  effect.batch(effects)
}

/// Send bye to all peers and close all PCs + release mic.
fn send_byes_and_close_effect(peers: List(String)) -> Effect(Msg) {
  effect.from(fn(_dispatch) {
    list.each(peers, fn(pid) { libp2p.send_audio_bye(pid) })
    libp2p.close_all_audio_pcs()
  })
}

/// Reconcile audio PCs: ensure we have a PC for every connected peer
/// that has joined audio. Called on each tick.
fn reconcile_audio_pcs(model: Model, peers: List(String)) -> Effect(Msg) {
  case model.audio_joined {
    False -> effect.none()
    True -> {
      let effects =
        list.filter_map(peers, fn(pid) {
          case pid == model.peer_id || pid == model.relay_peer_id {
            True -> Error(Nil)
            False -> {
              // Check if peer has joined audio
              let peer_in_audio = case
                list.find(model.peer_presence, fn(e) { e.0 == pid })
              {
                Ok(#(_, presence)) -> presence.joined
                Error(_) -> False
              }
              case peer_in_audio {
                False -> Error(Nil)
                True -> {
                  // Check if we already have a healthy PC
                  let has_pc = case
                    list.find(model.audio_pc_states, fn(e) { e.0 == pid })
                  {
                    Ok(#(_, s)) -> s != "failed" && s != "closed"
                    Error(_) -> libp2p.has_audio_pc(pid)
                  }
                  case has_pc {
                    True -> Error(Nil)
                    False -> {
                      let should_offer =
                        string.compare(model.peer_id, pid) == order.Gt
                      Ok(create_audio_pc_effect(pid, should_offer, model))
                    }
                  }
                }
              }
            }
          }
        })
      effect.batch(effects)
    }
  }
}

/// Schedule a reconnect attempt for a peer with exponential backoff.
fn schedule_reconnect_effect(model: Model, peer_id: String) -> Effect(Msg) {
  let attempt = get_reconnect_count(model, peer_id) + 1
  let base = model.reconnect_base_delay_ms
  let max_delay = model.reconnect_max_delay_ms
  let delay = int.min(base * pow2(attempt - 1), max_delay)
  effect.from(fn(dispatch) {
    libp2p.set_timeout(fn() { dispatch(ScheduledReconnect(peer_id)) }, delay)
  })
}

/// Simple power of 2.
fn pow2(n: Int) -> Int {
  case n <= 0 {
    True -> 1
    False -> 2 * pow2(n - 1)
  }
}

/// Poll discovery: ask the relay for peers in our room.
fn poll_discovery_effect(relay_peer_id: String, room: String) -> Effect(Msg) {
  effect.from(fn(dispatch) {
    libp2p.poll_discovery(relay_peer_id, room, fn(response) {
      dispatch(DiscoveryResponse(response))
    })
  })
}

/// Dial a list of addresses sequentially (try first, then next on failure).
fn dial_addrs_sequentially(addrs: List(String)) -> Effect(Msg) {
  case addrs {
    [] ->
      effect.from(fn(dispatch) {
        dispatch(PeerDialFailed("No addresses to dial"))
      })
    [first, ..rest] ->
      effect.from(fn(dispatch) {
        libp2p.dial_multiaddr(
          first,
          fn() { dispatch(PeerDialSucceeded) },
          fn(_err) {
            // Try next address
            case rest {
              [] -> dispatch(PeerDialFailed("All addresses failed"))
              _ -> {
                // We can't easily chain effects here, so just try the next one
                // directly via the FFI
                dial_remaining(rest, dispatch)
              }
            }
          },
        )
      })
  }
}

/// Recursively try remaining addresses.
fn dial_remaining(addrs: List(String), dispatch: fn(Msg) -> Nil) -> Nil {
  case addrs {
    [] -> dispatch(PeerDialFailed("All addresses failed"))
    [first, ..rest] ->
      libp2p.dial_multiaddr(
        first,
        fn() { dispatch(PeerDialSucceeded) },
        fn(_err) { dial_remaining(rest, dispatch) },
      )
  }
}

// -- Time --

@external(javascript, "./time.ffi.mjs", "now_ms")
fn now_ms() -> Float {
  0.0
}
