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
import gleam/string
import lustre
import lustre/effect.{type Effect}
import lustre/element.{type Element}
import sunset_web/domain.{
  type ChannelId, type Message, type Reaction, type Room, ChannelId, NoBridge,
  NoRelay, Reaction, Reconnecting, Room, RoomId,
}
import sunset_web/fixture
import sunset_web/storage
import sunset_web/theme.{type Mode, Dark, Light}
import sunset_web/views/channels
import sunset_web/views/details_panel
import sunset_web/views/landing
import sunset_web/views/main_panel
import sunset_web/views/members
import sunset_web/views/rooms
import sunset_web/views/shell

pub type View {
  LandingView
  RoomView(name: String)
}

pub type Model {
  Model(
    mode: Mode,
    view: View,
    joined_rooms: List(String),
    rooms_collapsed: Bool,
    landing_input: String,
    sidebar_search: String,
    current_channel: ChannelId,
    draft: String,
    reacting_to: Option(String),
    detail_msg_id: Option(String),
    reactions: Dict(String, List(Reaction)),
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
  SelectChannel(ChannelId)
  ToggleRoomsRail
  UpdateDraft(String)
  ToggleReactionPicker(String)
  AddReaction(String, String)
  OpenDetail(String)
  CloseDetail
}

pub fn main() {
  let app = lustre.application(init, update, view)
  let assert Ok(_) = lustre.start(app, "#app", Nil)
  Nil
}

fn init(_flags: Nil) -> #(Model, Effect(Msg)) {
  let stored_rooms = storage.read_joined_rooms()
  let last_used = storage.read_last_used()
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
  //   * Otherwise fall back to the persisted last-used room.
  //   * Otherwise show the landing page.
  // Direct-navigation to a room not yet in the joined list auto-joins
  // it (so a shared link adds the room rather than dropping the user
  // back to landing).
  let initial_view = case initial_hash, last_used, stored_rooms {
    "", "", _ -> LandingView
    "", _, [] if last_used == "" -> LandingView
    "", remembered, _ if remembered != "" -> RoomView(remembered)
    name, _, _ -> RoomView(name)
  }

  let joined = case initial_view {
    LandingView -> stored_rooms
    RoomView(name) -> ensure_joined(stored_rooms, name)
  }

  let model =
    Model(
      mode: initial_mode,
      view: initial_view,
      joined_rooms: joined,
      rooms_collapsed: False,
      landing_input: "",
      sidebar_search: "",
      current_channel: ChannelId(fixture.initial_channel_id),
      draft: "",
      reacting_to: None,
      detail_msg_id: None,
      reactions: seed_reactions(),
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
        storage.write_last_used(name)
        Nil
      })
    RoomView(name), _ ->
      effect.from(fn(_) {
        storage.write_last_used(name)
        Nil
      })
    LandingView, _ -> effect.none()
  }

  #(model, effect.batch([subscribe_hash, initial_persist, initial_hash_sync]))
}

fn seed_reactions() -> Dict(String, List(Reaction)) {
  fixture.messages()
  |> list.fold(dict.new(), fn(d, m) { dict.insert(d, m.id, m.reactions) })
}

/// If joining a new room, add it to the head of the list.
/// Otherwise, return the existing list preserving order.
fn ensure_joined(existing: List(String), name: String) -> List(String) {
  case list.contains(existing, name) {
    True -> existing
    False -> [name, ..existing]
  }
}

