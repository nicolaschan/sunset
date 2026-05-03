//// sunset.chat — Gleam + Lustre frontend.
////
//// Two top-level views:
////   * `LandingView` — empty state shown at root `/`. The user types a
////     room name and submits; we add it to their joined-rooms list and
////     navigate.
////   * `RoomView(name)` — the existing 4-column chat shell rendering
////     fixture data for the named room.
////
//// Routing is anchor-based: the URL fragment (`/#dusk-collective`) is
//// the source of truth for which room is active. A storage FFI shim
//// persists the joined-rooms list + last-used room to localStorage
//// so a refresh restores the user's state.

import gleam/dict.{type Dict}
import gleam/list
import gleam/option.{type Option, None, Some}
import gleam/result
import gleam/set.{type Set}
import gleam/string
import lustre
import lustre/attribute
import lustre/effect.{type Effect}
import lustre/element.{type Element}
import lustre/element/html
import lustre/event
import sunset_web/domain.{
  type ChannelId, type Message, type Reaction, type Room, ChannelId, NoBridge,
  Reaction, Room, RoomId,
}
import sunset_web/fixture
import sunset_web/scroll_anchor
import sunset_web/storage
import sunset_web/sunset.{
  type ClientHandle, type IncomingMessage, type RoomHandle,
}
import sunset_web/theme.{type Mode, Dark, Light}
import sunset_web/ui
import sunset_web/views/bottom_sheet
import sunset_web/views/channels
import sunset_web/views/details_panel
import sunset_web/views/landing
import sunset_web/views/main_panel
import sunset_web/views/members
import sunset_web/views/peer_status_popover
import sunset_web/views/phone_header
import sunset_web/views/rooms
import sunset_web/views/shell
import sunset_web/views/touch_drag
import sunset_web/views/voice_minibar
import sunset_web/views/voice_popover

/// Relays the client dials at startup when the URL has no
/// `?relay=…` query parameter. Each entry is fed through
/// `sunset-relay-resolver`, so bare hostnames work.
const default_relays: List(String) = ["relay.sunset.chat"]

pub type View {
  LandingView
  RoomView(name: String)
}

/// Per-room UI + engine state. The Model holds a
/// `Dict(String, RoomState)` keyed by the room name (URL fragment).
pub type RoomState {
  RoomState(
    handle: RoomHandle,
    messages: List(domain.Message),
    members: List(domain.Member),
    receipts: Dict(String, Set(String)),
    reactions: Dict(String, List(Reaction)),
    current_channel: ChannelId,
    draft: String,
    selected_msg_id: Option(String),
    reacting_to: Option(String),
    sheet: Option(domain.Sheet),
    peer_status_popover: Option(domain.MemberId),
  )
}

fn empty_room_state(handle: RoomHandle) -> RoomState {
  RoomState(
    handle: handle,
    messages: [],
    members: [],
    receipts: dict.new(),
    reactions: dict.new(),
    current_channel: ChannelId(fixture.initial_channel_id),
    draft: "",
    selected_msg_id: None,
    reacting_to: None,
    sheet: None,
    peer_status_popover: None,
  )
}

pub type Model {
  Model(
    mode: Mode,
    view: View,
    joined_rooms: List(String),
    rooms_collapsed: Bool,
    landing_input: String,
    sidebar_search: String,
    /// Name of the room currently being dragged in the rooms rail.
    /// `None` between drag operations.
    dragging_room: Option(String),
    /// Name of the room currently hovered over while dragging — used
    /// for the visible drop-target indicator.
    drag_over_room: Option(String),
    /// Per-member voice tweaks (volume / denoise / deafened),
    /// keyed by member name. Seeded from the fixture once.
    voice_settings: Dict(String, domain.VoiceSettings),
    /// Engine handle. None until the wasm bundle finishes initialising.
    client: Option(ClientHandle),
    /// Relay connection status: "disconnected", "connecting", "connected", "error".
    relay_status: String,
    /// Viewport class — drives the desktop/phone branch in `shell.view`.
    viewport: domain.Viewport,
    /// Currently open drawer on phone. Ignored on desktop. Channels and
    /// rooms drawers cross-transition (one swaps for the other) rather
    /// than stack.
    drawer: Option(domain.Drawer),
    /// Wall-clock unix-ms snapshot. Updated every second by the
    /// `Tick(now_ms)` message so the popover's age readout stays live.
    now_ms: Int,
    /// Per-room state, keyed by room name. Populated as each RoomOpened
    /// message arrives after bootstrap.
    rooms: Dict(String, RoomState),
  )
}

pub type Msg {
  NoOp
  ToggleMode
  HashChanged(String)
  UpdateLandingInput(String)
  UpdateSidebarSearch(String)
  JoinRoom(String)
  DeleteRoom(String)
  GoToLanding
  DragRoomStart(String)
  DragRoomOver(String)
  DragRoomLeave(String)
  DropRoomOn(String)
  DragRoomEnd
  SelectChannel(ChannelId)
  ToggleRoomsRail
  UpdateDraft(String)
  ToggleMessageSelected(String)
  ToggleReactionPicker(String)
  AddReaction(String, String)
  OpenDetail(String)
  CloseDetail
  OpenVoicePopover(String)
  CloseVoicePopover
  OpenPeerStatusPopover(domain.MemberId)
  ClosePeerStatusPopover
  Tick(Int)
  SetMemberVolume(String, Int)
  ToggleMemberDenoise(String)
  ToggleMemberDeafen(String)
  ResetMemberVoice(String)
  // sunset-web-wasm bridge wiring:
  IdentityReady(BitArray)
  ClientReady(ClientHandle)
  RelayConnectResult(Result(Nil, String))
  /// A room's wasm-side handle is ready; register callbacks + start presence.
  RoomOpened(name: String, handle: RoomHandle)
  IncomingMsg(room: String, im: IncomingMessage)
  IncomingReceipt(room: String, message_id: String, from_pubkey: String)
  SubmitDraft
  MessageSent(Result(String, String))
  MembersUpdated(room: String, members: List(domain.Member))
  RelayStatusUpdated(String)
  ViewportChanged(domain.Viewport)
  OpenDrawer(domain.Drawer)
  CloseDrawer
}

