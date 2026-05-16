//// Channels rail (column 2): room header, text channels, voice
//// channel (with grouped live detail when peers are connected),
//// and bridge channels.
////
//// The in-call self-controls (mute / deafen / leave) live in the
//// voice minibar at the top of the chat panel, not in this rail —
//// see `views/voice_minibar.gleam`. Per-peer settings (volume,
//// denoise, send quality, mute-for-me) live in the voice popover
//// that opens when the user taps a member row here.

import gleam/dict.{type Dict}
import gleam/dynamic/decode
import gleam/int
import gleam/list
import gleam/option.{type Option, None, Some}
import gleam/result
import gleam/string
import lustre/attribute
import lustre/element.{type Element}
import lustre/element/html
import lustre/event
import sunset_web/domain.{
  type Channel, type ChannelId, type Member, type Room, type Viewport,
  type VoicePeerStateUI, Desktop, MutedP, Phone, TextChannel, Voice,
}
import sunset_web/sunset
import sunset_web/theme.{type Palette}
import sunset_web/ui
import sunset_web/views/voice_meter

pub fn view(
  palette p: Palette,
  room r: Room,
  channels cs: List(Channel),
  members ms: List(Member),
  voice_peers voice_peers: Dict(String, VoicePeerStateUI),
  peer_levels peer_levels: Dict(String, Float),
  self_level self_level: Float,
  current_channel cur: ChannelId,
  voice_popover_open voice_popover_open: Option(String),
  on_select_channel sel: fn(ChannelId) -> msg,
  on_new_channel on_new_channel: fn(String) -> msg,
  noop noop: msg,
  on_open_voice_popover on_open_voice_popover: fn(String) -> msg,
  viewport viewport: Viewport,
  on_open_rooms on_open_rooms: msg,
  on_join_voice on_join_voice: msg,
  on_leave_voice on_leave_voice: msg,
  self_in_call self_in_call: Bool,
) -> Element(msg) {
  let text_channels = list.filter(cs, fn(c) { c.kind == TextChannel })
  let voice_channels = list.filter(cs, fn(c) { c.kind == Voice })
  let in_call = list.filter(ms, fn(m) { m.in_call })

  // `height: 100%` resolves correctly for both layouts: the drawer's
  // safe-area-padded content box on phone, and the desktop grid row
  // (which is sized to 100dvh by shell.desktop_view's
  // `grid-template-rows`). A bare 100dvh would overflow the drawer's
  // clipping box on phone PWA mode and cover the iOS home indicator.
  html.aside(
    [
      attribute.attribute("data-testid", "channels-rail"),
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
            list.append(
              list.map(text_channels, fn(c) { text_channel_row(p, c, cur, sel) }),
              [new_channel_input(p, noop, on_new_channel)],
            ),
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
                  voice_peers,
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
        ],
      ),
    ],
  )
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
        [title_text(r), chevron_right(p)],
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
      // Desktop: room title only — connection state is surfaced via a
      // text label in the rooms-rail meta_line ("reconnecting" /
      // "offline") and via the phone header on mobile, so a duplicate
      // dot here would only add visual noise.
      Desktop -> [title_el]
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
      attribute.attribute("data-testid", "text-channel-row"),
      attribute.attribute("data-channel-name", c.name),
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

/// Bottom-of-section input that lets the user type a fresh channel
/// name and press Enter to switch into it. The new channel is added
/// to the rail locally even before any traffic is observed, so the
/// composer can route a SubmitDraft into a brand-new channel without
/// waiting for someone else to post first.
fn new_channel_input(
  p: Palette,
  noop: msg,
  on_new_channel: fn(String) -> msg,
) -> Element(msg) {
  html.div(
    [
      ui.css([
        #("padding", "4px 12px 0 12px"),
        #("display", "flex"),
        #("align-items", "center"),
        #("gap", "6px"),
      ]),
    ],
    [
      html.span(
        [
          ui.css([
            #("color", p.text_faint),
            #("font-size", "16.25px"),
          ]),
        ],
        [html.text("+")],
      ),
      html.input([
        attribute.attribute("data-testid", "new-channel-input"),
        attribute.placeholder("new channel"),
        on_enter_with_value(noop, on_new_channel),
        ui.css([
          #("flex", "1"),
          #("min-width", "0"),
          #("box-sizing", "border-box"),
          #("background", "transparent"),
          #("border", "none"),
          #("padding", "4px 0"),
          #("font-family", "inherit"),
          #("font-size", "16.25px"),
          #("color", p.text),
          #("outline", "none"),
        ]),
      ]),
    ],
  )
}

