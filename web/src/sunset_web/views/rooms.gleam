//// Rooms rail (left column) — full and collapsed variants.

import gleam/dynamic/decode
import gleam/int
import gleam/list
import gleam/option.{type Option, Some}
import gleam/string
import lustre/attribute
import lustre/element.{type Element}
import lustre/element/html
import lustre/event
import sunset_web/domain.{
  type ConnStatus, type Room, type RoomId, Connected, Offline, Reconnecting,
}
import sunset_web/theme.{type Palette}
import sunset_web/ui

pub fn view(
  palette p: Palette,
  rooms rs: List(Room),
  current_room cur: RoomId,
  collapsed col: Bool,
  search search_value: String,
  noop noop: msg,
  dragging dragging: Option(String),
  drag_over drag_over: Option(String),
  on_select_room sel: fn(RoomId) -> msg,
  on_search_change on_search_change: fn(String) -> msg,
  on_join on_join: fn(String) -> msg,
  on_delete on_delete: fn(String) -> msg,
  on_drag_start on_drag_start: fn(String) -> msg,
  on_drag_over on_drag_over: fn(String) -> msg,
  on_drag_leave on_drag_leave: fn(String) -> msg,
  on_drop on_drop: fn(String) -> msg,
  on_drag_end on_drag_end: msg,
  toggle toggle: msg,
) -> Element(msg) {
  let width = case col {
    True -> "54px"
    False -> "260px"
  }
  html.aside(
    [
      attribute.attribute("data-testid", "rooms-rail"),
      ui.css([
        #("width", width),
        #("min-width", width),
        #("height", "100vh"),
        #("display", "flex"),
        #("flex-direction", "column"),
        #("background", p.surface),
        #("border-right", "1px solid " <> p.border),
        #("transition", "width 220ms ease"),
        // Children sometimes have absolute-positioned bits (unread badges)
        // that hang outside their bounding box, plus the inline list rows
        // are sized for the expanded state — clip everything that doesn't
        // fit so the collapsed 54px rail never spawns a horizontal scroll.
        #("overflow", "hidden"),
        #("min-width", "0"),
      ]),
    ],
    [
      brand_row(p, col, toggle),
      case col {
        True -> element.fragment([])
        False -> search_bar(p, search_value, noop, on_search_change, on_join)
      },
      rooms_list(
        p,
        rs,
        cur,
        col,
        dragging,
        drag_over,
        sel,
        on_delete,
        on_drag_start,
        on_drag_over,
        on_drag_leave,
        on_drop,
        on_drag_end,
      ),
      you_row(p, col),
    ],
  )
}