pub fn main() {
  storage.install_mobile_viewport_meta()
  scroll_anchor.attach_chat_scroll_anchor()
  let app = lustre.application(init, update, view)
  let assert Ok(_) = lustre.start(app, "#app", Nil)
  Nil
}

fn init(_flags: Nil) -> #(Model, Effect(Msg)) {
  let stored_rooms = storage.read_joined_rooms()
  let initial_hash = storage.read_hash()
  let initial_mode = case storage.read_saved_theme() {
    "dark" -> Dark
    "light" -> Light
    // No explicit choice yet: follow the OS / browser preference.
    _ ->
      case storage.prefers_dark() {
        True -> Dark
        False -> Light
      }
  }

  // Resolve the initial view + rooms list:
  //   * URL fragment wins if present.
  //   * Otherwise fall back to the first joined room (the user's
  //     stored ordering — head is the default).
  //   * Otherwise show the landing page.
  // Direct-navigation to a room not yet in the joined list auto-adds
  // it at the top, so a shared link behaves like a fresh "join new
  // room" rather than dropping the user back to landing.
  let initial_view = case initial_hash, stored_rooms {
    "", [] -> LandingView
    "", [first, ..] -> RoomView(first)
    name, _ -> RoomView(name)
  }

  let joined = case initial_view {
    LandingView -> stored_rooms
    RoomView(name) -> ensure_joined(stored_rooms, name)
  }

  let initial_viewport = case storage.is_phone_viewport() {
    True -> domain.Phone
    False -> domain.Desktop
  }

  let model =
    Model(
      mode: initial_mode,
      view: initial_view,
      joined_rooms: joined,
      rooms_collapsed: False,
      landing_input: "",
      sidebar_search: "",
      dragging_room: None,
      drag_over_room: None,
      voice_settings: seed_voice_settings(),
      client: None,
      relay_status: "disconnected",
      viewport: initial_viewport,
      drawer: None,
      now_ms: 0,
      rooms: dict.new(),
    )

  let subscribe_hash =
    effect.from(fn(dispatch) {
      storage.on_hash_change(fn(hash) { dispatch(HashChanged(hash)) })
    })

  let initial_persist = case joined == stored_rooms {
    True -> effect.none()
    False ->
      effect.from(fn(_) {
        storage.write_joined_rooms(joined)
        Nil
      })
  }

  let initial_hash_sync = case initial_view, initial_hash {
    RoomView(name), "" ->
      effect.from(fn(_) {
        storage.set_hash(name)
        Nil
      })
    _, _ -> effect.none()
  }

  // Bootstrap the sunset-web-wasm bridge: load identity → create client →
  // (optionally) connect to relay from ?relay=… query param.
  let bootstrap =
    effect.from(fn(dispatch) {
      sunset.load_or_create_identity(fn(seed) { dispatch(IdentityReady(seed)) })
    })

  let subscribe_viewport =
    effect.from(fn(dispatch) {
      storage.on_viewport_change(fn(is_phone) {
        let v = case is_phone {
          True -> domain.Phone
          False -> domain.Desktop
        }
        dispatch(ViewportChanged(v))
      })
    })

  let subscribe_touch_drag =
    effect.from(fn(dispatch) {
      touch_drag.attach(
        touch_drag.Callbacks(
          on_start: fn(name) { dispatch(DragRoomStart(name)) },
          on_over: fn(name) { dispatch(DragRoomOver(name)) },
          on_drop: fn(name) { dispatch(DropRoomOn(name)) },
          on_end: fn() { dispatch(DragRoomEnd) },
        ),
      )
    })

  let ticker_eff =
    effect.from(fn(dispatch) {
      sunset.set_interval_ms(1000, fn() { dispatch(Tick(sunset.now_ms())) })
    })

  #(
    model,
    effect.batch([
      subscribe_hash,
      initial_persist,
      initial_hash_sync,
      bootstrap,
      subscribe_viewport,
      subscribe_touch_drag,
      ticker_eff,
    ]),
  )
}

/// Seed per-member voice tweaks for every in-call member: full
/// volume, denoise on, not locally muted. Keyed by member name so
/// callers don't need to thread MemberId through the popover wiring.
fn seed_voice_settings() -> Dict(String, domain.VoiceSettings) {
  fixture.members()
  |> list.fold(dict.new(), fn(d, m) {
    case m.in_call {
      True ->
        dict.insert(
          d,
          m.name,
          domain.VoiceSettings(volume: 100, denoise: True, deafened: False),
        )
      False -> d
    }
  })
}

fn member_voice_settings(
  settings: Dict(String, domain.VoiceSettings),
  name: String,
) -> domain.VoiceSettings {
  case dict.get(settings, name) {
    Ok(s) -> s
    Error(_) ->
      domain.VoiceSettings(volume: 100, denoise: True, deafened: False)
  }
}

/// Add `name` to `existing` if it isn't already present (prepending
/// at the head, which is where new rooms appear). If `name` is
/// already in the list it is returned unchanged — selecting an
/// existing room must NOT reorder the rail; only an explicit
/// drag-drop reorders the user-managed list.
fn ensure_joined(existing: List(String), name: String) -> List(String) {
  case list.contains(existing, name) {
    True -> existing
    False -> [name, ..existing]
  }
}

/// Move `name` so it lands immediately before `target`. If `target`
/// is the same as `name` (drop on self) the list is returned
/// unchanged. If `name` is already adjacent above `target` the move
/// is also a no-op.
fn reorder_before(
  rooms: List(String),
  name: String,
  target: String,
) -> List(String) {
  case name == target {
    True -> rooms
    False -> {
      let without = list.filter(rooms, fn(r) { r != name })
      list.flatten(
        list.map(without, fn(r) {
          case r == target {
            True -> [name, r]
            False -> [r]
          }
        }),
      )
    }
  }
}

