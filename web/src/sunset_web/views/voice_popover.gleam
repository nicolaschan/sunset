//// Floating popover that opens when the user clicks an in-call
//// member in the voice channel detail block.
////
//// Shows the peer's status, a level-driven waveform reflecting their
//// live audio, and three per-peer controls:
////   * Volume slider — 0–100% for the local user, 0–200% for others.
////   * Denoise toggle — strip background noise from this peer's
////     incoming stream (or your outgoing stream, on the self row).
////   * Mute-for-me / Reset — only on non-self rows.
////
//// Anchored at a fixed position over the chat shell so it can render
//// wider than the 230px channels column. The host renders the popover
//// once at the shell level; visibility is driven by `Model.voice_popover`.

import gleam/dynamic/decode
import gleam/float
import gleam/int
import lustre/attribute
import lustre/element.{type Element}
import lustre/element/html
import lustre/event
import sunset_web/domain.{
  type Member, type VoiceSettings, Direct, MutedP, NoRelay, OneHop, SelfRelay,
  TwoHop, ViaPeer,
}
import sunset_web/theme.{type Palette}
import sunset_web/ui

pub type Placement {
  Floating
  InSheet
}

pub fn view(
  palette p: Palette,
  placement placement: Placement,
  member m: Member,
  settings settings: VoiceSettings,
  level level: Float,
  on_close on_close: msg,
  on_set_volume on_set_volume: fn(Int) -> msg,
  on_toggle_denoise on_toggle_denoise: msg,
  on_toggle_deafen on_toggle_deafen: msg,
  on_reset on_reset: msg,
) -> Element(msg) {
  let is_self = m.you
  let max_volume = case is_self {
    True -> 100
    False -> 200
  }

  let body_children = [
    header(p, m, settings, level, on_close),
    waveform_strip(p, m, settings, level),
    body(p, m, settings, max_volume, on_set_volume, on_toggle_denoise),
    case is_self {
      True -> element.fragment([])
      False -> footer(p, settings, on_toggle_deafen, on_reset)
    },
  ]

  case placement {
    Floating ->
      html.div(
        [
          attribute.attribute("data-testid", "voice-popover"),
          ui.css([
            #("position", "fixed"),
            #("top", "120px"),
            #("left", "540px"),
            #("width", "320px"),
            #("background", p.surface),
            #("color", p.text),
            #("border", "1px solid " <> p.border),
            #("border-radius", "10px"),
            #("box-shadow", p.shadow_lg),
            #("z-index", "20"),
            #("display", "flex"),
            #("flex-direction", "column"),
          ]),
        ],
        body_children,
      )
    InSheet ->
      html.div(
        [
          attribute.attribute("data-testid", "voice-popover"),
          ui.css([
            #("display", "flex"),
            #("flex-direction", "column"),
            #("width", "100%"),
            #("color", p.text),
          ]),
        ],
        body_children,
      )
  }
}

fn header(
  p: Palette,
  m: Member,
  settings: VoiceSettings,
  level: Float,
  on_close: msg,
) -> Element(msg) {
  // Drive the "speaking" header text from real audio level rather than
  // the static presence enum — m.status is a coarse online/away/muted
  // flag, not a moment-to-moment voice activity signal.
  let muted = m.status == MutedP
  let speaking = !muted && level >. speaking_threshold()
  let status_text = case muted, speaking {
    True, _ -> "muted"
    False, True -> "speaking"
    False, False -> "in call"
  }
  let relay_text = relay_label(m)
  html.div(
    [
      ui.css([
        #("display", "flex"),
        #("align-items", "center"),
        #("gap", "10px"),
        #("padding", "12px 14px"),
        #("border-bottom", "1px solid " <> p.border_soft),
      ]),
    ],
    [
      avatar(p, m),
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
          html.div(
            [
              ui.css([
                #("display", "flex"),
                #("align-items", "baseline"),
                #("gap", "6px"),
              ]),
            ],
            [
              html.span(
                [
                  ui.css([
                    #("font-weight", "600"),
                    #("font-size", "16.875px"),
                    #("color", p.text),
                  ]),
                ],
                [html.text(m.name)],
              ),
              case m.you {
                True -> you_tag(p)
                False -> element.fragment([])
              },
            ],
          ),
          html.div(
            [
              ui.css([
                #("font-size", "13.125px"),
                #("color", p.text_muted),
              ]),
            ],
            [
              html.text(status_text),
              case relay_text {
                "" -> element.fragment([])
                t ->
                  html.span([], [
                    html.text(" · "),
                    html.text(t),
                  ])
              },
              case settings.deafened && !m.you {
                True ->
                  html.span([ui.css([#("color", p.warn)])], [
                    html.text(" · muted for you"),
                  ])
                False -> element.fragment([])
              },
            ],
          ),
        ],
      ),
      close_button(p, on_close),
    ],
  )
}

