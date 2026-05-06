//// Channels rail (column 2): room header, text channels, voice
//// channel (with grouped live detail when peers are connected),
//// and bridge channels.

import gleam/dict.{type Dict}
import gleam/int
import gleam/list
import gleam/option.{type Option, None, Some}
import gleam/result
import lustre/attribute
import lustre/element.{type Element}
import lustre/element/html
import lustre/event
import sunset_web/domain.{
  type Channel, type ChannelId, type ConnStatus, type Member, type Relay,
  type Room, type Viewport, Connected, Desktop, MutedP, Offline, Phone,
  Reconnecting, TextChannel, Voice,
}
import sunset_web/sunset
import sunset_web/theme.{type Palette}
import sunset_web/ui
import sunset_web/views/relays as relays_view
import sunset_web/views/voice_meter

pub fn view(
  palette p: Palette,
  room r: Room,
  channels cs: List(Channel),
  members ms: List(Member),
  peer_levels peer_levels: Dict(String, Float),
  self_level self_level: Float,
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
  self_in_call self_in_call: Bool,
  self_muted self_muted: Bool,
  self_deafened self_deafened: Bool,
  relays relays: List(Relay),
  on_open_relay on_open_relay: fn(Float) -> msg,
) -> Element(msg) {
  let text_channels = list.filter(cs, fn(c) { c.kind == TextChannel })
  let voice_channels = list.filter(cs, fn(c) { c.kind == Voice })
  let in_call = list.filter(ms, fn(m) { m.in_call })

  // Always anchor the desktop self-controls bar to the channels rail's
  // first voice channel, regardless of whether anyone is in_call yet.
  // The bar's bottom seam needs to line up with the rooms rail's "you"
  // row and the main panel's composer (shell.spec.js's
  // "column-bottom rows share a top y-coordinate" check), and a user
  // who hasn't joined voice still wants to see the channel they can
  // join. Pre-cleanup the fixture's hardcoded in_call: 3 was what made
  // this branch fire for free; post-cleanup the rail derives in_call
  // from real peers, so we anchor by existence rather than activity.
  let active_voice =
    voice_channels
    |> list.first
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
                  peer_levels,
                  self_level,
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
            self_in_call,
            self_muted,
            self_deafened,
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
  peer_levels: Dict(String, Float),
  self_level: Float,
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
        peer_levels,
        self_level,
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
  peer_levels: Dict(String, Float),
  self_level: Float,
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
            voice_member_row(
              p,
              m,
              peer_levels,
              self_level,
              popover_open,
              on_open_voice_popover,
            )
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
  peer_levels: Dict(String, Float),
  self_level: Float,
  popover_open: Option(String),
  on_open_voice_popover: fn(String) -> msg,
) -> Element(msg) {
  // The voice subsystem keys peers by *full* pubkey hex throughout —
  // FFI peer node table, voice.peers state, voice.peer_levels, popover
  // identity, and per-peer volume RPCs all line up on this string.
  // Member.id is short_pubkey for display elsewhere (members rail,
  // message authoring), so this row derives the full hex on demand.
  let peer_key = sunset.bits_to_hex(m.pubkey)
  let muted = m.status == MutedP
  // Real audio level: 0..1, driven by the FFI's per-peer RMS smoother
  // (or the local mic level for self). When muted, force to 0 so the
  // bar matches the "muted" affordance instead of pretending audio is
  // flowing.
  let raw_level = case m.you {
    True -> self_level
    False ->
      dict.get(peer_levels, peer_key)
      |> result.unwrap(0.0)
  }
  let level = case muted {
    True -> 0.0
    False -> raw_level
  }
  let speaking = level >. voice_meter.speaking_threshold()
  let dot_color = case muted, speaking {
    True, _ -> p.text_faint
    False, True -> p.live
    False, False -> p.accent
  }
  let active = case popover_open {
    Some(open_id) -> open_id == peer_key
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
      attribute.attribute("data-peer-hex", peer_key),
      attribute.attribute(
        "data-voice-level",
        voice_meter.level_to_attribute(level),
      ),
      attribute.attribute("data-voice-speaking", case speaking {
        True -> "true"
        False -> "false"
      }),
      event.on_click(on_open_voice_popover(peer_key)),
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
        False -> waveform_meter(p, level)
      },
    ],
  )
}

/// 12-bar VU-meter visualisation of an audio level (0..1). Bar `i`
/// scales between `min_height` and `max_height` proportional to where
/// `level` falls along its slot — so bars light up left-to-right as
/// the level climbs, and the trailing bars fade out as it drops. Drives
/// off the smoothed FFI level so the user can see who is talking.
fn waveform_meter(p: Palette, level: Float) -> Element(msg) {
  let bars_count = 12
  html.span(
    [
      attribute.attribute("data-testid", "voice-waveform"),
      ui.css([
        #("display", "inline-flex"),
        #("align-items", "center"),
        #("gap", "1px"),
        #("height", "12px"),
      ]),
    ],
    list.index_map(list.repeat(Nil, bars_count), fn(_, i) {
      meter_bar(p, level, i, bars_count)
    }),
  )
}

fn meter_bar(p: Palette, level: Float, i: Int, n: Int) -> Element(msg) {
  // Treat each bar as covering its slot of the 0..1 range. A bar is
  // fully lit when `level >= (i+1)/n`, fully dark when `level <= i/n`,
  // and partially lit in between — same shape as a hardware VU meter.
  let slot_lo = int.to_float(i) /. int.to_float(n)
  let slot_hi = int.to_float(i + 1) /. int.to_float(n)
  let fill = case level <=. slot_lo {
    True -> 0.0
    False ->
      case level >=. slot_hi {
        True -> 1.0
        False -> { level -. slot_lo } /. { slot_hi -. slot_lo }
      }
  }
  // Map fill (0..1) into a height that's still a 1 px sliver at idle
  // so the meter is visible as a row of dots before audio arrives.
  let min_h = 1.5
  let max_h = 11.0
  let h = min_h +. fill *. { max_h -. min_h }
  let opacity = case fill <. 0.05 {
    True -> "0.3"
    False -> "0.95"
  }
  let color = case fill >. 0.0 {
    True -> p.accent
    False -> p.text_faint
  }
  html.span(
    [
      ui.css([
        #("display", "inline-block"),
        #("width", "2px"),
        #("border-radius", "1px"),
        #("background", color),
        #("opacity", opacity),
        #("height", voice_meter.float_to_px(h)),
      ]),
    ],
    [],
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
  self_in_call: Bool,
  self_muted: Bool,
  self_deafened: Bool,
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