fn sanitize(raw: String) -> String {
  string.trim(raw)
}

@external(javascript, "./sunset_web/sunset.ffi.mjs", "currentTimeMs")
fn current_time_ms() -> Int

@external(javascript, "./sunset_web/sunset.ffi.mjs", "shortPubkey")
fn short_pubkey(bits: BitArray) -> String

@external(javascript, "./sunset_web/sunset.ffi.mjs", "shortInitials")
fn short_initials(bits: BitArray) -> String

@external(javascript, "./sunset_web/sunset.ffi.mjs", "formatTimeMs")
fn format_time_ms(ms: Int) -> String

/// Look up the active room name from the current view.
fn active_room_name(model: Model) -> String {
  case model.view {
    RoomView(n) -> n
    LandingView -> ""
  }
}

/// Apply `f` to the active room's RoomState (if it exists) and re-insert
/// the result into `model.rooms`. If there is no active room (landing view)
/// or the room isn't in the dict yet, returns the model unchanged.
fn with_active_room(
  model: Model,
  f: fn(RoomState) -> #(RoomState, Effect(Msg)),
) -> #(Model, Effect(Msg)) {
  let name = active_room_name(model)
  case name, dict.get(model.rooms, name) {
    "", _ -> #(model, effect.none())
    _, Error(_) -> #(model, effect.none())
    _, Ok(state) -> {
      let #(new_state, eff) = f(state)
      let new_rooms = dict.insert(model.rooms, name, new_state)
      #(Model(..model, rooms: new_rooms), eff)
    }
  }
}