/// Enter on the new-channel input fires `on_new_channel(trimmed_value)`
/// and preventDefaults so the keystroke can't bleed through to a
/// different focused element after Lustre's re-render. Non-Enter keys
/// dispatch `noop` so we don't run the new-channel reducer on every
/// keystroke. Mirrors the rooms-rail pattern.
fn on_enter_with_value(
  noop: msg,
  on_new_channel: fn(String) -> msg,
) -> attribute.Attribute(msg) {
  event.advanced("keydown", {
    use key <- decode.subfield(["key"], decode.string)
    use value <- decode.subfield(["target", "value"], decode.string)
    decode.success(case key {
      "Enter" ->
        event.handler(
          on_new_channel(string.trim(value)),
          prevent_default: True,
          stop_propagation: False,
        )
      _ -> event.handler(noop, prevent_default: False, stop_propagation: False)
    })
  })
}

fn voice_block(
  p: Palette,
  c: Channel,
  in_call_members: List(Member),
  voice_peers: Dict(String, VoicePeerStateUI),
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
        voice_peers,
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
      // Same mic glyph as the live voice block, in muted color so the
      // idle state reads as "voice channel, no one in it yet" rather
      // than introducing a different shape (the old ◐ half-circle didn't
      // map to anything meaningful and was easy to misread as a status).
      html.span(
        [
          ui.css([
            #("color", p.text_faint),
            #("display", "inline-flex"),
            #("align-items", "center"),
          ]),
        ],
        [voice_icon()],
      ),
      html.span([ui.css([#("flex", "1")])], [html.text(c.name)]),
    ],
  )
}

fn live_voice_block(
  p: Palette,
  c: Channel,
  ms: List(Member),
  voice_peers: Dict(String, VoicePeerStateUI),
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
  // Magenta accent treatment is reserved for the "you are in this
  // call" state — it's the same brand role the voice minibar uses to
  // signal active participation. When we're observing peers in a
  // call we haven't joined, the block uses neutral surface tones so
  // the user reads the rail as "this channel is live" without
  // mistaking it for "you're connected".
  let block_bg = case self_in_call {
    True -> p.accent_soft
    False -> p.surface_sunk
  }
  let header_color = case self_in_call {
    True -> p.accent_deep
    False -> p.text_muted
  }
  html.div(
    [
      ui.css([
        #("background", block_bg),
        #("border-radius", "6px"),
        #("padding-bottom", "10px"),
      ]),
    ],
    [
      html.button(
        [
          attribute.attribute("data-testid", "voice-channel-row"),
          attribute.attribute("data-voice-self-joined", case self_in_call {
            True -> "true"
            False -> "false"
          }),
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
            #("color", header_color),
            #("border", "none"),
            #("background", "transparent"),
            #("font-family", "inherit"),
            #("text-align", "left"),
            #("cursor", "pointer"),
            #("width", "100%"),
          ]),
        ],
        [
          // Voice icon — clearer than a colored dot at conveying
          // "this is a voice channel". Inherits color from the
          // parent button.
          voice_icon(),
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
          [connector_line(p, self_in_call)],
          list.map(ms, fn(m) {
            voice_member_row(
              p,
              m,
              voice_peers,
              peer_levels,
              self_level,
              self_in_call,
              popover_open,
              on_open_voice_popover,
            )
          }),
        ]),
      ),
    ],
  )
}

fn connector_line(p: Palette, self_in_call: Bool) -> Element(msg) {
  // Bound to the inset of the members container's padding so the line
  // only spans the rows themselves, not the surrounding whitespace at
  // the top and bottom of the block. Color tracks the block's tone:
  // accent when self is in the call, neutral when observing.
  let #(color, opacity) = case self_in_call {
    True -> #(p.accent, "0.35")
    False -> #(p.text_faint, "0.55")
  }
  html.span(
    [
      ui.css([
        #("position", "absolute"),
        #("left", "16px"),
        #("top", "4px"),
        #("bottom", "10px"),
        #("width", "2px"),
        #("background", color),
        #("opacity", opacity),
        #("border-radius", "1px"),
      ]),
    ],
    [],
  )
}

