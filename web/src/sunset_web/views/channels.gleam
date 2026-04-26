//// Channels rail (column 2): room header, text channels, voice
//// channels (with grouped live detail for the active Lounge), and
//// bridge channels.

import gleam/int
import gleam/list
import lustre/attribute
import lustre/element.{type Element}
import lustre/element/html
import lustre/event
import sunset_web/domain.{
  type Channel, type ChannelId, type ConnStatus, type Member, type Room, Bridge,
  Connected, Minecraft, MutedP, Offline, Reconnecting, Speaking, TextChannel,
  Voice,
}
import sunset_web/theme.{type Palette}
import sunset_web/ui

pub fn view(
  palette p: Palette,
  room r: Room,
  channels cs: List(Channel),
  members ms: List(Member),
  current_channel cur: ChannelId,
  on_select_channel sel: fn(ChannelId) -> msg,
) -> Element(msg) {
  let text_channels = list.filter(cs, fn(c) { c.kind == TextChannel })
  let voice_channels = list.filter(cs, fn(c) { c.kind == Voice })
  let bridge_channels =
    list.filter(cs, fn(c) {
      case c.kind {
        Bridge(_) -> True
        _ -> False
      }
    })
  let in_call = list.filter(ms, fn(m) { m.in_call })

  html.aside(
    [
      ui.css([
        #("height", "100vh"),
        #("display", "flex"),
        #("flex-direction", "column"),
        #("background", p.surface_alt),
        #("border-right", "1px solid " <> p.border),
      ]),
    ],
    [
      room_header(p, r),
      html.div(
        [
          ui.css([
            #("flex", "1 1 auto"),
            #("overflow-y", "auto"),
            #("padding", "8px 8px 16px 8px"),
            #("display", "flex"),
            #("flex-direction", "column"),
            #("gap", "12px"),
          ]),
        ],
        [
          section(
            p,
            "Channels",
            list.map(text_channels, fn(c) { text_channel_row(p, c, cur, sel) }),
          ),
          section(
            p,
            "Voice",
            list.flatten([
              list.map(voice_channels, fn(c) { voice_block(p, c, in_call) }),
            ]),
          ),
          case bridge_channels {
            [] -> element.fragment([])
            _ ->
              section(
                p,
                "Bridges",
                list.map(bridge_channels, fn(c) { bridge_channel_row(p, c) }),
              )
          },
        ],
      ),
    ],
  )
}

fn room_header(p: Palette, r: Room) -> Element(msg) {
  html.div(
    [
      ui.css([
        #("padding", "14px 14px 10px 14px"),
        #("border-bottom", "1px solid " <> p.border_soft),
        #("display", "flex"),
        #("flex-direction", "column"),
        #("gap", "4px"),
        #("min-height", "48px"),
      ]),
    ],
    [
      html.div(
        [
          ui.css([
            #("display", "flex"),
            #("align-items", "center"),
            #("gap", "8px"),
            #("min-width", "0"),
          ]),
        ],
        [
          html.span(
            [
              ui.css([
                #("font-weight", "600"),
                #("font-size", "14.5px"),
                #("color", p.text),
                #("white-space", "nowrap"),
                #("overflow", "hidden"),
                #("text-overflow", "ellipsis"),
                #("flex", "1"),
                #("min-width", "0"),
              ]),
            ],
            [html.text(r.name)],
          ),
          conn_icon(p, r.status),
        ],
      ),
      html.div(
        [
          ui.css([
            #("font-size", "11.5px"),
            #("color", p.text_muted),
          ]),
        ],
        [
          html.text(
            int.to_string(r.online)
            <> " of "
            <> int.to_string(r.members)
            <> " online",
          ),
        ],
      ),
    ],
  )
}

fn section(p: Palette, title: String, rows: List(Element(msg))) -> Element(msg) {
  html.div(
    [
      ui.css([
        #("display", "flex"),
        #("flex-direction", "column"),
        #("gap", "1px"),
      ]),
    ],
    [
      html.div(
        [
          ui.css([
            #("padding", "4px 12px 6px 12px"),
            #("font-size", "10.5px"),
            #("font-weight", "600"),
            #("text-transform", "uppercase"),
            #("letter-spacing", "0.04em"),
            #("color", p.text_faint),
          ]),
        ],
        [html.text(title)],
      ),
      ..rows
    ],
  )
}