fn update(model: Model, msg: Msg) -> #(Model, Effect(Msg)) {
  case msg {
    NoOp -> #(model, effect.none())
    ToggleMode -> {
      let next_mode = case model.mode {
        Light -> Dark
        Dark -> Light
      }
      let label = case next_mode {
        Light -> "light"
        Dark -> "dark"
      }
      #(
        Model(..model, mode: next_mode),
        effect.from(fn(_) {
          storage.write_saved_theme(label)
          Nil
        }),
      )
    }
    HashChanged(hash) -> {
      let new_view = case hash {
        "" -> LandingView
        name -> RoomView(name)
      }
      let new_rooms = case new_view {
        LandingView -> model.joined_rooms
        RoomView(name) -> ensure_joined(model.joined_rooms, name)
      }
      let persisted = case new_rooms == model.joined_rooms {
        True -> effect.none()
        False ->
          effect.from(fn(_) {
            storage.write_joined_rooms(new_rooms)
            Nil
          })
      }
      // If navigating to a room not yet opened, trigger open_room.
      let new_name = case new_view {
        RoomView(n) -> n
        LandingView -> ""
      }
      let open_eff = case
        new_name,
        dict.has_key(model.rooms, new_name),
        model.client
      {
        "", _, _ -> effect.none()
        _, True, _ -> effect.none()
        _, False, Some(client) ->
          effect.from(fn(dispatch) {
            sunset.open_room(client, new_name, fn(handle) {
              dispatch(RoomOpened(new_name, handle))
            })
          })
        _, False, None -> effect.none()
      }
      #(
        Model(..model, view: new_view, joined_rooms: new_rooms),
        effect.batch([persisted, open_eff]),
      )
    }
    UpdateLandingInput(s) -> #(Model(..model, landing_input: s), effect.none())
    UpdateSidebarSearch(s) -> #(
      Model(..model, sidebar_search: s),
      effect.none(),
    )
    JoinRoom(raw) -> {
      let name = sanitize(raw)
      case name {
        "" -> #(model, effect.none())
        _ -> {
          let was_new = !list.contains(model.joined_rooms, name)
          let new_room_list = ensure_joined(model.joined_rooms, name)
          // On phone, picking a room from the rooms drawer should land
          // the user in the channels drawer for the new room (so they
          // can pick a channel). Otherwise close to chat as before.
          let new_drawer = case model.viewport, model.drawer {
            domain.Phone, Some(domain.RoomsDrawer) ->
              Some(domain.ChannelsDrawer)
            _, _ -> None
          }
          let new_model =
            Model(
              ..model,
              joined_rooms: new_room_list,
              view: RoomView(name),
              landing_input: "",
              sidebar_search: "",
              drawer: new_drawer,
            )
          let persist_eff = case was_new {
            True ->
              effect.from(fn(_) {
                storage.write_joined_rooms(new_room_list)
                Nil
              })
            False -> effect.none()
          }
          // Open the room via wasm if not already opened.
          let open_eff = case dict.has_key(model.rooms, name), model.client {
            False, Some(client) ->
              effect.from(fn(dispatch) {
                sunset.open_room(client, name, fn(handle) {
                  dispatch(RoomOpened(name, handle))
                })
              })
            _, _ -> effect.none()
          }
          #(
            new_model,
            effect.batch([
              persist_eff,
              effect.from(fn(_) {
                storage.set_hash(name)
                Nil
              }),
              open_eff,
            ]),
          )
        }
      }
    }
    DeleteRoom(name) -> {
      let new_room_list = list.filter(model.joined_rooms, fn(r) { r != name })
      let active_was_deleted = case model.view {
        RoomView(active) -> active == name
        LandingView -> False
      }
      let new_view = case active_was_deleted, new_room_list {
        True, [next, ..] -> RoomView(next)
        True, [] -> LandingView
        False, _ -> model.view
      }
      let updated_rooms_dict = dict.delete(model.rooms, name)
      let persist =
        effect.from(fn(_) {
          storage.write_joined_rooms(new_room_list)
          case new_view {
            RoomView(n) -> storage.set_hash(n)
            LandingView -> storage.set_hash("")
          }
          Nil
        })
      #(
        Model(
          ..model,
          joined_rooms: new_room_list,
          view: new_view,
          rooms: updated_rooms_dict,
        ),
        persist,
      )
    }
    GoToLanding -> {
      let persist =
        effect.from(fn(_) {
          storage.set_hash("")
          Nil
        })
      #(Model(..model, view: LandingView), persist)
    }
    DragRoomStart(name) -> #(
      Model(..model, dragging_room: Some(name)),
      effect.none(),
    )
    DragRoomOver(name) -> {
      let next = case model.drag_over_room {
        Some(current) if current == name -> model.drag_over_room
        _ -> Some(name)
      }
      #(Model(..model, drag_over_room: next), effect.none())
    }
    DragRoomLeave(name) -> {
      let next = case model.drag_over_room {
        Some(current) if current == name -> None
        _ -> model.drag_over_room
      }
      #(Model(..model, drag_over_room: next), effect.none())
    }
    DropRoomOn(target) -> {
      case model.dragging_room {
        None -> #(Model(..model, drag_over_room: None), effect.none())
        Some(src) -> {
          let new_room_list = reorder_before(model.joined_rooms, src, target)
          let persist = case new_room_list == model.joined_rooms {
            True -> effect.none()
            False ->
              effect.from(fn(_) {
                storage.write_joined_rooms(new_room_list)
                Nil
              })
          }
          #(
            Model(
              ..model,
              joined_rooms: new_room_list,
              dragging_room: None,
              drag_over_room: None,
            ),
            persist,
          )
        }
      }
    }
    DragRoomEnd -> #(
      Model(..model, dragging_room: None, drag_over_room: None),
      effect.none(),
    )
    SelectChannel(id) ->
      with_active_room(model, fn(state) {
        #(RoomState(..state, current_channel: id), effect.none())
      })
    ToggleRoomsRail -> #(
      Model(..model, rooms_collapsed: !model.rooms_collapsed),
      effect.none(),
    )
    UpdateDraft(s) ->
      with_active_room(model, fn(state) {
        #(RoomState(..state, draft: s), effect.none())
      })
    IdentityReady(seed) -> {
      let create_client_eff =
        effect.from(fn(dispatch) {
          sunset.create_client(seed, fn(client) {
            dispatch(ClientReady(client))
          })
        })
      #(model, create_client_eff)
    }
    ClientReady(client) -> {
      // Connect to relays.
      let relays = case sunset.relay_url_param() {
        Ok(url) -> [url]
        Error(_) -> default_relays
      }
      let connect_eff =
        effect.from(fn(dispatch) {
          list.each(relays, fn(url) {
            sunset.add_relay(client, url, fn(r) {
              dispatch(RelayConnectResult(r))
            })
          })
        })

      // Open rooms: the active room first (immediate), others staggered.
      let active_name = case model.view {
        RoomView(name) -> name
        LandingView ->
          case model.joined_rooms {
            [] -> ""
            [first, ..] -> first
          }
      }
      let other_names =
        list.filter(model.joined_rooms, fn(n) { n != active_name })

      let open_active_eff = case active_name {
        "" -> effect.none()
        name ->
          effect.from(fn(dispatch) {
            sunset.open_room(client, name, fn(handle) {
              dispatch(RoomOpened(name, handle))
            })
          })
      }
      // Stagger other rooms 50 ms apart to avoid KDF contention.
      let open_others_eff =
        effect.from(fn(dispatch) {
          list.index_map(other_names, fn(name, i) {
            sunset.set_timeout_ms(i * 50, fn() {
              sunset.open_room(client, name, fn(handle) {
                dispatch(RoomOpened(name, handle))
              })
            })
          })
          Nil
        })

      let new_status = case relays {
        [] -> "disconnected"
        _ -> "connecting"
      }
      #(
        Model(..model, client: Some(client), relay_status: new_status),
        effect.batch([connect_eff, open_active_eff, open_others_eff]),
      )
    }
    RelayConnectResult(Ok(_)) -> #(
      Model(..model, relay_status: "connected"),
      effect.none(),
    )
    RelayConnectResult(Error(_)) -> #(
      Model(..model, relay_status: "error"),
      effect.none(),
    )
    RoomOpened(name, handle) -> {
      let state = empty_room_state(handle)
      let new_rooms = dict.insert(model.rooms, name, state)
      let #(interval, ttl, refresh) = sunset.presence_params_from_url()
      let wire_eff =
        effect.from(fn(dispatch) {
          sunset.on_message(handle, fn(im) { dispatch(IncomingMsg(name, im)) })
          sunset.on_receipt(handle, fn(r) {
            dispatch(IncomingReceipt(
              name,
              sunset.rec_for_value_hash_hex(r),
              short_pubkey(sunset.rec_from_pubkey(r)),
            ))
          })
          sunset.on_members_changed(handle, fn(ms) {
            dispatch(MembersUpdated(name, map_members(ms)))
          })
          sunset.on_relay_status_changed(handle, fn(s) {
            dispatch(RelayStatusUpdated(s))
          })
          sunset.start_presence(handle, interval, ttl, refresh)
        })
      #(Model(..model, rooms: new_rooms), wire_eff)
    }
    IncomingMsg(name, im) -> {
      case dict.get(model.rooms, name) {
        Error(_) -> #(model, effect.none())
        Ok(state) -> {
          let new_msg =
            domain.Message(
              id: sunset.inc_value_hash_hex(im),
              author: short_pubkey(sunset.inc_author_pubkey(im)),
              initials: short_initials(sunset.inc_author_pubkey(im)),
              time: format_time_ms(sunset.inc_sent_at_ms(im)),
              body: sunset.inc_body(im),
              seen_by: 0,
              you: sunset.inc_is_self(im),
              pending: False,
              reactions: [],
              bridge: NoBridge,
              details: domain.NoDetails,
            )
          // Append; dedupe by id to handle Replay::All re-emits.
          let updated = case
            list.any(state.messages, fn(m) { m.id == new_msg.id })
          {
            True -> state.messages
            False -> list.append(state.messages, [new_msg])
          }
          let new_state = RoomState(..state, messages: updated)
          #(
            Model(..model, rooms: dict.insert(model.rooms, name, new_state)),
            effect.none(),
          )
        }
      }
    }
    SubmitDraft -> {
      let active_name = active_room_name(model)
      case active_name, dict.get(model.rooms, active_name) {
        "", _ -> #(model, effect.none())
        _, Error(_) -> #(model, effect.none())
        _, Ok(state) -> {
          let body = sanitize(state.draft)
          case body {
            "" -> #(model, effect.none())
            _ -> {
              let send_eff =
                effect.from(fn(dispatch) {
                  sunset.send_message(
                    state.handle,
                    body,
                    current_time_ms(),
                    fn(r) { dispatch(MessageSent(r)) },
                  )
                })
              let cleared = RoomState(..state, draft: "")
              #(
                Model(
                  ..model,
                  rooms: dict.insert(model.rooms, active_name, cleared),
                ),
                send_eff,
              )
            }
          }
        }
      }
    }
    MessageSent(_) -> #(model, effect.none())
    IncomingReceipt(name, message_id, from_pubkey) -> {
      case dict.get(model.rooms, name) {
        Error(_) -> #(model, effect.none())
        Ok(state) -> {
          let existing = case dict.get(state.receipts, message_id) {
            Ok(s) -> s
            Error(_) -> set.new()
          }
          let updated = set.insert(existing, from_pubkey)
          let new_state =
            RoomState(
              ..state,
              receipts: dict.insert(state.receipts, message_id, updated),
            )
          #(
            Model(..model, rooms: dict.insert(model.rooms, name, new_state)),
            effect.none(),
          )
        }
      }
    }
    MembersUpdated(name, ms) -> {
      case dict.get(model.rooms, name) {
        Error(_) -> #(model, effect.none())
        Ok(state) -> {
          // If the open popover's target left, close it.
          let next_popover = case state.peer_status_popover {
            None -> None
            Some(target) ->
              case list.find(ms, fn(m) { m.id == target }) {
                Ok(_) -> Some(target)
                Error(_) -> None
              }
          }
          let new_state =
            RoomState(..state, members: ms, peer_status_popover: next_popover)
          #(
            Model(..model, rooms: dict.insert(model.rooms, name, new_state)),
            effect.none(),
          )
        }
      }
    }
    RelayStatusUpdated(s) -> #(Model(..model, relay_status: s), effect.none())
    ToggleMessageSelected(id) ->
      with_active_room(model, fn(state) {
        // Tap/click on a message body. Toggle selection — same id
        // deselects, different id replaces. Closing also dismisses any
        // open reaction picker for the previously-selected message so
        // the UI doesn't end up with a phantom picker on a hidden row.
        let next = case state.selected_msg_id {
          Some(open) if open == id -> None
          _ -> Some(id)
        }
        let next_picker = case next {
          None -> None
          Some(_) -> state.reacting_to
        }
        #(
          RoomState(..state, selected_msg_id: next, reacting_to: next_picker),
          effect.none(),
        )
      })
    ToggleReactionPicker(id) ->
      with_active_room(model, fn(state) {
        let next = case state.reacting_to {
          Some(open) if open == id -> None
          _ -> Some(id)
        }
        #(RoomState(..state, reacting_to: next), effect.none())
      })
    AddReaction(id, emoji) ->
      with_active_room(model, fn(state) {
        let current = case dict.get(state.reactions, id) {
          Ok(rs) -> rs
          Error(_) -> []
        }
        let next = toggle_reaction(current, emoji)
        #(
          RoomState(
            ..state,
            reactions: dict.insert(state.reactions, id, next),
            reacting_to: None,
          ),
          effect.none(),
        )
      })
    OpenDetail(id) ->
      with_active_room(model, fn(state) {
        // Opening the details panel pins selection on the same id so the
        // row's action toolbar stays visible while the panel is open and
        // no other row's hover affordance can sneak in alongside it
        // (the global stylesheet's :has() rule keys off .is-selected).
        #(
          RoomState(
            ..state,
            sheet: Some(domain.DetailsSheet(message_id: id)),
            reacting_to: None,
            selected_msg_id: Some(id),
          ),
          effect.none(),
        )
      })
    CloseDetail ->
      with_active_room(model, fn(state) {
        // Only close if the active sheet is the details one — guards against
        // a Voice sheet being opened concurrently and accidentally dismissed.
        // When closing, drop the matching selection so the toolbar collapses
        // back to its default state.
        let #(next_sheet, next_selected) = case state.sheet {
          Some(domain.DetailsSheet(message_id: id)) -> #(
            None,
            case state.selected_msg_id {
              Some(open) if open == id -> None
              other -> other
            },
          )
          other -> #(other, state.selected_msg_id)
        }
        #(
          RoomState(..state, sheet: next_sheet, selected_msg_id: next_selected),
          effect.none(),
        )
      })
    OpenVoicePopover(name) ->
      with_active_room(model, fn(state) {
        #(
          RoomState(
            ..state,
            sheet: Some(domain.VoiceSheet(member_name: name)),
            reacting_to: None,
          ),
          effect.none(),
        )
      })
    CloseVoicePopover ->
      with_active_room(model, fn(state) {
        #(
          RoomState(..state, sheet: case state.sheet {
            Some(domain.VoiceSheet(_)) -> None
            other -> other
          }),
          effect.none(),
        )
      })
    OpenPeerStatusPopover(member_id) ->
      with_active_room(model, fn(state) {
        #(
          RoomState(..state, peer_status_popover: Some(member_id)),
          effect.none(),
        )
      })
    ClosePeerStatusPopover ->
      with_active_room(model, fn(state) {
        #(RoomState(..state, peer_status_popover: None), effect.none())
      })
    Tick(now) -> #(Model(..model, now_ms: now), effect.none())
    SetMemberVolume(name, value) -> {
      let settings = member_voice_settings(model.voice_settings, name)
      let next = domain.VoiceSettings(..settings, volume: value)
      #(
        Model(
          ..model,
          voice_settings: dict.insert(model.voice_settings, name, next),
        ),
        effect.none(),
      )
    }
    ToggleMemberDenoise(name) -> {
      let settings = member_voice_settings(model.voice_settings, name)
      let next = domain.VoiceSettings(..settings, denoise: !settings.denoise)
      #(
        Model(
          ..model,
          voice_settings: dict.insert(model.voice_settings, name, next),
        ),
        effect.none(),
      )
    }
    ToggleMemberDeafen(name) -> {
      let settings = member_voice_settings(model.voice_settings, name)
      let next = domain.VoiceSettings(..settings, deafened: !settings.deafened)
      #(
        Model(
          ..model,
          voice_settings: dict.insert(model.voice_settings, name, next),
        ),
        effect.none(),
      )
    }
    ResetMemberVoice(name) -> #(
      Model(
        ..model,
        voice_settings: dict.insert(
          model.voice_settings,
          name,
          domain.VoiceSettings(volume: 100, denoise: True, deafened: False),
        ),
      ),
      effect.none(),
    )
    ViewportChanged(v) -> {
      // Crossing the boundary in either direction closes any open drawer.
      // Sheets intentionally survive: DetailsSheet and VoiceSheet render
      // on both viewports (right-rail / floating popover on desktop).
      #(Model(..model, viewport: v, drawer: None), effect.none())
    }
    OpenDrawer(d) -> #(Model(..model, drawer: Some(d)), effect.none())
    CloseDrawer -> #(Model(..model, drawer: None), effect.none())
  }
}