fn close_button(p: Palette, on_close: msg) -> Element(msg) {
  html.button(
    [
      attribute.title("Close"),
      attribute.attribute("aria-label", "Close popover"),
      attribute.attribute("data-testid", "voice-popover-close"),
      event.on_click(on_close),
      ui.css([
        #("width", "26px"),
        #("height", "26px"),
        #("display", "inline-flex"),
        #("align-items", "center"),
        #("justify-content", "center"),
        #("padding", "0"),
        #("border", "1px solid " <> p.border_soft),
        #("background", "transparent"),
        #("color", p.text_muted),
        #("border-radius", "6px"),
        #("cursor", "pointer"),
        #("font-family", "inherit"),
      ]),
    ],
    [
      element.namespaced(
        "http://www.w3.org/2000/svg",
        "svg",
        [
          attribute.attribute("width", "12"),
          attribute.attribute("height", "12"),
          attribute.attribute("viewBox", "0 0 12 12"),
          attribute.attribute("fill", "none"),
        ],
        [
          element.namespaced(
            "http://www.w3.org/2000/svg",
            "path",
            [
              attribute.attribute("d", "M3 3l6 6M9 3l-6 6"),
              attribute.attribute("stroke", "currentColor"),
              attribute.attribute("stroke-width", "1.5"),
              attribute.attribute("stroke-linecap", "round"),
            ],
            [],
          ),
        ],
      ),
    ],
  )
}

fn avatar(p: Palette, m: Member) -> Element(msg) {
  html.span(
    [
      ui.css([
        #("display", "inline-flex"),
        #("align-items", "center"),
        #("justify-content", "center"),
        #("width", "36px"),
        #("height", "36px"),
        #("border-radius", "999px"),
        #("background", p.accent_soft),
        #("color", p.accent_deep),
        #("font-size", "13.125px"),
        #("font-weight", "600"),
        #("text-transform", "uppercase"),
        #("letter-spacing", "0.04em"),
        #("flex-shrink", "0"),
      ]),
    ],
    [html.text(m.initials)],
  )
}

fn you_tag(p: Palette) -> Element(msg) {
  html.span(
    [
      ui.css([
        #("padding", "1px 5px"),
        #("border-radius", "3px"),
        #("background", p.surface_alt),
        #("color", p.text_faint),
        #("font-size", "10px"),
        #("font-weight", "500"),
        #("letter-spacing", "0.02em"),
        #("text-transform", "uppercase"),
      ]),
    ],
    [html.text("you")],
  )
}

fn relay_label(m: Member) -> String {
  case m.relay {
    Direct -> "direct"
    OneHop -> "1-hop"
    TwoHop -> "2-hop"
    ViaPeer(name) -> "via " <> name
    SelfRelay -> ""
    NoRelay -> ""
  }
}

/// Audio-level threshold above which we render the popover header /
/// waveform as "speaking". Mirrors the channels-rail row threshold so
/// both surfaces flip together.
fn speaking_threshold() -> Float {
  0.05
}

