//// Channels rail (column 2): room header, text channels, voice
//// channels (with grouped live detail for the active Lounge), and
//// bridge channels.

import gleam/int
import gleam/list
import gleam/option.{type Option, None, Some}
import lustre/attribute
import lustre/element.{type Element}
import lustre/element/html
import lustre/event
import sunset_web/domain.{
  type Channel, type ChannelId, type ConnStatus, type Member, type Relay,
  type Room, type Viewport, Connected, Desktop, MemberId, MutedP, Offline, Phone,
  Reconnecting, Speaking, TextChannel, Voice,
}
import sunset_web/theme.{type Palette}
import sunset_web/ui
import sunset_web/views/relays as relays_view

pub fn view(
  palette p: Palette,
  room r: Room,
  channels cs: List(Channel),
  members ms: List(Member),
  current_channel cur: ChannelId,
  voice_popover_open voice_popover_open: Option(String),
  on_select_channel sel: fn(ChannelId) -> msg,
  on_open_voice_popover on_open_voice_popover: fn(String) -> msg,
  viewport viewport: Viewport,
  on_open_rooms on_open_rooms: msg,
  on_join_voice on_join_voice: msg,
  on_leave_voice on_leave_voice: msg,
  on_mute_self on_mute_self: msg,
  on_deafen_self on_deafen_self: msg,
  on_toggle_denoise on_toggle_denoise: msg,
  self_in_call self_in_call: Bool,
  self_muted self_muted: Bool,
  self_deafened self_deafened: Bool,
  denoise_on denoise_on: Bool,
  relays relays: List(Relay),
  on_open_relay on_open_relay: fn(Float) -> msg,
) -> Element(msg) {
  let text_channels = list.filter(cs, fn(c) { c.kind == TextChannel })
  let voice_channels = list.filter(cs, fn(c) { c.kind == Voice })
  let in_call = list.filter(ms, fn(m) { m.in_call })

  let active_voice =
    list.find(voice_channels, fn(c) { c.in_call > 0 })
    |> result_to_option
  // `height: 100%` resolves correctly for both layouts: the drawer's
  // safe-area-padded content box on phone, and the desktop grid row
  // (which is sized to 100dvh by shell.desktop_view's
  // `grid-template-rows`). A bare 100dvh would overflow the drawer's
  // clipping box on phone PWA mode and cover the iOS home indicator.
  html.aside(
    [
      ui.css([
        #("height", "100%"),
        #("min-height", "0"),
        #("display", "flex"),
        #("flex-direction", "column"),
        #("background", p.surface_alt),
        #("border-right", "1px solid " <> p.border),
        #("overflow", "hidden"),
        #("min-width", "0"),
      ]),
    ],
    [
      room_header(p, r, viewport, on_open_rooms),
      html.div(
        [
          ui.css([
            #("flex", "1 1 auto"),
            #("min-height", "0"),
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
              list.map(voice_channels, fn(c) {
                voice_block(
                  p,
                  c,
                  in_call,
                  voice_popover_open,
                  on_open_voice_popover,
                  on_join_voice,
                  on_leave_voice,
                  self_in_call,
                )
              }),
            ]),
          ),
          relays_view.rail_section(
            palette: p,
            relays: relays,
            on_open: on_open_relay,
          ),
        ],
      ),
      case viewport, active_voice {
        Desktop, Some(c) ->
          self_control_bar(
            p,
            c.name,
            on_leave_voice,
            on_join_voice,
            on_mute_self,
            on_deafen_self,
            on_toggle_denoise,
            self_in_call,
            self_muted,
            self_deafened,
            denoise_on,
          )
        _, _ -> element.fragment([])
      },
    ],
  )
}

fn result_to_option(r: Result(a, b)) -> Option(a) {
  case r {
    Ok(v) -> Some(v)
    Error(_) -> None
  }
}

