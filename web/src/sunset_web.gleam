//// sunset.chat — Gleam + Lustre frontend (D1 visual shell).
////
//// Static fixture data; no backend wiring yet. Routing details, voice
//// controls, reactions, attachments and the message-details panel are
//// rendered as static state. Eventually swapped for live `sunset-store`
//// data over WASM FFI.

import lustre
import lustre/element.{type Element}
import lustre/element/html
import sunset_web/domain.{type ChannelId, type RoomId, ChannelId, RoomId}
import sunset_web/fixture
import sunset_web/theme.{type Mode, type Palette, Dark, Light}
import sunset_web/ui
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
    ToggleRoomsRail ->
      Model(..model, rooms_collapsed: !model.rooms_collapsed)
    UpdateDraft(s) -> Model(..model, draft: s)
  }
}

fn view(model: Model) -> Element(Msg) {
  let palette = theme.palette_for(model.mode)

  shell.view(
    model.mode,
    palette,
    model.rooms_collapsed,
    ToggleMode,
    placeholder_panel(palette, "rooms"),
    placeholder_panel(palette, "channels"),
    placeholder_panel(palette, "main"),
    placeholder_panel(palette, "members"),
  )
}

fn placeholder_panel(palette: Palette, label: String) -> Element(Msg) {
  html.div(
    [
      ui.css([
        #("border-right", "1px solid " <> palette.border),
        #("padding", "16px"),
        #("color", palette.text_faint),
        #("background", palette.surface_alt),
        #("overflow", "auto"),
      ]),
    ],
    [html.text(label)],
  )
}