fn sanitize(raw: String) -> String {
  string.trim(raw)
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
      let last_used_eff = case new_view {
        RoomView(name) ->
          effect.from(fn(_) {
            storage.write_last_used(name)
            Nil
          })
        LandingView -> effect.none()
      }
      #(
        Model(..model, view: new_view, joined_rooms: new_rooms),
        effect.batch([persisted, last_used_eff]),
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
          let new_rooms = ensure_joined(model.joined_rooms, name)
          let new_model =
            Model(
              ..model,
              joined_rooms: new_rooms,
              view: RoomView(name),
              landing_input: "",
              sidebar_search: "",
            )
          #(
            new_model,
            effect.from(fn(_) {
              storage.write_joined_rooms(new_rooms)
              storage.write_last_used(name)
              storage.set_hash(name)
              Nil
            }),
          )
        }
      }
    }
    DeleteRoom(name) -> {
      let new_rooms = list.filter(model.joined_rooms, fn(r) { r != name })
      let active_was_deleted = case model.view {
        RoomView(active) -> active == name
        LandingView -> False
      }
      let new_view = case active_was_deleted, new_rooms {
        True, [next, ..] -> RoomView(next)
        True, [] -> LandingView
        False, _ -> model.view
      }
      let new_last_used = case new_view {
        RoomView(n) -> n
        LandingView -> ""
      }
      let persist =
        effect.from(fn(_) {
          storage.write_joined_rooms(new_rooms)
          storage.write_last_used(new_last_used)
          case new_view {
            RoomView(n) -> storage.set_hash(n)
            LandingView -> storage.set_hash("")
          }
          Nil
        })
      #(Model(..model, joined_rooms: new_rooms, view: new_view), persist)
    }
    GoToLanding -> {
      let persist =
        effect.from(fn(_) {
          storage.set_hash("")
          Nil
        })
      #(Model(..model, view: LandingView), persist)
    }
    SelectChannel(id) -> #(Model(..model, current_channel: id), effect.none())
    ToggleRoomsRail -> #(
      Model(..model, rooms_collapsed: !model.rooms_collapsed),
      effect.none(),
    )
    UpdateDraft(s) -> #(Model(..model, draft: s), effect.none())
    ToggleReactionPicker(id) -> {
      let next = case model.reacting_to {
        Some(open) if open == id -> None
        _ -> Some(id)
      }
      #(Model(..model, reacting_to: next), effect.none())
    }
    AddReaction(id, emoji) -> {
      let current = case dict.get(model.reactions, id) {
        Ok(rs) -> rs
        Error(_) -> []
      }
      let next = toggle_reaction(current, emoji)
      let new_model =
        Model(
          ..model,
          reactions: dict.insert(model.reactions, id, next),
          reacting_to: None,
        )
      #(new_model, effect.none())
    }
    OpenDetail(id) -> #(
      Model(..model, detail_msg_id: Some(id), reacting_to: None),
      effect.none(),
    )
    CloseDetail -> #(Model(..model, detail_msg_id: None), effect.none())
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
  let displayed_rooms = resolve_rooms(model.joined_rooms)
  let filtered = filter_rooms(displayed_rooms, model.sidebar_search)
  let active_room = lookup_room(displayed_rooms, current_name)

  let raw_messages = fixture.messages()
  let messages_with_live_reactions =
    list.map(raw_messages, fn(m) {
      case dict.get(model.reactions, m.id) {
        Ok(rs) -> domain.Message(..m, reactions: rs)
        Error(_) -> m
      }
    })

  let detail_msg = case model.detail_msg_id {
    None -> None
    Some(id) -> find_message(messages_with_live_reactions, id)
  }

  shell.view(
    model.mode,
    palette,
    model.rooms_collapsed,
    detail_msg != None,
    ToggleMode,
    rooms.view(
      palette: palette,
      rooms: filtered,
      current_room: RoomId(current_name),
      collapsed: model.rooms_collapsed,
      search: model.sidebar_search,
      noop: NoOp,
      on_select_room: fn(id) {
        let RoomId(name) = id
        JoinRoom(name)
      },
      on_search_change: UpdateSidebarSearch,
      on_join: JoinRoom,
      on_delete: DeleteRoom,
      toggle: ToggleRoomsRail,
    ),
    channels.view(
      palette: palette,
      room: active_room,
      channels: fixture.channels(),
      members: fixture.members(),
      current_channel: model.current_channel,
      on_select_channel: SelectChannel,
    ),
    main_panel.view(
      palette: palette,
      current_channel: model.current_channel,
      messages: messages_with_live_reactions,
      draft: model.draft,
      on_draft: UpdateDraft,
      reacting_to: model.reacting_to,
      detail_msg_id: model.detail_msg_id,
      on_toggle_reaction_picker: ToggleReactionPicker,
      on_add_reaction: AddReaction,
      on_open_detail: OpenDetail,
    ),
    case detail_msg {
      Some(m) ->
        details_panel.view(palette: palette, message: m, on_close: CloseDetail)
      None -> members.view(palette: palette, members: fixture.members())
    },
  )
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
fn resolve_rooms(names: List(String)) -> List(Room) {
  let fixture_rooms = fixture.rooms()
  list.map(names, fn(name) {
    case list.find(fixture_rooms, fn(r) { r.name == name }) {
      Ok(r) -> Room(..r, id: RoomId(name))
      Error(_) -> synthetic_room(name)
    }
  })
}

fn lookup_room(rs: List(Room), name: String) -> Room {
  case list.find(rs, fn(r) { r.name == name }) {
    Ok(r) -> r
    Error(_) -> synthetic_room(name)
  }
}

/// Default Room record for a name we have no fixture entry for. Reads
/// like a freshly-joined room with no observed activity yet.
fn synthetic_room(name: String) -> Room {
  let _ = NoRelay
  let _ = Reconnecting
  Room(
    id: RoomId(name),
    name: name,
    members: 1,
    online: 1,
    in_call: 0,
    status: domain.Connected,
    last_active: "now",
    unread: 0,
    bridge: NoBridge,
  )
}

fn find_message(ms: List(Message), id: String) -> Option(Message) {
  list.find(ms, fn(m) { m.id == id })
  |> option.from_result
}