/// Toggle "you reacted with this emoji" semantics over an existing
/// reactions list. Mirrors the natural chat pattern: first click adds,
/// second click removes; the count tracks how many distinct people
/// (including you) have reacted with that emoji.
fn toggle_reaction(rs: List(Reaction), emoji: String) -> List(Reaction) {
  case list.find(rs, fn(r) { r.emoji == emoji }) {
    Error(_) -> [Reaction(emoji: emoji, count: 1, by_you: True), ..rs]
    Ok(existing) ->
      case existing.by_you {
        True -> {
          let updated_count = existing.count - 1
          case updated_count {
            0 -> list.filter(rs, fn(r) { r.emoji != emoji })
            _ ->
              list.map(rs, fn(r) {
                case r.emoji == emoji {
                  True ->
                    Reaction(
                      emoji: r.emoji,
                      count: updated_count,
                      by_you: False,
                    )
                  False -> r
                }
              })
          }
        }
        False ->
          list.map(rs, fn(r) {
            case r.emoji == emoji {
              True -> Reaction(emoji: r.emoji, count: r.count + 1, by_you: True)
              False -> r
            }
          })
      }
  }
}

fn view(model: Model) -> Element(Msg) {
  let palette = theme.palette_for(model.mode)
  case model.view {
    LandingView ->
      landing.view(
        palette: palette,
        mode: model.mode,
        viewport: model.viewport,
        input: model.landing_input,
        noop: NoOp,
        on_input: UpdateLandingInput,
        on_join: JoinRoom,
        on_toggle_mode: ToggleMode,
      )
    RoomView(name) -> room_view(model, palette, name)
  }
}