fn waveform_strip(
  p: Palette,
  m: Member,
  settings: VoiceSettings,
  level: Float,
) -> Element(msg) {
  // 36-bar level visualisation driven by the smoothed FFI level. When
  // muted (either remote-muted via voice state, or muted-for-me on a
  // peer row) we force the level to 0 so the bars sit flat — not a
  // "this peer is silent" lie, since the audio really is being
  // suppressed locally.
  let muted = m.status == MutedP || { !m.you && settings.deafened }
  let effective_level = case muted {
    True -> 0.0
    False -> level
  }
  let speaking = effective_level >. speaking_threshold()
  html.div(
    [
      attribute.attribute("data-testid", "voice-popover-waveform"),
      attribute.attribute(
        "data-voice-level",
        level_to_attribute(effective_level),
      ),
      attribute.attribute("data-voice-speaking", case speaking {
        True -> "true"
        False -> "false"
      }),
      ui.css([
        #("padding", "10px 14px 6px 14px"),
        #("background", p.surface_alt),
      ]),
    ],
    [bars(p, effective_level)],
  )
}

fn bars(p: Palette, level: Float) -> Element(msg) {
  let bars_count = 36
  let bars_list = list_repeat_index(bars_count)
  html.div(
    [
      ui.css([
        #("display", "flex"),
        #("align-items", "center"),
        #("justify-content", "space-between"),
        #("height", "32px"),
        #("gap", "2px"),
      ]),
    ],
    bars_list
      |> list_map(fn(i) { single_bar(p, level, i, bars_count) }),
  )
}

fn single_bar(p: Palette, level: Float, i: Int, n: Int) -> Element(msg) {
  // Each bar is shaped by a sinusoidal envelope (peak in the middle,
  // tapered at the edges) so the strip reads as a waveform — and the
  // whole envelope scales with the live audio level so it grows as the
  // peer talks louder. At idle, every bar collapses to a 2 px dot.
  let envelope = arch_envelope(i, n)
  let max_h = 4.0 +. envelope *. 28.0
  let h = 2.0 +. level *. max_h
  let opacity = case level >. 0.02 {
    True -> "0.85"
    False -> "0.35"
  }
  let color = case level >. speaking_threshold() {
    True -> p.accent
    False -> p.text_faint
  }
  html.span(
    [
      ui.css([
        #("display", "inline-block"),
        #("width", "4px"),
        #("border-radius", "2px"),
        #("background", color),
        #("opacity", opacity),
        #("height", float_to_px(h)),
      ]),
    ],
    [],
  )
}

fn arch_envelope(i: Int, n: Int) -> Float {
  // Tent: 0 at the edges, 1 at the centre. Cheap stand-in for a sin
  // shape; reads as a smooth arch when rendered with 36 bars.
  let mid = int.to_float(n) /. 2.0
  let pos = int.to_float(i) +. 0.5
  let dist = case pos <. mid {
    True -> mid -. pos
    False -> pos -. mid
  }
  let normalised = 1.0 -. dist /. mid
  case normalised <. 0.0 {
    True -> 0.0
    False -> normalised
  }
}

fn float_to_px(f: Float) -> String {
  int.to_string(float.round(f)) <> "px"
}

fn level_to_attribute(f: Float) -> String {
  let scaled = float.round(f *. 100.0)
  let int_part = scaled / 100
  let frac = scaled % 100
  let frac_part = case frac < 10 {
    True -> "0" <> int.to_string(frac)
    False -> int.to_string(frac)
  }
  int.to_string(int_part) <> "." <> frac_part
}

fn list_repeat_index(n: Int) -> List(Int) {
  do_list_repeat_index(n, 0, [])
}

fn do_list_repeat_index(n: Int, i: Int, acc: List(Int)) -> List(Int) {
  case i >= n {
    True -> list_reverse(acc)
    False -> do_list_repeat_index(n, i + 1, [i, ..acc])
  }
}

fn list_reverse(xs: List(a)) -> List(a) {
  do_reverse(xs, [])
}

fn do_reverse(xs: List(a), acc: List(a)) -> List(a) {
  case xs {
    [] -> acc
    [head, ..tail] -> do_reverse(tail, [head, ..acc])
  }
}