fn brand_row(p: Palette, collapsed: Bool, toggle: msg) -> Element(msg) {
  case collapsed {
    True ->
      // Collapsed: just the chevron, centered in the 54px rail. No logo.
      html.div(
        [
          ui.css([
            #("box-sizing", "border-box"),
            #("height", "60px"),
            #("flex-shrink", "0"),
            #("display", "flex"),
            #("align-items", "center"),
            #("justify-content", "center"),
            #("padding", "0"),
          ]),
        ],
        [collapse_button(p, collapsed, toggle)],
      )
    False ->
      // Expanded: logo + brand text on the left, chevron on the right.
      html.div(
        [
          ui.css([
            #("box-sizing", "border-box"),
            #("height", "60px"),
            #("flex-shrink", "0"),
            #("display", "flex"),
            #("align-items", "center"),
            #("padding", "0 12px 0 14px"),
            #("gap", "8px"),
          ]),
        ],
        [
          html.div(
            [
              ui.css([
                #("flex", "1"),
                #("display", "flex"),
                #("align-items", "center"),
                #("gap", "10px"),
                #("min-width", "0"),
              ]),
            ],
            [
              html.span(
                [ui.css([#("color", p.accent), #("display", "inline-flex")])],
                [logo(22)],
              ),
              html.span(
                [
                  ui.css([
                    #("font-weight", "600"),
                    #("font-size", "18.75px"),
                    #("letter-spacing", "-0.01em"),
                    #("color", p.text),
                  ]),
                ],
                [html.text("sunset")],
              ),
            ],
          ),
          collapse_button(p, collapsed, toggle),
        ],
      )
  }
}

fn collapse_button(p: Palette, collapsed: Bool, toggle: msg) -> Element(msg) {
  let path = case collapsed {
    True -> "M5 3l4 4-4 4"
    False -> "M9 3L5 7l4 4"
  }
  html.button(
    [
      ui.css([
        #("display", "inline-flex"),
        #("align-items", "center"),
        #("justify-content", "center"),
        #("width", "22px"),
        #("height", "22px"),
        #("border", "none"),
        #("background", "transparent"),
        #("color", p.text_faint),
        #("cursor", "pointer"),
        #("border-radius", "4px"),
        #("padding", "0"),
      ]),
      event.on_click(toggle),
      attribute.title(case collapsed {
        True -> "Expand rooms"
        False -> "Collapse rooms"
      }),
    ],
    [
      element.namespaced(
        "http://www.w3.org/2000/svg",
        "svg",
        [
          attribute.attribute("width", "14"),
          attribute.attribute("height", "14"),
          attribute.attribute("viewBox", "0 0 14 14"),
          attribute.attribute("fill", "none"),
        ],
        [
          element.namespaced(
            "http://www.w3.org/2000/svg",
            "path",
            [
              attribute.attribute("d", path),
              attribute.attribute("stroke", "currentColor"),
              attribute.attribute("stroke-width", "1.5"),
              attribute.attribute("stroke-linecap", "round"),
              attribute.attribute("stroke-linejoin", "round"),
            ],
            [],
          ),
        ],
      ),
    ],
  )
}

fn search_bar(
  p: Palette,
  value: String,
  noop: msg,
  on_change: fn(String) -> msg,
  on_join: fn(String) -> msg,
) -> Element(msg) {
  html.div(
    [
      ui.css([
        #("padding", "0 12px 8px 12px"),
        #("display", "flex"),
        #("gap", "6px"),
        #("align-items", "center"),
      ]),
    ],
    [
      html.input([
        attribute.attribute("data-testid", "rooms-search"),
        attribute.value(value),
        attribute.placeholder("Search or join…"),
        event.on_input(on_change),
        on_enter_with_value(noop, on_join),
        ui.css([
          #("flex", "1"),
          #("min-width", "0"),
          #("box-sizing", "border-box"),
          #("background", p.surface_alt),
          #("border", "1px solid " <> p.border_soft),
          #("border-radius", "6px"),
          #("padding", "6px 10px"),
          #("font-family", "inherit"),
          #("font-size", "15.625px"),
          #("color", p.text),
          #("outline", "none"),
        ]),
      ]),
      case value {
        "" -> element.fragment([])
        _ -> join_button(p, value, on_join)
      },
    ],
  )
}

fn join_button(
  p: Palette,
  value: String,
  on_join: fn(String) -> msg,
) -> Element(msg) {
  html.button(
    [
      attribute.title("Join " <> value),
      attribute.attribute("data-testid", "rooms-search-join"),
      event.on_click(on_join(value)),
      ui.css([
        #("display", "inline-flex"),
        #("align-items", "center"),
        #("justify-content", "center"),
        #("width", "28px"),
        #("height", "28px"),
        #("padding", "0"),
        #("border", "1px solid " <> p.accent_deep),
        #("background", p.accent),
        #("color", p.accent_ink),
        #("border-radius", "6px"),
        #("cursor", "pointer"),
        #("font-family", "inherit"),
        #("font-size", "14px"),
        #("font-weight", "600"),
      ]),
    ],
    [html.text("↵")],
  )
}

fn on_enter_with_value(
  noop: msg,
  on_join: fn(String) -> msg,
) -> attribute.Attribute(msg) {
  event.on("keydown", {
    use key <- decode.subfield(["key"], decode.string)
    use value <- decode.subfield(["target", "value"], decode.string)
    decode.success(case key {
      "Enter" -> on_join(value)
      _ -> noop
    })
  })
}

fn rooms_list(
  p: Palette,
  rs: List(Room),
  cur: RoomId,
  collapsed: Bool,
  dragging: Option(String),
  drag_over: Option(String),
  sel: fn(RoomId) -> msg,
  on_delete: fn(String) -> msg,
  on_drag_start: fn(String) -> msg,
  on_drag_over: fn(String) -> msg,
  on_drag_leave: fn(String) -> msg,
  on_drop: fn(String) -> msg,
  on_drag_end: msg,
) -> Element(msg) {
  let padding = case collapsed {
    True -> "0 0 12px 0"
    False -> "0 8px 12px 8px"
  }
  html.div(
    [
      ui.css([
        #("flex", "1 1 auto"),
        #("min-height", "0"),
        // The vertical scrollbar can appear when the list overflows; in
        // collapsed mode the rail is only 54px wide so a classic 15-ish
        // pixel scrollbar would leave no room for the 38px mini-buttons.
        // Force horizontal clipping so any spillover is hidden rather
        // than scrolled, and use a thin scrollbar so the visible width
        // doesn't shift much when content overflows.
        #("overflow-x", "hidden"),
        #("overflow-y", "auto"),
        #("scrollbar-width", "thin"),
        #("padding", padding),
        #("display", "flex"),
        #("flex-direction", "column"),
        #("gap", "1px"),
      ]),
    ],
    list.map(rs, fn(r) {
      case collapsed {
        True -> room_mini(p, r, cur, sel)
        False ->
          room_full(
            p,
            r,
            cur,
            dragging,
            drag_over,
            sel,
            on_delete,
            on_drag_start,
            on_drag_over,
            on_drag_leave,
            on_drop,
            on_drag_end,
          )
      }
    }),
  )
}

fn room_full(
  p: Palette,
  r: Room,
  cur: RoomId,
  dragging: Option(String),
  drag_over: Option(String),
  sel: fn(RoomId) -> msg,
  on_delete: fn(String) -> msg,
  on_drag_start: fn(String) -> msg,
  on_drag_over: fn(String) -> msg,
  on_drag_leave: fn(String) -> msg,
  on_drop: fn(String) -> msg,
  on_drag_end: msg,
) -> Element(msg) {
  let active = r.id == cur
  let bg = case active {
    True -> p.accent_soft
    False -> "transparent"
  }
  let is_dragging_self = case dragging {
    Some(name) -> name == r.name
    _ -> False
  }
  let is_drop_target = case dragging, drag_over {
    Some(src), Some(over) -> over == r.name && src != r.name
    _, _ -> False
  }
  let drop_indicator_color = case is_drop_target {
    True -> p.accent
    False -> "transparent"
  }
  let row_opacity = case is_dragging_self {
    True -> "0.4"
    False -> "1"
  }
  html.div(
    [
      attribute.class("room-row"),
      attribute.attribute("data-room-name", r.name),
      attribute.attribute("draggable", "true"),
      event.on("dragstart", decode.success(on_drag_start(r.name))),
      event.on("dragend", decode.success(on_drag_end)),
      event.prevent_default(event.on(
        "dragover",
        decode.success(on_drag_over(r.name)),
      )),
      event.on("dragleave", decode.success(on_drag_leave(r.name))),
      event.prevent_default(event.on(
        "drop",
        decode.success(on_drop(r.name)),
      )),
      ui.css([
        #("position", "relative"),
        #("display", "flex"),
        #("align-items", "center"),
        #("opacity", row_opacity),
        #("cursor", "grab"),
        // Top-edge drop indicator: highlights the row that the
        // dragged item will be inserted *above*.
        #("border-top", "2px solid " <> drop_indicator_color),
        #("margin-top", case is_drop_target {
          True -> "0"
          False -> "2px"
        }),
      ]),
    ],
    [
      html.button(
        [
          event.on_click(sel(r.id)),
          ui.css([
            #("flex", "1"),
            #("min-width", "0"),
            #("display", "flex"),
            #("align-items", "center"),
            #("gap", "10px"),
            #("padding", "8px 30px 8px 10px"),
            #("border", "none"),
            #("background", bg),
            #("border-radius", "6px"),
            #("cursor", "pointer"),
            #("text-align", "left"),
            #("font-family", "inherit"),
            #("color", p.text),
          ]),
        ],
        [
          conn_dot(p, r.status),
          html.div(
            [
              ui.css([
                #("flex", "1"),
                #("min-width", "0"),
                #("display", "flex"),
                #("flex-direction", "column"),
                #("gap", "2px"),
              ]),
            ],
            [
              html.span(
                [
                  ui.css([
                    #("font-weight", case active {
                      True -> "600"
                      False -> "500"
                    }),
                    #("font-size", "16.25px"),
                    #("white-space", "nowrap"),
                    #("overflow", "hidden"),
                    #("text-overflow", "ellipsis"),
                  ]),
                ],
                [html.text(r.name)],
              ),
              html.div(
                [
                  ui.css([
                    #("font-size", "14.375px"),
                    #("color", p.text_muted),
                    #("font-weight", "400"),
                    #("display", "flex"),
                    #("gap", "6px"),
                    #("flex-wrap", "wrap"),
                  ]),
                ],
                meta_line(p, r),
              ),
            ],
          ),
          case r.unread {
            0 -> element.fragment([])
            n -> unread_pill(p, n)
          },
        ],
      ),
      delete_button(p, r.name, on_delete),
    ],
  )
}