fn room_view(model: Model, palette, current_name: String) -> Element(Msg) {
  case dict.get(model.rooms, current_name) {
    Error(_) -> loading_room_view(palette, current_name)
    Ok(state) -> room_view_with_state(model, palette, current_name, state)
  }
}

fn loading_room_view(palette: theme.Palette, name: String) -> Element(Msg) {
  html.div(
    [
      ui.css([
        #("display", "flex"),
        #("align-items", "center"),
        #("justify-content", "center"),
        #("height", "100vh"),
        #("height", "100dvh"),
        #("background", palette.bg),
        #("color", palette.text_muted),
        #("font-size", "1rem"),
        #("font-family", "inherit"),
      ]),
    ],
    [html.text("opening " <> name <> "…")],
  )
}

fn room_view_with_state(
  model: Model,
  palette,
  current_name: String,
  state: RoomState,
) -> Element(Msg) {
  let displayed_rooms = resolve_rooms(model.joined_rooms, model.relay_status)
  let filtered = filter_rooms(displayed_rooms, model.sidebar_search)
  let active_room =
    lookup_room(displayed_rooms, current_name, model.relay_status)

  let raw_messages = state.messages
  let messages_with_live_reactions =
    list.map(raw_messages, fn(m) {
      case dict.get(state.reactions, m.id) {
        Ok(rs) -> domain.Message(..m, reactions: rs)
        Error(_) -> m
      }
    })

  let detail_msg = case state.sheet {
    Some(domain.DetailsSheet(message_id: id)) ->
      find_message(messages_with_live_reactions, id)
    _ -> None
  }

  let reaction_sheet_el = case model.viewport, state.reacting_to {
    domain.Phone, Some(id) ->
      bottom_sheet.view(
        palette: palette,
        open: True,
        on_close: ToggleReactionPicker(id),
        test_id: "reaction-sheet",
        content: phone_reaction_grid(palette, id),
      )
    _, _ -> element.fragment([])
  }

  let details_sheet_el = case model.viewport, state.sheet {
    domain.Phone, Some(domain.DetailsSheet(message_id: id)) ->
      case find_message(messages_with_live_reactions, id) {
        Some(m) ->
          bottom_sheet.view(
            palette: palette,
            open: True,
            on_close: CloseDetail,
            test_id: "details-sheet",
            content: details_panel.view(
              palette: palette,
              message: m,
              receipts: receipts_for(state.receipts, m.id),
              members: state.members,
              on_close: CloseDetail,
            ),
          )
        None -> element.fragment([])
      }
    _, _ -> element.fragment([])
  }

  let voice_sheet_el = case model.viewport, state.sheet {
    domain.Phone, Some(domain.VoiceSheet(member_name: name)) ->
      case list.find(fixture.members(), fn(m) { m.name == name }) {
        Ok(m) ->
          bottom_sheet.view(
            palette: palette,
            open: True,
            on_close: CloseVoicePopover,
            test_id: "voice-sheet",
            content: voice_popover.view(
              palette: palette,
              placement: voice_popover.InSheet,
              member: m,
              settings: member_voice_settings(model.voice_settings, name),
              on_close: CloseVoicePopover,
              on_set_volume: fn(v) { SetMemberVolume(name, v) },
              on_toggle_denoise: ToggleMemberDenoise(name),
              on_toggle_deafen: ToggleMemberDeafen(name),
              on_reset: ResetMemberVoice(name),
            ),
          )
        Error(_) -> element.fragment([])
      }
    _, _ -> element.fragment([])
  }

  let peer_status_sheet_el = case model.viewport, state.peer_status_popover {
    domain.Phone, Some(member_id) ->
      case list.find(state.members, fn(m) { m.id == member_id }) {
        Ok(m) ->
          bottom_sheet.view(
            palette: palette,
            open: True,
            on_close: ClosePeerStatusPopover,
            test_id: "peer-status-sheet",
            content: peer_status_popover.view(
              palette: palette,
              member: m,
              now_ms: model.now_ms,
              placement: peer_status_popover.InSheet,
              on_close: ClosePeerStatusPopover,
            ),
          )
        Error(_) -> element.fragment([])
      }
    _, _ -> element.fragment([])
  }

  let user_in_call = list.any(fixture.members(), fn(m) { m.you && m.in_call })

  let active_voice_channel_name =
    list.find(fixture.channels(), fn(c) {
      c.kind == domain.Voice && c.in_call > 0
    })
    |> result.map(fn(c) { c.name })
    |> result.unwrap("")

  let voice_minibar_el = case model.viewport, user_in_call {
    domain.Phone, True ->
      voice_minibar.view(
        palette: palette,
        channel_name: active_voice_channel_name,
        on_open: OpenVoicePopover("you"),
      )
    _, _ -> element.fragment([])
  }

  shell.view(
    model.mode,
    palette,
    model.viewport,
    model.rooms_collapsed,
    detail_msg != None,
    model.drawer,
    ToggleMode,
    CloseDrawer,
    rooms.view(
      palette: palette,
      rooms: filtered,
      current_room: RoomId(current_name),
      collapsed: model.rooms_collapsed,
      search: model.sidebar_search,
      noop: NoOp,
      dragging: model.dragging_room,
      drag_over: model.drag_over_room,
      on_select_room: fn(id) {
        let RoomId(name) = id
        JoinRoom(name)
      },
      on_search_change: UpdateSidebarSearch,
      on_join: JoinRoom,
      on_delete: DeleteRoom,
      on_drag_start: DragRoomStart,
      on_drag_over: DragRoomOver,
      on_drag_leave: DragRoomLeave,
      on_drop: DropRoomOn,
      on_drag_end: DragRoomEnd,
      toggle: ToggleRoomsRail,
      viewport: model.viewport,
      members: state.members,
      mode: model.mode,
      on_toggle_mode: ToggleMode,
    ),
    // Voice path stays fixture-backed (in-call counts) — real voice presence is V3.
    channels.view(
      palette: palette,
      room: active_room,
      channels: fixture.channels(),
      members: fixture.members(),
      current_channel: state.current_channel,
      voice_popover_open: case state.sheet {
        Some(domain.VoiceSheet(member_name: name)) -> Some(name)
        _ -> None
      },
      on_select_channel: SelectChannel,
      on_open_voice_popover: OpenVoicePopover,
      viewport: model.viewport,
      on_open_rooms: OpenDrawer(domain.RoomsDrawer),
    ),
    main_panel.view(
      palette: palette,
      viewport: model.viewport,
      current_channel: state.current_channel,
      messages: messages_with_live_reactions,
      draft: state.draft,
      on_draft: UpdateDraft,
      on_submit: SubmitDraft,
      noop: NoOp,
      reacting_to: state.reacting_to,
      detail_msg_id: case state.sheet {
        Some(domain.DetailsSheet(message_id: id)) -> Some(id)
        _ -> None
      },
      on_toggle_reaction_picker: ToggleReactionPicker,
      on_add_reaction: AddReaction,
      on_open_detail: OpenDetail,
      receipts: state.receipts,
      selected_msg_id: state.selected_msg_id,
      on_toggle_selected: ToggleMessageSelected,
      voice_minibar: voice_minibar_el,
    ),
    case model.viewport, detail_msg {
      domain.Desktop, Some(m) ->
        details_panel.view(
          palette: palette,
          message: m,
          receipts: receipts_for(state.receipts, m.id),
          members: state.members,
          on_close: CloseDetail,
        )
      _, _ ->
        members.view(
          palette: palette,
          members: state.members,
          on_open_status: OpenPeerStatusPopover,
        )
    },
    element.fragment([
      voice_popover_overlay(palette, model, state),
      peer_status_popover_overlay(palette, model, state),
    ]),
    phone_header.view(
      palette: palette,
      room: active_room,
      on_open_channels: OpenDrawer(domain.ChannelsDrawer),
      on_open_members: OpenDrawer(domain.MembersDrawer),
    ),
    details_sheet_el,
    voice_sheet_el,
    peer_status_sheet_el,
    reaction_sheet_el,
  )
}