fn voice_member_row(
  p: Palette,
  m: Member,
  voice_peers: Dict(String, VoicePeerStateUI),
  peer_levels: Dict(String, Float),
  self_level: Float,
  self_in_call: Bool,
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
  // "Connected" = we have audio flow with this peer (in_call from
  // voice.peers, driven by frame/heartbeat liveness). "In voice
  // channel" = peer announced presence (in_voice_channel; broader,
  // already gated upstream — Member.in_call carries that signal
  // here). The connected/not-connected distinction is what drives
  // the dimmed style + "connecting…" affordance for peers we see
  // in the channel but haven't established a P2P link with yet.
  // Self is trivially connected once we've joined.
  let connected = case m.you {
    True -> self_in_call
    False ->
      case dict.get(voice_peers, peer_key) {
        Ok(ps) -> ps.in_call
        Error(_) -> False
      }
  }
  // Real audio level: 0..1, driven by the FFI's per-peer RMS smoother
  // (or the local mic level for self). When muted or not yet
  // connected (no audio path), force to 0 — neither the level meter
  // nor the "speaking" highlight should imply audio is flowing.
  let raw_level = case m.you {
    True -> self_level
    False ->
      dict.get(peer_levels, peer_key)
      |> result.unwrap(0.0)
  }
  let level = case muted, connected {
    False, True -> raw_level
    _, _ -> 0.0
  }
  let speaking = connected && level >. voice_meter.speaking_threshold()
  // Voice member dot semantics:
  //   * Speaking → green, the only "active" signal in the row
  //   * In-call but quiet → no dot at all (the trailing waveform meter
  //     already conveys "audio path live, no level"; the dot was
  //     redundant)
  //   * Not connected (still in voice channel) → hollow gray ring,
  //     matching the same convention used in the members rail
  //   * Muted → small mic-muted glyph, conveys *why* there's no audio
  //     better than a colored dot did
  let leading_glyph = case connected, muted, speaking {
    False, _, _ -> hollow_dot(p.text_faint)
    True, True, _ -> mic_muted_glyph(p)
    True, False, True -> filled_dot(p.live)
    True, False, False -> empty_slot()
  }
  let active = case popover_open {
    Some(open_id) -> open_id == peer_key
    None -> False
  }
  let bg = case active {
    True -> p.surface
    False -> "transparent"
  }
  // Disconnected rows render at reduced opacity so the eye reads them
  // as "in the channel but not currently audible" without removing
  // them from the roster. In observer mode (self not in this call) we
  // skip the dimming — every row is "not connected to me" trivially,
  // and the block's neutral palette already signals "not in it"; an
  // additional 0.55 opacity on every row would just make the names
  // hard to read.
  let row_opacity = case connected, self_in_call {
    True, _ -> "1"
    False, False -> "1"
    False, True -> "0.55"
  }
  // Observer mode (self not in this call) makes the row purely
  // informational: there's no audio path to this peer, so the
  // per-peer popover (volume / mute-for-me / send quality) has
  // nothing to act on. Disabling the button suppresses clicks at
  // the browser level and we override `cursor` to `default` so the
  // row doesn't read as a tappable target.
  html.button(
    [
      attribute.attribute("data-testid", "voice-member"),
      attribute.attribute("data-voice-name", m.name),
      attribute.attribute("data-peer-hex", peer_key),
      attribute.attribute("data-voice-connected", case connected {
        True -> "true"
        False -> "false"
      }),
      attribute.attribute(
        "data-voice-level",
        voice_meter.level_to_attribute(level),
      ),
      attribute.attribute("data-voice-speaking", case speaking {
        True -> "true"
        False -> "false"
      }),
      attribute.disabled(!self_in_call),
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
        #("cursor", case self_in_call {
          True -> "pointer"
          False -> "default"
        }),
        #("opacity", row_opacity),
      ]),
    ],
    [
      leading_glyph,
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
      // Trailing slot. Three states drive what shows here:
      //   * Self is in the call AND we have audio flow with this peer
      //     → live waveform meter (or "muted" pill if the peer is
      //     muted): same affordance the previous design had.
      //   * Self is in the call but we don't have a P2P link yet (or
      //     anymore) → animated 'connecting' dots — we're actively
      //     trying to reach this peer.
      //   * Self is NOT in the call → render nothing. We aren't
      //     dialing anyone; the rail is in observer mode and showing
      //     a connecting affordance for every peer would lie about
      //     work we aren't doing.
      case self_in_call, connected, muted {
        False, _, _ -> element.fragment([])
        True, False, _ -> not_connected_label(p)
        True, True, True ->
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
        True, True, False -> waveform_meter(p, level)
      },
    ],
  )
}