fn list_map(xs: List(a), f: fn(a) -> b) -> List(b) {
  do_map(xs, f, [])
}

fn do_map(xs: List(a), f: fn(a) -> b, acc: List(b)) -> List(b) {
  case xs {
    [] -> list_reverse(acc)
    [head, ..tail] -> do_map(tail, f, [f(head), ..acc])
  }
}

fn body(
  p: Palette,
  m: Member,
  settings: VoiceSettings,
  max_volume: Int,
  on_set_volume: fn(Int) -> msg,
  on_toggle_denoise: msg,
) -> Element(msg) {
  html.div(
    [
      ui.css([
        #("padding", "14px"),
        #("display", "flex"),
        #("flex-direction", "column"),
        #("gap", "16px"),
      ]),
    ],
    [
      volume_control(p, m, settings, max_volume, on_set_volume),
      denoise_control(p, m, settings, on_toggle_denoise),
    ],
  )
}

fn volume_control(
  p: Palette,
  m: Member,
  settings: VoiceSettings,
  max_volume: Int,
  on_set_volume: fn(Int) -> msg,
) -> Element(msg) {
  let label_text = case m.you {
    True -> "Output volume"
    False -> "Volume for me"
  }
  html.div(
    [
      ui.css([
        #("display", "flex"),
        #("flex-direction", "column"),
        #("gap", "6px"),
      ]),
    ],
    [
      html.div(
        [
          ui.css([
            #("display", "flex"),
            #("align-items", "baseline"),
            #("justify-content", "space-between"),
          ]),
        ],
        [
          control_label(p, label_text),
          html.span(
            [
              ui.css([
                #("font-family", theme.font_mono),
                #("font-size", "13.125px"),
                #("color", p.text),
                #("font-weight", "600"),
                #("font-variant-numeric", "tabular-nums"),
              ]),
            ],
            [html.text(int.to_string(settings.volume) <> "%")],
          ),
        ],
      ),
      html.input([
        attribute.attribute("data-testid", "voice-popover-volume"),
        attribute.attribute("type", "range"),
        attribute.attribute("min", "0"),
        attribute.attribute("max", int.to_string(max_volume)),
        attribute.attribute("step", "5"),
        attribute.value(int.to_string(settings.volume)),
        on_input_int(on_set_volume),
        ui.css([
          #("width", "100%"),
          #("accent-color", p.accent),
        ]),
      ]),
      html.div(
        [
          ui.css([
            #("display", "flex"),
            #("justify-content", "space-between"),
            #("font-size", "10.5px"),
            #("color", p.text_faint),
            #("font-family", theme.font_mono),
          ]),
        ],
        [
          html.span([], [html.text("0")]),
          html.span([], [html.text("100")]),
          case max_volume {
            200 -> html.span([], [html.text("200")])
            _ -> element.fragment([])
          },
        ],
      ),
    ],
  )
}

fn denoise_control(
  p: Palette,
  m: Member,
  settings: VoiceSettings,
  on_toggle: msg,
) -> Element(msg) {
  let hint = case m.you {
    True -> "Strip background noise from your outgoing audio."
    False ->
      "Filter background noise from their incoming stream — applied locally."
  }
  html.div(
    [
      ui.css([
        #("display", "flex"),
        #("flex-direction", "column"),
        #("gap", "6px"),
      ]),
    ],
    [
      html.div(
        [
          ui.css([
            #("display", "flex"),
            #("align-items", "center"),
            #("justify-content", "space-between"),
            #("gap", "8px"),
          ]),
        ],
        [
          control_label(p, "Denoise"),
          toggle_switch(p, settings.denoise, on_toggle, "voice-popover-denoise"),
        ],
      ),
      html.div(
        [
          ui.css([
            #("font-size", "12.5px"),
            #("color", p.text_muted),
          ]),
        ],
        [html.text(hint)],
      ),
    ],
  )
}

