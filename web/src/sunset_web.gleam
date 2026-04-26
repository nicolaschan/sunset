//// sunset.chat — Gleam + Lustre frontend (D1 visual shell).
////
//// Static fixture data; no backend wiring yet. Routing details, voice
//// controls, reactions, attachments and the message-details panel are
//// rendered as static state. Eventually swapped for live `sunset-store`
//// data over WASM FFI.

import gleam/list
import gleam/option.{type Option, None, Some}
import lustre
import lustre/element.{type Element}
import sunset_web/domain.{
  type ChannelId, type Room, type RoomId, ChannelId, RoomId,
}
import sunset_web/fixture
import sunset_web/theme.{type Mode, Dark, Light}
import sunset_web/views/channels
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
  )
}

pub type Msg {
  ToggleMode
  SelectRoom(RoomId)
  SelectChannel(ChannelId)
  ToggleRoomsRail
  UpdateDraft(String)
}

pub fn main() {
  let app = lustre.simple(init, update, view)
  let assert Ok(_) = lustre.start(app, "#app", Nil)
  Nil
}

fn init(_flags: Nil) -> Model {
  Model(
    mode: Light,
    current_room: RoomId(fixture.initial_room_id),
    current_channel: ChannelId(fixture.initial_channel_id),
    rooms_collapsed: False,
    draft: "",
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

  shell.view(
    model.mode,
    palette,
    model.rooms_collapsed,
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
      messages: fixture.messages(),
      draft: model.draft,
      on_draft: UpdateDraft,
    ),
    members.view(palette: palette, members: fixture.members()),
  )
}

fn current_room(rs: List(Room), id: RoomId) -> Option(Room) {
  list.find(rs, fn(r) { r.id == id })
  |> option.from_result
}