fn room_header(
  p: Palette,
  r: Room,
  viewport: Viewport,
  on_open_rooms: msg,
) -> Element(msg) {
  // Single-line title row, vertically centred in the 60px header. The
  // "X online" subtitle was dropped — the same count is already visible
  // in the members rail, and removing it gives the title room to breathe.
  // On phone the title becomes a tappable button that opens the rooms drawer.
  let title_el = case viewport {
    Phone ->
      html.button(
        [
          attribute.attribute("data-testid", "channels-room-title"),
          attribute.title("Switch room"),
          attribute.attribute("aria-label", "Switch room"),
          event.on_click(on_open_rooms),
          ui.css([
            #("flex", "1"),
            #("min-width", "0"),
            #("display", "flex"),
            #("align-items", "center"),
            #("gap", "6px"),
            #("padding", "0"),
            #("border", "none"),
            #("background", "transparent"),
            #("color", p.text),
            #("font-family", "inherit"),
            #("font-weight", "600"),
            #("font-size", "18.75px"),
            #("text-align", "left"),
            #("cursor", "pointer"),
          ]),
        ],
        [title_text(r), conn_icon(p, r.status), chevron_right(p)],
      )
    Desktop ->
      html.span(
        [
          ui.css([
            #("font-weight", "600"),
            #("font-size", "18.75px"),
            #("color", p.text),
            #("white-space", "nowrap"),
            #("overflow", "hidden"),
            #("text-overflow", "ellipsis"),
            #("flex", "1"),
            #("min-width", "0"),
          ]),
        ],
        [html.text(r.name)],
      )
  }
  html.div(
    [
      ui.css([
        #("box-sizing", "border-box"),
        #("height", "60px"),
        #("flex-shrink", "0"),
        #("padding", "0 16px"),
        #("border-bottom", "1px solid " <> p.border_soft),
        #("display", "flex"),
        #("align-items", "center"),
        #("gap", "8px"),
        #("min-width", "0"),
      ]),
    ],
    case viewport {
      Phone -> [title_el]
      Desktop -> [title_el, conn_icon(p, r.status)]
    },
  )
}

fn title_text(r: Room) -> Element(msg) {
  html.span(
    [
      ui.css([
        #("white-space", "nowrap"),
        #("overflow", "hidden"),
        #("text-overflow", "ellipsis"),
        #("min-width", "0"),
      ]),
    ],
    [html.text(r.name)],
  )
}

