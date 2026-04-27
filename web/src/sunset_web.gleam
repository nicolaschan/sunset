//// sunset.chat — Gleam + Lustre frontend (D1 visual shell).
////
//// Static fixture data; no backend wiring yet. Voice popovers,
//// attachments, and read-up-to-here details panel are rendered as
//// static state. Eventually swapped for live `sunset-store` data over
//// WASM FFI.

import gleam/dict.{type Dict}
import gleam/list
import gleam/option.{type Option, None, Some}
import lustre
import lustre/element.{type Element}
import sunset_web/domain.{
  type ChannelId, type Message, type Reaction, type Room, type RoomId, ChannelId,
  Reaction, RoomId,
}
import sunset_web/fixture
import sunset_web/theme.{type Mode, Dark, Light}
import sunset_web/views/channels
import sunset_web/views/details_panel
import sunset_web/views/main_panel
import sunset_web/views/members
import sunset_web/views/rooms
import sunset_web/views/shell

pub type Model {
  Model(
    mode: Mode,
    current_room: RoomId,
    current_channel: ChannelId,
    rooms_collapsed: Bool,
    draft: String,
    /// Message id whose quick-react picker is currently open. None when
    /// no picker is showing.
    reacting_to: Option(String),
    /// Message id whose details panel is currently open. None means the
    /// right column shows the regular members rail instead.
    detail_msg_id: Option(String),
    /// Mutable reactions per message id, seeded from the fixture and
    /// mutated by AddReaction / RemoveReaction.
    reactions: Dict(String, List(Reaction)),
  )
}

pub type Msg {
  ToggleMode
  SelectRoom(RoomId)
  SelectChannel(ChannelId)
  ToggleRoomsRail
  UpdateDraft(String)
  ToggleReactionPicker(String)
  AddReaction(String, String)
  OpenDetail(String)
  CloseDetail
}

pub fn main() {
  let app = lustre.simple(init, update, view)
  let assert Ok(_) = lustre.start(app, "#app", Nil)
  Nil
}

fn init(_flags: Nil) -> Model {
  let initial_reactions =
    fixture.messages()
    |> list.fold(dict.new(), fn(d, m) { dict.insert(d, m.id, m.reactions) })
  Model(
    mode: Light,
    current_room: RoomId(fixture.initial_room_id),
    current_channel: ChannelId(fixture.initial_channel_id),
    rooms_collapsed: False,
    draft: "",
    reacting_to: None,
    detail_msg_id: None,
    reactions: initial_reactions,
  )
}

fn update(model: Model, msg: Msg) -> Model {
  case msg {
    ToggleMode ->
      Model(..model, mode: case model.mode {
        Light -> Dark
        Dark -> Light
      })
    SelectRoom(id) -> Model(..model, current_room: id)
    SelectChannel(id) -> Model(..model, current_channel: id)
    ToggleRoomsRail -> Model(..model, rooms_collapsed: !model.rooms_collapsed)
    UpdateDraft(s) -> Model(..model, draft: s)
    ToggleReactionPicker(id) -> {
      let next = case model.reacting_to {
        Some(open) if open == id -> None
        _ -> Some(id)
      }
      Model(..model, reacting_to: next)
    }
    AddReaction(id, emoji) -> {
      let current = case dict.get(model.reactions, id) {
        Ok(rs) -> rs
        Error(_) -> []
      }
      let next = toggle_reaction(current, emoji)
      Model(
        ..model,
        reactions: dict.insert(model.reactions, id, next),
        reacting_to: None,
      )
    }
    OpenDetail(id) -> Model(..model, detail_msg_id: Some(id), reacting_to: None)
    CloseDetail -> Model(..model, detail_msg_id: None)
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
  let rs = fixture.rooms()
  let room = case current_room(rs, model.current_room) {
    Some(r) -> r
    None -> {
      // Fixture has at least one room; this branch is unreachable but
      // gives the compiler a fallback so the view stays total.
      let assert [first, ..] = rs
      first
    }
  }
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
      rooms: rs,
      current_room: model.current_room,
      collapsed: model.rooms_collapsed,
      on_select_room: SelectRoom,
      toggle: ToggleRoomsRail,
    ),
    channels.view(
      palette: palette,
      room: room,
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

fn current_room(rs: List(Room), id: RoomId) -> Option(Room) {
  list.find(rs, fn(r) { r.id == id })
  |> option.from_result
}

fn find_message(ms: List(Message), id: String) -> Option(Message) {
  list.find(ms, fn(m) { m.id == id })
  |> option.from_result
}