/// Small × button anchored to the right edge of each room row. The
/// button is hidden by default and revealed when the row is hovered
/// (CSS rule lives in shell.gleam's global_reset).
fn delete_button(
  p: Palette,
  name: String,
  on_delete: fn(String) -> msg,
) -> Element(msg) {
  html.button(
    [
      attribute.title("Remove " <> name),
      attribute.class("room-delete"),
      attribute.attribute("data-testid", "room-delete"),
      attribute.attribute("aria-label", "Remove " <> name),
      event.on_click(on_delete(name)),
      ui.css([
        #("position", "absolute"),
        #("top", "50%"),
        #("right", "8px"),
        #("transform", "translateY(-50%)"),
        #("width", "20px"),
        #("height", "20px"),
        #("display", "inline-flex"),
        #("align-items", "center"),
        #("justify-content", "center"),
        #("padding", "0"),
        #("border", "1px solid " <> p.border_soft),
        #("background", p.surface),
        #("color", p.text_muted),
        #("border-radius", "4px"),
        #("cursor", "pointer"),
        #("font-family", "inherit"),
        #("font-size", "13px"),
        #("line-height", "1"),
      ]),
    ],
    [html.text("×")],
  )
}

fn meta_line(p: Palette, r: Room) -> List(Element(msg)) {
  // Every span in the meta line is regular weight: the room name above
  // is the only bold element in this row.
  let online_total =
    html.span([ui.css([#("font-weight", "400")])], [
      html.text(
        int_to_string(r.online) <> "/" <> int_to_string(r.members) <> " online",
      ),
    ])

  let in_call_part = case r.in_call {
    0 -> element.fragment([])
    n ->
      html.span([ui.css([#("color", p.accent), #("font-weight", "400")])], [
        html.text("· " <> int_to_string(n) <> " in voice"),
      ])
  }

  let status_part = case r.status {
    Reconnecting ->
      html.span([ui.css([#("color", p.warn), #("font-weight", "400")])], [
        html.text("· reconnecting"),
      ])
    Offline ->
      html.span([ui.css([#("color", p.text_faint), #("font-weight", "400")])], [
        html.text("· offline"),
      ])
    Connected -> element.fragment([])
  }

  [online_total, in_call_part, status_part]
}

fn room_mini(
  p: Palette,
  r: Room,
  cur: RoomId,
  sel: fn(RoomId) -> msg,
) -> Element(msg) {
  let active = r.id == cur
  let bg = case active {
    True -> p.accent_soft
    False -> "transparent"
  }
  let dot = case r.status {
    Connected -> p.live
    Reconnecting -> p.warn
    Offline -> p.text_faint
  }
  html.button(
    [
      event.on_click(sel(r.id)),
      attribute.title(r.name),
      ui.css([
        #("position", "relative"),
        #("display", "flex"),
        #("align-items", "center"),
        #("justify-content", "center"),
        #("width", "38px"),
        #("height", "38px"),
        #("margin", "0 auto"),
        #("border", "none"),
        #("background", bg),
        #("border-radius", "8px"),
        #("cursor", "pointer"),
        #("color", p.text),
        #("font-family", "inherit"),
        #("font-weight", "600"),
        #("font-size", "16.25px"),
      ]),
    ],
    [
      html.span(
        [
          ui.css([
            #("position", "absolute"),
            #("top", "2px"),
            #("right", "2px"),
            #("width", "7px"),
            #("height", "7px"),
            #("border-radius", "999px"),
            #("background", dot),
          ]),
        ],
        [],
      ),
      html.span([], [html.text(string.uppercase(string.slice(r.name, 0, 1)))]),
      case r.unread {
        0 -> element.fragment([])
        n ->
          html.span(
            [
              ui.css([
                #("position", "absolute"),
                #("bottom", "-2px"),
                #("right", "-2px"),
                #("min-width", "16px"),
                #("height", "16px"),
                #("padding", "0 4px"),
                #("border-radius", "999px"),
                #("background", p.accent),
                #("color", p.accent_ink),
                #("font-size", "12.5px"),
                #("font-weight", "600"),
                #("display", "inline-flex"),
                #("align-items", "center"),
                #("justify-content", "center"),
              ]),
            ],
            [html.text(int_to_string(n))],
          )
      },
    ],
  )
}

fn conn_dot(p: Palette, s: ConnStatus) -> Element(msg) {
  let c = case s {
    Connected -> p.live
    Reconnecting -> p.warn
    Offline -> p.text_faint
  }
  html.span(
    [
      ui.css([
        #("width", "7px"),
        #("height", "7px"),
        #("border-radius", "999px"),
        #("background", c),
        #("display", "inline-block"),
        #("flex-shrink", "0"),
      ]),
    ],
    [],
  )
}

fn unread_pill(p: Palette, n: Int) -> Element(msg) {
  html.span(
    [
      ui.css([
        #("min-width", "18px"),
        #("padding", "0 6px"),
        #("height", "18px"),
        #("border-radius", "999px"),
        #("background", p.accent),
        #("color", p.accent_ink),
        #("font-size", "13.125px"),
        #("font-weight", "600"),
        #("display", "inline-flex"),
        #("align-items", "center"),
        #("justify-content", "center"),
      ]),
    ],
    [html.text(int_to_string(n))],
  )
}

fn you_row(p: Palette, collapsed: Bool) -> Element(msg) {
  // Pinned at the bottom of the rooms rail. The fixed 64px height +
  // border-top is shared by the channels-rail self-bar and the main
  // panel composer so all three column-bottom rows visually align.
  let padding = case collapsed {
    True -> "0"
    False -> "0 14px"
  }
  let justify = case collapsed {
    True -> "center"
    False -> "flex-start"
  }
  html.div(
    [
      ui.css([
        #("box-sizing", "border-box"),
        #("height", "64px"),
        #("flex-shrink", "0"),
        #("display", "flex"),
        #("align-items", "center"),
        #("justify-content", justify),
        #("gap", "8px"),
        #("padding", padding),
        #("border-top", "1px solid " <> p.border_soft),
      ]),
    ],
    [
      html.span(
        [
          ui.css([
            #("color", p.live),
            #("font-size", "12.5px"),
            #("line-height", "1"),
          ]),
        ],
        [html.text("●")],
      ),
      case collapsed {
        True -> element.fragment([])
        False ->
          html.span(
            [
              ui.css([
                #("flex", "1"),
                #("display", "flex"),
                #("align-items", "baseline"),
                #("gap", "6px"),
                #("min-width", "0"),
              ]),
            ],
            [
              html.span(
                [ui.css([#("font-weight", "500"), #("color", p.text)])],
                [html.text("you")],
              ),
              html.span(
                [
                  ui.css([
                    #("font-family", theme.font_mono),
                    #("font-size", "13.125px"),
                    #("color", p.text_faint),
                  ]),
                ],
                [html.text("8f3c…a2")],
              ),
            ],
          )
      },
    ],
  )
}

fn logo(size: Int) -> Element(msg) {
  let s = int_to_string(size)
  element.namespaced(
    "http://www.w3.org/2000/svg",
    "svg",
    [
      attribute.attribute("width", s),
      attribute.attribute("height", s),
      attribute.attribute("viewBox", "0 0 28 28"),
      attribute.attribute("fill", "none"),
    ],
    [
      element.namespaced(
        "http://www.w3.org/2000/svg",
        "circle",
        [
          attribute.attribute("cx", "14"),
          attribute.attribute("cy", "14"),
          attribute.attribute("r", "6.5"),
          attribute.attribute("fill", "currentColor"),
          attribute.attribute("opacity", "0.28"),
        ],
        [],
      ),
      element.namespaced(
        "http://www.w3.org/2000/svg",
        "circle",
        [
          attribute.attribute("cx", "14"),
          attribute.attribute("cy", "14"),
          attribute.attribute("r", "3.6"),
          attribute.attribute("fill", "currentColor"),
        ],
        [],
      ),
      element.namespaced(
        "http://www.w3.org/2000/svg",
        "line",
        [
          attribute.attribute("x1", "3"),
          attribute.attribute("y1", "20.5"),
          attribute.attribute("x2", "25"),
          attribute.attribute("y2", "20.5"),
          attribute.attribute("stroke", "currentColor"),
          attribute.attribute("stroke-width", "1.6"),
          attribute.attribute("stroke-linecap", "round"),
        ],
        [],
      ),
      element.namespaced(
        "http://www.w3.org/2000/svg",
        "line",
        [
          attribute.attribute("x1", "6"),
          attribute.attribute("y1", "24"),
          attribute.attribute("x2", "22"),
          attribute.attribute("y2", "24"),
          attribute.attribute("stroke", "currentColor"),
          attribute.attribute("stroke-width", "1.6"),
          attribute.attribute("stroke-linecap", "round"),
          attribute.attribute("opacity", "0.5"),
        ],
        [],
      ),
    ],
  )
}

fn int_to_string(n: Int) -> String {
  int.to_string(n)
}