fn voice_popover_overlay(
  palette,
  model: Model,
  state: RoomState,
) -> Element(Msg) {
  case model.viewport, state.sheet {
    domain.Desktop, Some(domain.VoiceSheet(member_name: name)) ->
      // Voice path stays fixture-backed (in-call counts) — real voice presence is V3.
      case list.find(fixture.members(), fn(m) { m.name == name }) {
        Error(_) -> element.fragment([])
        Ok(m) ->
          voice_popover.view(
            palette: palette,
            placement: voice_popover.Floating,
            member: m,
            settings: member_voice_settings(model.voice_settings, name),
            on_close: CloseVoicePopover,
            on_set_volume: fn(v) { SetMemberVolume(name, v) },
            on_toggle_denoise: ToggleMemberDenoise(name),
            on_toggle_deafen: ToggleMemberDeafen(name),
            on_reset: ResetMemberVoice(name),
          )
      }
    _, _ -> element.fragment([])
  }
}

/// Floating popover for desktop. The phone path renders through
/// `peer_status_sheet_el` (a `bottom_sheet.view` wrapper), matching the
/// voice popover convention.
fn peer_status_popover_overlay(
  palette,
  model: Model,
  state: RoomState,
) -> Element(Msg) {
  case model.viewport, state.peer_status_popover {
    domain.Desktop, Some(member_id) ->
      case list.find(state.members, fn(m) { m.id == member_id }) {
        Error(_) -> element.fragment([])
        Ok(m) ->
          peer_status_popover.view(
            palette: palette,
            member: m,
            now_ms: model.now_ms,
            placement: peer_status_popover.Floating,
            on_close: ClosePeerStatusPopover,
          )
      }
    _, _ -> element.fragment([])
  }
}