/// Affordance for a peer who is in the voice channel but we don't
/// have a P2P connection to yet (or anymore). Three pulsing dots
/// instead of a wide "connecting…" label so the icon stays the same
/// width as the row's normal trailing slot (the waveform meter) and
/// doesn't push the member name around as state flips. Animation
/// keyframes live in shell.global_reset; reduced-motion users get a
/// static dimmed dot.
fn not_connected_label(p: Palette) -> Element(msg) {
  let dot =
    html.span(
      [
        attribute.class("voice-connecting-dot"),
        ui.css([
          #("display", "inline-block"),
          #("width", "4px"),
          #("height", "4px"),
          #("border-radius", "50%"),
          #("background", p.text_faint),
        ]),
      ],
      [],
    )
  html.span(
    [
      attribute.attribute("data-testid", "voice-member-not-connected"),
      attribute.attribute("aria-label", "connecting"),
      attribute.title("connecting"),
      ui.css([
        #("display", "inline-flex"),
        #("align-items", "center"),
        #("gap", "3px"),
      ]),
    ],
    [dot, dot, dot],
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

/// 7px filled dot used for the "speaking" indicator on a voice
/// member row. Wider context lives at the call site.
fn filled_dot(color: String) -> Element(msg) {
  html.span(
    [
      ui.css([
        #("width", "7px"),
        #("height", "7px"),
        #("border-radius", "999px"),
        #("background", color),
        #("flex-shrink", "0"),
      ]),
    ],
    [],
  )
}

/// Hollow ring — same footprint as `filled_dot`, but reads as "off" /
/// "not connected" without relying on the user remembering which gray
/// means what.
fn hollow_dot(color: String) -> Element(msg) {
  html.span(
    [
      ui.css([
        #("width", "7px"),
        #("height", "7px"),
        #("border-radius", "999px"),
        #("background", "transparent"),
        #("border", "1.5px solid " <> color),
        #("box-sizing", "border-box"),
        #("flex-shrink", "0"),
      ]),
    ],
    [],
  )
}

/// Empty 7×7 placeholder so the column of leading glyphs in the voice
/// member rows aligns whether the row has a dot, a ring, or nothing
/// (in-call but not currently speaking).
fn empty_slot() -> Element(msg) {
  html.span(
    [
      ui.css([
        #("width", "7px"),
        #("height", "7px"),
        #("flex-shrink", "0"),
      ]),
    ],
    [],
  )
}

/// Mic-with-slash glyph for a peer who is muted. Sits where the
/// speaking-state dot would otherwise go so the eye reads "no audio,
/// because mic off" rather than "no audio, unknown why".
fn mic_muted_glyph(p: Palette) -> Element(msg) {
  element.namespaced(
    "http://www.w3.org/2000/svg",
    "svg",
    [
      attribute.attribute("width", "10"),
      attribute.attribute("height", "10"),
      attribute.attribute("viewBox", "0 0 12 12"),
      attribute.attribute("fill", "none"),
      ui.css([
        #("color", p.text_faint),
        #("flex-shrink", "0"),
      ]),
    ],
    [
      element.namespaced(
        "http://www.w3.org/2000/svg",
        "rect",
        [
          attribute.attribute("x", "4.5"),
          attribute.attribute("y", "1.5"),
          attribute.attribute("width", "3"),
          attribute.attribute("height", "5"),
          attribute.attribute("rx", "1.5"),
          attribute.attribute("stroke", "currentColor"),
          attribute.attribute("stroke-width", "1.2"),
        ],
        [],
      ),
      element.namespaced(
        "http://www.w3.org/2000/svg",
        "path",
        [
          attribute.attribute("d", "M2.5 6a3.5 3.5 0 007 0M6 9.5v1.5"),
          attribute.attribute("stroke", "currentColor"),
          attribute.attribute("stroke-width", "1.2"),
          attribute.attribute("stroke-linecap", "round"),
        ],
        [],
      ),
      element.namespaced(
        "http://www.w3.org/2000/svg",
        "path",
        [
          attribute.attribute("d", "M1.5 1l9 10"),
          attribute.attribute("stroke", "currentColor"),
          attribute.attribute("stroke-width", "1.2"),
          attribute.attribute("stroke-linecap", "round"),
        ],
        [],
      ),
    ],
  )
}

/// Microphone glyph — leading icon on the live voice channel header.
/// "Voice channel = use your mic" is the universal convention (Discord,
/// Slack, etc.), and a mic icon doesn't get visually confused with the
/// member-row speaking dots the way a speaker-with-waves design did.
fn voice_icon() -> Element(msg) {
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
      // Mic capsule.
      element.namespaced(
        "http://www.w3.org/2000/svg",
        "rect",
        [
          attribute.attribute("x", "6"),
          attribute.attribute("y", "2"),
          attribute.attribute("width", "4"),
          attribute.attribute("height", "8"),
          attribute.attribute("rx", "2"),
          attribute.attribute("stroke", "currentColor"),
          attribute.attribute("stroke-width", "1.4"),
        ],
        [],
      ),
      // Stand — yoke arc + vertical post + base bar.
      element.namespaced(
        "http://www.w3.org/2000/svg",
        "path",
        [
          attribute.attribute(
            "d",
            "M3.5 8a4.5 4.5 0 009 0M8 12.5V14.5M5.5 14.5h5",
          ),
          attribute.attribute("stroke", "currentColor"),
          attribute.attribute("stroke-width", "1.4"),
          attribute.attribute("stroke-linecap", "round"),
        ],
        [],
      ),
    ],
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