fn footer(
  p: Palette,
  settings: VoiceSettings,
  on_toggle_deafen: msg,
  on_reset: msg,
) -> Element(msg) {
  html.div(
    [
      ui.css([
        #("display", "flex"),
        #("gap", "8px"),
        #("padding", "0 14px 14px 14px"),
      ]),
    ],
    [
      mute_for_me_button(p, settings.deafened, on_toggle_deafen),
      reset_button(p, on_reset),
    ],
  )
}

fn mute_for_me_button(
  p: Palette,
  deafened: Bool,
  on_toggle: msg,
) -> Element(msg) {
  let bg = case deafened {
    True -> p.warn_soft
    False -> p.surface_alt
  }
  let color = case deafened {
    True -> p.warn
    False -> p.text
  }
  html.button(
    [
      attribute.attribute("data-testid", "voice-popover-deafen"),
      attribute.attribute("aria-pressed", case deafened {
        True -> "true"
        False -> "false"
      }),
      event.on_click(on_toggle),
      ui.css([
        #("flex", "1"),
        #("padding", "8px 10px"),
        #("border", "1px solid " <> p.border_soft),
        #("background", bg),
        #("color", color),
        #("border-radius", "6px"),
        #("font-family", "inherit"),
        #("font-size", "13.125px"),
        #("font-weight", "500"),
        #("cursor", "pointer"),
      ]),
    ],
    [
      html.text(case deafened {
        True -> "● Muted for you"
        False -> "Mute for me"
      }),
    ],
  )
}

fn reset_button(p: Palette, on_reset: msg) -> Element(msg) {
  html.button(
    [
      attribute.attribute("data-testid", "voice-popover-reset"),
      event.on_click(on_reset),
      ui.css([
        #("padding", "8px 12px"),
        #("border", "1px solid " <> p.border_soft),
        #("background", "transparent"),
        #("color", p.text_muted),
        #("border-radius", "6px"),
        #("font-family", "inherit"),
        #("font-size", "13.125px"),
        #("font-weight", "500"),
        #("cursor", "pointer"),
      ]),
    ],
    [html.text("Reset")],
  )
}

fn control_label(p: Palette, label: String) -> Element(msg) {
  html.span(
    [
      ui.css([
        #("font-size", "12.5px"),
        #("color", p.text_faint),
        #("text-transform", "uppercase"),
        #("letter-spacing", "0.05em"),
        #("font-weight", "600"),
      ]),
    ],
    [html.text(label)],
  )
}

fn toggle_switch(
  p: Palette,
  on: Bool,
  click: msg,
  test_id: String,
) -> Element(msg) {
  let bg = case on {
    True -> p.accent
    False -> p.surface_alt
  }
  let knob_x = case on {
    True -> "20px"
    False -> "2px"
  }
  html.button(
    [
      attribute.attribute("data-testid", test_id),
      attribute.attribute("aria-pressed", case on {
        True -> "true"
        False -> "false"
      }),
      attribute.attribute("role", "switch"),
      event.on_click(click),
      ui.css([
        #("position", "relative"),
        #("width", "40px"),
        #("height", "22px"),
        #("padding", "0"),
        #("border", "1px solid " <> p.border_soft),
        #("background", bg),
        #("border-radius", "999px"),
        #("cursor", "pointer"),
        #("flex-shrink", "0"),
      ]),
    ],
    [
      html.span(
        [
          ui.css([
            #("position", "absolute"),
            #("top", "2px"),
            #("left", knob_x),
            #("width", "16px"),
            #("height", "16px"),
            #("border-radius", "999px"),
            #("background", p.surface),
            #("transition", "left 120ms ease"),
            #("box-shadow", p.shadow),
          ]),
        ],
        [],
      ),
    ],
  )
}

/// Decoder: parse `event.target.value` as an integer and dispatch
/// `msg(value)`. Used by the volume slider.
fn on_input_int(handler: fn(Int) -> msg) -> attribute.Attribute(msg) {
  event.on("input", {
    use raw <- decode.subfield(["target", "value"], decode.string)
    case int.parse(raw) {
      Ok(n) -> decode.success(handler(n))
      Error(_) -> decode.failure(handler(0), "non-integer-volume")
    }
  })
}