fn filter_rooms(rs: List(Room), search: String) -> List(Room) {
  let needle = string.lowercase(string.trim(search))
  case needle {
    "" -> rs
    _ ->
      list.filter(rs, fn(r) {
        string.contains(does: string.lowercase(r.name), contain: needle)
      })
  }
}

/// Resolve a list of joined room names to rich Room records. Names
/// that match a fixture room reuse its mock data; anything else falls
/// back to a synthetic Room so the rail still renders something useful.
fn resolve_rooms(names: List(String), relay_status: String) -> List(Room) {
  let fixture_rooms = fixture.rooms()
  list.map(names, fn(name) {
    case list.find(fixture_rooms, fn(r) { r.name == name }) {
      Ok(r) ->
        Room(..r, status: relay_status_to_conn(relay_status), id: RoomId(name))
      Error(_) -> synthetic_room(name, relay_status)
    }
  })
}

fn lookup_room(rs: List(Room), name: String, relay_status: String) -> Room {
  case list.find(rs, fn(r) { r.name == name }) {
    Ok(r) -> r
    Error(_) -> synthetic_room(name, relay_status)
  }
}

/// Default Room record for a name we have no fixture entry for. Reads
/// like a freshly-joined room with no observed activity yet.
fn synthetic_room(name: String, relay_status: String) -> Room {
  Room(
    id: RoomId(name),
    name: name,
    members: 1,
    online: 1,
    in_call: 0,
    status: relay_status_to_conn(relay_status),
    last_active: "now",
    unread: 0,
    bridge: NoBridge,
  )
}

fn relay_status_to_conn(relay_status: String) -> domain.ConnStatus {
  case relay_status {
    "connected" -> domain.Connected
    "connecting" -> domain.Reconnecting
    "error" -> domain.Offline
    "disconnected" -> domain.Offline
    _ -> domain.Connected
  }
}

fn find_message(ms: List(Message), id: String) -> Option(Message) {
  list.find(ms, fn(m) { m.id == id })
  |> option.from_result
}

fn receipts_for(receipts: Dict(String, Set(String)), id: String) -> Set(String) {
  case dict.get(receipts, id) {
    Ok(s) -> s
    Error(_) -> set.new()
  }
}

fn map_members(ms: List(sunset.MemberJs)) -> List(domain.Member) {
  list.map(ms, fn(m) {
    let pk = sunset.mem_pubkey(m)
    domain.Member(
      id: domain.MemberId(short_pubkey(pk)),
      name: short_pubkey(pk),
      initials: short_initials(pk),
      status: presence_to_status(sunset.mem_presence(m)),
      relay: connection_mode_to_relay(sunset.mem_connection_mode(m)),
      you: sunset.mem_is_self(m),
      in_call: False,
      bridge: domain.NoBridge,
      role: domain.NoRole,
      last_heartbeat_ms: sunset.mem_last_heartbeat_ms(m),
    )
  })
}

fn presence_to_status(s: String) -> domain.Presence {
  case s {
    "online" -> domain.Online
    "away" -> domain.Away
    _ -> domain.OfflineP
  }
}

fn connection_mode_to_relay(s: String) -> domain.RelayStatus {
  case s {
    "direct" -> domain.Direct
    "via_relay" -> domain.OneHop
    "self" -> domain.SelfRelay
    _ -> domain.NoRelay
  }
}

fn phone_reaction_grid(
  palette: theme.Palette,
  message_id: String,
) -> Element(Msg) {
  let emojis = ["🌅", "👍", "👀", "🔥", "🌙"]
  html.div(
    [
      attribute.attribute("data-testid", "reaction-picker"),
      ui.css([
        #("display", "grid"),
        #("grid-template-columns", "repeat(5, 1fr)"),
        #("gap", "8px"),
        #("padding", "16px 16px 24px 16px"),
      ]),
    ],
    list.map(emojis, fn(e) {
      html.button(
        [
          attribute.attribute("aria-label", e),
          event.on_click(AddReaction(message_id, e)),
          ui.css([
            #("padding", "12px"),
            #("font-size", "26px"),
            #("border", "1px solid " <> palette.border_soft),
            #("background", palette.surface),
            #("color", palette.text),
            #("border-radius", "10px"),
            #("font-family", "inherit"),
            #("cursor", "pointer"),
          ]),
        ],
        [html.text(e)],
      )
    }),
  )
}