fn chevron_right(p: Palette) -> Element(msg) {
  element.namespaced(
    "http://www.w3.org/2000/svg",
    "svg",
    [
      attribute.attribute("width", "16"),
      attribute.attribute("height", "16"),
      attribute.attribute("viewBox", "0 0 16 16"),
      attribute.attribute("fill", "none"),
    ],
    [
      element.namespaced(
        "http://www.w3.org/2000/svg",
        "path",
        [
          attribute.attribute("d", "M6 4l4 4-4 4"),
          attribute.attribute("stroke", p.text_faint),
          attribute.attribute("stroke-width", "1.5"),
          attribute.attribute("stroke-linecap", "round"),
          attribute.attribute("stroke-linejoin", "round"),
        ],
        [],
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
            #("font-size", "13.125px"),
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
        #("font-size", "16.25px"),
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
  popover_open: Option(String),
  on_open_voice_popover: fn(String) -> msg,
  on_join: msg,
  on_leave: msg,
  self_in_call: Bool,
) -> Element(msg) {
  let is_live = c.in_call > 0
  case is_live {
    False -> idle_voice_row(p, c, on_join)
    True ->
      live_voice_block(
        p,
        c,
        in_call_members,
        popover_open,
        on_open_voice_popover,
        on_join,
        on_leave,
        self_in_call,
      )
  }
}

fn idle_voice_row(p: Palette, c: Channel, on_join: msg) -> Element(msg) {
  html.button(
    [
      attribute.attribute("data-testid", "voice-channel-row"),
      attribute.attribute("aria-label", "Join " <> c.name),
      event.on_click(on_join),
      ui.css([
        #("display", "flex"),
        #("align-items", "center"),
        #("gap", "8px"),
        #("padding", "6px 12px"),
        #("font-size", "16.25px"),
        #("color", p.text),
        #("border-radius", "6px"),
        #("border", "none"),
        #("background", "transparent"),
        #("font-family", "inherit"),
        #("text-align", "left"),
        #("cursor", "pointer"),
        #("width", "100%"),
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
                #("font-size", "13.75px"),
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

fn live_voice_block(
  p: Palette,
  c: Channel,
  ms: List(Member),
  popover_open: Option(String),
  on_open_voice_popover: fn(String) -> msg,
  on_join: msg,
  on_leave: msg,
  self_in_call: Bool,
) -> Element(msg) {
  let toggle_msg = case self_in_call {
    True -> on_leave
    False -> on_join
  }
  html.div(
    [
      ui.css([
        #("background", p.accent_soft),
        #("border-radius", "6px"),
        #("padding-bottom", "10px"),
      ]),
    ],
    [
      html.button(
        [
          attribute.attribute("data-testid", "voice-channel-row"),
          attribute.attribute("aria-label", case self_in_call {
            True -> "Leave " <> c.name
            False -> "Join " <> c.name
          }),
          event.on_click(toggle_msg),
          ui.css([
            #("display", "flex"),
            #("align-items", "center"),
            #("gap", "8px"),
            #("padding", "6px 12px"),
            #("font-size", "16.25px"),
            #("font-weight", "600"),
            #("color", p.accent_deep),
            #("border", "none"),
            #("background", "transparent"),
            #("font-family", "inherit"),
            #("text-align", "left"),
            #("cursor", "pointer"),
            #("width", "100%"),
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
            #("padding", "2px 12px 8px 22px"),
            #("display", "flex"),
            #("flex-direction", "column"),
            #("gap", "2px"),
          ]),
        ],
        list.flatten([
          [connector_line(p)],
          list.map(ms, fn(m) {
            voice_member_row(p, m, popover_open, on_open_voice_popover)
          }),
        ]),
      ),
    ],
  )
}

fn connector_line(p: Palette) -> Element(msg) {
  // Bound to the inset of the members container's padding so the line
  // only spans the rows themselves, not the surrounding whitespace at
  // the top and bottom of the light-blue block.
  html.span(
    [
      ui.css([
        #("position", "absolute"),
        #("left", "16px"),
        #("top", "4px"),
        #("bottom", "10px"),
        #("width", "2px"),
        #("background", p.accent),
        #("opacity", "0.35"),
        #("border-radius", "1px"),
      ]),
    ],
    [],
  )
}

fn voice_member_row(
  p: Palette,
  m: Member,
  popover_open: Option(String),
  on_open_voice_popover: fn(String) -> msg,
) -> Element(msg) {
  let MemberId(id_str) = m.id
  let dot_color = case m.status {
    MutedP -> p.text_faint
    Speaking -> p.live
    _ -> p.accent
  }
  let speaking = m.status == Speaking
  let muted = m.status == MutedP
  let active = case popover_open {
    Some(open_id) -> open_id == id_str
    None -> False
  }
  let bg = case active {
    True -> p.surface
    False -> "transparent"
  }
  html.button(
    [
      attribute.attribute("data-testid", "voice-member"),
      attribute.attribute("data-voice-name", m.name),
      event.on_click(on_open_voice_popover(id_str)),
      ui.css([
        #("display", "flex"),
        #("align-items", "center"),
        #("gap", "8px"),
        #("padding", "4px 6px"),
        #("border-radius", "4px"),
        #("border", "none"),
        #("background", bg),
        #("color", p.text),
        #("text-align", "left"),
        #("font-family", "inherit"),
        #("font-size", "15.625px"),
        #("cursor", "pointer"),
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
                #("font-size", "13.125px"),
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
        #("font-size", "11.875px"),
        #("font-weight", "500"),
        #("letter-spacing", "0.02em"),
        #("text-transform", "uppercase"),
      ]),
    ],
    [html.text("you")],
  )
}

/// Self-controls bar — pinned to the bottom of the channels column when
/// the user is in a call (or there is an active voice channel to join).
/// Shows what voice channel they're connected to on the left, with three
/// small icon-only buttons (mic / headphones / leave) on the right.
fn self_control_bar(
  p: Palette,
  channel_name: String,
  on_leave: msg,
  on_join: msg,
  on_mute: msg,
  on_deafen: msg,
  on_toggle_denoise: msg,
  self_in_call: Bool,
  self_muted: Bool,
  self_deafened: Bool,
  denoise_on: Bool,
) -> Element(msg) {
  let _ = on_join
  // Fixed 64px height with a 1px border-top so this row aligns visually
  // with the rooms-rail you_row and the main-panel composer (their
  // top borders sit on the same y-coordinate across the seam).
  html.div(
    [
      ui.css([
        #("box-sizing", "border-box"),
        #("height", "64px"),
        #("flex-shrink", "0"),
        #("display", "flex"),
        #("align-items", "center"),
        #("gap", "8px"),
        #("padding", "0 12px"),
        #("background", p.surface),
        #("border-top", "1px solid " <> p.border_soft),
      ]),
    ],
    [
      html.div(
        [
          ui.css([
            #("display", "flex"),
            #("flex-direction", "column"),
            #("flex", "1"),
            #("min-width", "0"),
          ]),
        ],
        [
          html.span(
            [
              ui.css([
                #("font-size", "13.125px"),
                #("text-transform", "uppercase"),
                #("letter-spacing", "0.06em"),
                #("color", p.text_faint),
                #("font-weight", "600"),
              ]),
            ],
            [html.text("Connected")],
          ),
          html.span(
            [
              ui.css([
                #("font-size", "15.625px"),
                #("color", p.text),
                #("font-weight", "600"),
                #("display", "flex"),
                #("align-items", "center"),
                #("gap", "6px"),
                #("margin-top", "1px"),
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
                    #("flex-shrink", "0"),
                  ]),
                ],
                [],
              ),
              html.text(channel_name),
            ],
          ),
        ],
      ),
      self_btn(
        p,
        case self_muted {
          True -> "Unmute mic"
          False -> "Mute mic"
        },
        mic_icon(),
        self_muted,
        Some(on_mute),
      ),
      self_btn(
        p,
        case self_deafened {
          True -> "Undeafen"
          False -> "Deafen"
        },
        headphones_icon(),
        self_deafened,
        Some(on_deafen),
      ),
      // Denoise toggle. Defaults to on; the warn highlight fires when
      // the user has explicitly turned RNNoise off (i.e. opted into
      // raw audio), so it sits visually beside mute / deafen as another
      // "you're in a non-default state" indicator.
      self_btn_with_testid(
        p,
        case denoise_on {
          True -> "Disable noise reduction"
          False -> "Enable noise reduction"
        },
        denoise_icon(denoise_on),
        !denoise_on,
        Some(on_toggle_denoise),
        "voice-denoise-toggle",
      ),
      case self_in_call {
        True -> leave_btn(p, on_leave)
        False -> element.fragment([])
      },
    ],
  )
}

fn self_btn(
  p: Palette,
  title: String,
  icon: Element(msg),
  active: Bool,
  on_click: Option(msg),
) -> Element(msg) {
  self_btn_with_testid(p, title, icon, active, on_click, "")
}

fn self_btn_with_testid(
  p: Palette,
  title: String,
  icon: Element(msg),
  active: Bool,
  on_click: Option(msg),
  testid: String,
) -> Element(msg) {
  let bg = case active {
    True -> p.warn_soft
    False -> p.surface_alt
  }
  let color = case active {
    True -> p.warn
    False -> p.text
  }
  let click_attr = case on_click {
    Some(msg) -> [event.on_click(msg)]
    None -> []
  }
  let testid_attr = case testid {
    "" -> []
    id -> [
      attribute.attribute("data-testid", id),
      attribute.attribute("aria-pressed", case active {
        True -> "true"
        False -> "false"
      }),
    ]
  }
  html.button(
    list.flatten([
      [
        attribute.title(title),
        ui.css([
          #("width", "32px"),
          #("height", "32px"),
          #("display", "inline-flex"),
          #("align-items", "center"),
          #("justify-content", "center"),
          #("padding", "0"),
          #("border", "1px solid " <> p.border_soft),
          #("background", bg),
          #("color", color),
          #("border-radius", "6px"),
          #("cursor", "pointer"),
          #("font-family", "inherit"),
          #("flex-shrink", "0"),
        ]),
      ],
      click_attr,
      testid_attr,
    ]),
    [icon],
  )
}

fn leave_btn(p: Palette, on_click: msg) -> Element(msg) {
  let _ = p
  html.button(
    [
      attribute.title("Leave call"),
      attribute.attribute("data-testid", "voice-leave"),
      event.on_click(on_click),
      ui.css([
        #("width", "32px"),
        #("height", "32px"),
        #("display", "inline-flex"),
        #("align-items", "center"),
        #("justify-content", "center"),
        #("padding", "0"),
        #("border", "none"),
        #("background", "#a8242c"),
        #("color", "#ffffff"),
        #("border-radius", "6px"),
        #("cursor", "pointer"),
        #("font-family", "inherit"),
        #("flex-shrink", "0"),
      ]),
    ],
    [phone_hangup_icon()],
  )
}

fn mic_icon() -> Element(msg) {
  element.namespaced(
    "http://www.w3.org/2000/svg",
    "svg",
    [
      attribute.attribute("width", "14"),
      attribute.attribute("height", "14"),
      attribute.attribute("viewBox", "0 0 16 16"),
      attribute.attribute("fill", "none"),
    ],
    [
      element.namespaced(
        "http://www.w3.org/2000/svg",
        "rect",
        [
          attribute.attribute("x", "6"),
          attribute.attribute("y", "2.5"),
          attribute.attribute("width", "4"),
          attribute.attribute("height", "8"),
          attribute.attribute("rx", "2"),
          attribute.attribute("stroke", "currentColor"),
          attribute.attribute("stroke-width", "1.4"),
        ],
        [],
      ),
      element.namespaced(
        "http://www.w3.org/2000/svg",
        "path",
        [
          attribute.attribute("d", "M3.5 8a4.5 4.5 0 009 0M8 12.5V14"),
          attribute.attribute("stroke", "currentColor"),
          attribute.attribute("stroke-width", "1.4"),
          attribute.attribute("stroke-linecap", "round"),
        ],
        [],
      ),
    ],
  )
}

fn headphones_icon() -> Element(msg) {
  element.namespaced(
    "http://www.w3.org/2000/svg",
    "svg",
    [
      attribute.attribute("width", "14"),
      attribute.attribute("height", "14"),
      attribute.attribute("viewBox", "0 0 16 16"),
      attribute.attribute("fill", "none"),
    ],
    [
      element.namespaced(
        "http://www.w3.org/2000/svg",
        "path",
        [
          attribute.attribute("d", "M3 9V7a5 5 0 0110 0v2"),
          attribute.attribute("stroke", "currentColor"),
          attribute.attribute("stroke-width", "1.4"),
          attribute.attribute("stroke-linecap", "round"),
        ],
        [],
      ),
      element.namespaced(
        "http://www.w3.org/2000/svg",
        "rect",
        [
          attribute.attribute("x", "2.5"),
          attribute.attribute("y", "9"),
          attribute.attribute("width", "3"),
          attribute.attribute("height", "4"),
          attribute.attribute("rx", "1"),
          attribute.attribute("stroke", "currentColor"),
          attribute.attribute("stroke-width", "1.3"),
        ],
        [],
      ),
      element.namespaced(
        "http://www.w3.org/2000/svg",
        "rect",
        [
          attribute.attribute("x", "10.5"),
          attribute.attribute("y", "9"),
          attribute.attribute("width", "3"),
          attribute.attribute("height", "4"),
          attribute.attribute("rx", "1"),
          attribute.attribute("stroke", "currentColor"),
          attribute.attribute("stroke-width", "1.3"),
        ],
        [],
      ),
    ],
  )
}

/// Denoise / "noise reduction" icon: a sparkle (RNNoise is ML, sparkles
/// read as "smart filtering" in product UI), with a diagonal slash when
/// the toggle is off so the disabled state reads at a glance.
fn denoise_icon(on: Bool) -> Element(msg) {
  let sparkle =
    element.namespaced(
      "http://www.w3.org/2000/svg",
      "path",
      [
        attribute.attribute(
          "d",
          "M8 2.5l1.1 3.4 3.4 1.1-3.4 1.1L8 11.5l-1.1-3.4L3.5 7l3.4-1.1L8 2.5z",
        ),
        attribute.attribute("fill", "currentColor"),
        attribute.attribute("fill-opacity", "0.85"),
      ],
      [],
    )
  let small_sparkle =
    element.namespaced(
      "http://www.w3.org/2000/svg",
      "path",
      [
        attribute.attribute(
          "d",
          "M12.5 11l.5 1.5L14.5 13l-1.5.5L12.5 15l-.5-1.5L10.5 13l1.5-.5L12.5 11z",
        ),
        attribute.attribute("fill", "currentColor"),
        attribute.attribute("fill-opacity", "0.65"),
      ],
      [],
    )
  let slash = case on {
    True -> element.fragment([])
    False ->
      element.namespaced(
        "http://www.w3.org/2000/svg",
        "line",
        [
          attribute.attribute("x1", "2"),
          attribute.attribute("y1", "14"),
          attribute.attribute("x2", "14"),
          attribute.attribute("y2", "2"),
          attribute.attribute("stroke", "currentColor"),
          attribute.attribute("stroke-width", "1.6"),
          attribute.attribute("stroke-linecap", "round"),
        ],
        [],
      )
  }
  element.namespaced(
    "http://www.w3.org/2000/svg",
    "svg",
    [
      attribute.attribute("width", "14"),
      attribute.attribute("height", "14"),
      attribute.attribute("viewBox", "0 0 16 16"),
      attribute.attribute("fill", "none"),
    ],
    [sparkle, small_sparkle, slash],
  )
}

fn phone_hangup_icon() -> Element(msg) {
  element.namespaced(
    "http://www.w3.org/2000/svg",
    "svg",
    [
      attribute.attribute("width", "14"),
      attribute.attribute("height", "14"),
      attribute.attribute("viewBox", "0 0 16 16"),
      attribute.attribute("fill", "none"),
    ],
    [
      element.namespaced(
        "http://www.w3.org/2000/svg",
        "path",
        [
          attribute.attribute(
            "d",
            "M3.2 9.6c-.7-.7-.7-1.9 0-2.6 2.65-2.65 6.95-2.65 9.6 0 .7.7.7 1.9 0 2.6l-1.1 1.1c-.4.4-1 .4-1.4 0L9.4 9.8c-.4-.4-.4-1 0-1.4l.5-.5a4 4 0 00-3.8 0l.5.5c.4.4.4 1 0 1.4L5.7 10.7c-.4.4-1 .4-1.4 0L3.2 9.6z",
          ),
          attribute.attribute("fill", "currentColor"),
        ],
        [],
      ),
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
        #("font-size", "13.125px"),
        #("font-weight", "600"),
        #("display", "inline-flex"),
        #("align-items", "center"),
        #("justify-content", "center"),
      ]),
    ],
    [html.text(int.to_string(n))],
  )
}