fn text_channel_row(
  p: Palette,
  c: Channel,
  cur: ChannelId,
  sel: fn(ChannelId) -> msg,
) -> Element(msg) {
  let active = c.id == cur
  let bg = case active {
    True -> p.accent_soft
    False -> "transparent"
  }
  let color = case active {
    True -> p.accent_deep
    False -> p.text
  }

  html.button(
    [
      event.on_click(sel(c.id)),
      ui.css([
        #("display", "flex"),
        #("align-items", "center"),
        #("gap", "8px"),
        #("padding", "6px 12px"),
        #("border", "none"),
        #("background", bg),
        #("border-radius", "6px"),
        #("cursor", "pointer"),
        #("font-family", "inherit"),
        #("font-size", "13px"),
        #("color", color),
        #("text-align", "left"),
      ]),
    ],
    [
      html.span([ui.css([#("color", p.text_faint)])], [html.text("#")]),
      html.span([ui.css([#("flex", "1")])], [html.text(c.name)]),
      case c.unread {
        0 -> element.fragment([])
        n -> unread_pill(p, n)
      },
    ],
  )
}

fn voice_block(
  p: Palette,
  c: Channel,
  in_call_members: List(Member),
) -> Element(msg) {
  let is_live = c.in_call > 0
  case is_live {
    False -> idle_voice_row(p, c)
    True -> live_voice_block(p, c, in_call_members)
  }
}

fn idle_voice_row(p: Palette, c: Channel) -> Element(msg) {
  html.div(
    [
      ui.css([
        #("display", "flex"),
        #("align-items", "center"),
        #("gap", "8px"),
        #("padding", "6px 12px"),
        #("font-size", "13px"),
        #("color", p.text),
        #("border-radius", "6px"),
      ]),
    ],
    [
      html.span([ui.css([#("color", p.text_faint)])], [html.text("◐")]),
      html.span([ui.css([#("flex", "1")])], [html.text(c.name)]),
      case c.in_call {
        0 -> element.fragment([])
        n ->
          html.span(
            [
              ui.css([
                #("font-size", "11px"),
                #("color", p.accent),
                #("font-weight", "600"),
              ]),
            ],
            [html.text(int.to_string(n) <> " live")],
          )
      },
    ],
  )
}

fn live_voice_block(p: Palette, c: Channel, ms: List(Member)) -> Element(msg) {
  html.div(
    [
      ui.css([
        #("background", p.accent_soft),
        #("border-radius", "6px"),
        #("padding-bottom", "4px"),
      ]),
    ],
    [
      html.div(
        [
          ui.css([
            #("display", "flex"),
            #("align-items", "center"),
            #("gap", "8px"),
            #("padding", "6px 12px"),
            #("font-size", "13px"),
            #("font-weight", "600"),
            #("color", p.accent_deep),
          ]),
        ],
        [
          html.span(
            [
              ui.css([
                #("width", "8px"),
                #("height", "8px"),
                #("border-radius", "999px"),
                #("background", p.live),
                #("display", "inline-block"),
                #("flex-shrink", "0"),
              ]),
            ],
            [],
          ),
          html.span([ui.css([#("flex", "1")])], [html.text(c.name)]),
        ],
      ),
      html.div(
        [
          ui.css([
            #("position", "relative"),
            #("padding", "2px 12px 4px 22px"),
            #("display", "flex"),
            #("flex-direction", "column"),
            #("gap", "2px"),
          ]),
        ],
        list.flatten([
          [connector_line(p)],
          list.map(ms, fn(m) { voice_member_row(p, m) }),
        ]),
      ),
      self_control_bar(p),
    ],
  )
}

fn connector_line(p: Palette) -> Element(msg) {
  html.span(
    [
      ui.css([
        #("position", "absolute"),
        #("left", "16px"),
        #("top", "0"),
        #("bottom", "0"),
        #("width", "2px"),
        #("background", p.accent),
        #("opacity", "0.35"),
        #("border-radius", "1px"),
      ]),
    ],
    [],
  )
}

fn voice_member_row(p: Palette, m: Member) -> Element(msg) {
  let dot_color = case m.status {
    MutedP -> p.text_faint
    Speaking -> p.live
    _ -> p.accent
  }
  let speaking = m.status == Speaking
  let muted = m.status == MutedP
  html.div(
    [
      ui.css([
        #("display", "flex"),
        #("align-items", "center"),
        #("gap", "8px"),
        #("padding", "4px 6px"),
        #("border-radius", "4px"),
        #("font-size", "12.5px"),
      ]),
    ],
    [
      html.span(
        [
          ui.css([
            #("width", "6px"),
            #("height", "6px"),
            #("border-radius", "999px"),
            #("background", dot_color),
            #("flex-shrink", "0"),
          ]),
        ],
        [],
      ),
      html.span(
        [
          ui.css([
            #("font-weight", case speaking {
              True -> "600"
              False -> "400"
            }),
            #("color", case speaking {
              True -> p.text
              False -> p.text_muted
            }),
          ]),
        ],
        list.flatten([
          [html.text(m.name)],
          case m.you {
            True -> [you_tag(p)]
            False -> []
          },
        ]),
      ),
      html.span([ui.css([#("flex", "1")])], []),
      case muted {
        True ->
          html.span(
            [
              ui.css([
                #("font-size", "10.5px"),
                #("color", p.text_faint),
                #("font-style", "italic"),
              ]),
            ],
            [html.text("muted")],
          )
        False -> waveform_placeholder(p, speaking)
      },
    ],
  )
}

fn waveform_placeholder(p: Palette, speaking: Bool) -> Element(msg) {
  // V1: a flat row of 12 thin bars; the live animation is deferred.
  let color = case speaking {
    True -> p.accent
    False -> p.text_faint
  }
  let bars =
    [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11]
    |> list.map(fn(_) {
      html.span(
        [
          ui.css([
            #("display", "inline-block"),
            #("width", "2px"),
            #("height", case speaking {
              True -> "10px"
              False -> "2px"
            }),
            #("background", color),
            #("border-radius", "1px"),
            #("opacity", case speaking {
              True -> "0.85"
              False -> "0.4"
            }),
          ]),
        ],
        [],
      )
    })
  html.span(
    [
      ui.css([
        #("display", "inline-flex"),
        #("align-items", "center"),
        #("gap", "1px"),
      ]),
    ],
    bars,
  )
}

fn you_tag(p: Palette) -> Element(msg) {
  html.span(
    [
      ui.css([
        #("margin-left", "6px"),
        #("padding", "1px 4px"),
        #("border-radius", "3px"),
        #("background", p.surface),
        #("color", p.text_faint),
        #("font-size", "9.5px"),
        #("font-weight", "500"),
        #("letter-spacing", "0.02em"),
        #("text-transform", "uppercase"),
      ]),
    ],
    [html.text("you")],
  )
}

fn self_control_bar(p: Palette) -> Element(msg) {
  html.div(
    [
      ui.css([
        #("display", "flex"),
        #("align-items", "center"),
        #("gap", "6px"),
        #("padding", "6px 8px"),
        #("border-top", "1px solid " <> p.accent),
        #("margin-top", "4px"),
      ]),
    ],
    [
      self_btn(p, "Mic", False),
      self_btn(p, "Headphones", False),
      html.span([ui.css([#("flex", "1")])], []),
      leave_btn(p),
    ],
  )
}

fn self_btn(p: Palette, label: String, danger: Bool) -> Element(msg) {
  let bg = case danger {
    True -> p.warn_soft
    False -> p.surface
  }
  let color = case danger {
    True -> p.warn
    False -> p.text_muted
  }
  html.button(
    [
      attribute.title(label),
      ui.css([
        #("display", "inline-flex"),
        #("align-items", "center"),
        #("justify-content", "center"),
        #("gap", "4px"),
        #("padding", "4px 8px"),
        #("border", "1px solid " <> p.border_soft),
        #("background", bg),
        #("color", color),
        #("border-radius", "4px"),
        #("cursor", "pointer"),
        #("font-size", "11px"),
        #("font-family", "inherit"),
      ]),
    ],
    [html.text(label)],
  )
}

fn leave_btn(p: Palette) -> Element(msg) {
  html.button(
    [
      attribute.title("Leave call"),
      ui.css([
        #("display", "inline-flex"),
        #("align-items", "center"),
        #("gap", "4px"),
        #("padding", "4px 8px"),
        #("border", "1px solid " <> p.warn_soft),
        #("background", "transparent"),
        #("color", p.warn),
        #("border-radius", "4px"),
        #("cursor", "pointer"),
        #("font-size", "11px"),
        #("font-family", "inherit"),
      ]),
    ],
    [html.text("Leave")],
  )
}

fn bridge_channel_row(p: Palette, c: Channel) -> Element(msg) {
  let icon = case c.kind {
    Bridge(Minecraft) -> "⛏"
    _ -> "↗"
  }
  html.div(
    [
      ui.css([
        #("display", "flex"),
        #("align-items", "center"),
        #("gap", "8px"),
        #("padding", "6px 12px"),
        #("font-size", "13px"),
        #("color", p.text_muted),
      ]),
    ],
    [
      html.span([ui.css([#("color", p.accent), #("opacity", "0.7")])], [
        html.text(icon),
      ]),
      html.span([ui.css([#("flex", "1")])], [html.text(c.name)]),
    ],
  )
}

fn conn_icon(p: Palette, status: ConnStatus) -> Element(msg) {
  let c = case status {
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
        #("font-size", "10.5px"),
        #("font-weight", "600"),
        #("display", "inline-flex"),
        #("align-items", "center"),
        #("justify-content", "center"),
      ]),
    ],
    [html.text(int.to_string(n))],
  )
}
