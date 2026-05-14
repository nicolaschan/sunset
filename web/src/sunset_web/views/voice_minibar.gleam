//// In-flow voice-status panel shown in the phone chat view when the
//// user is in a call. Sits directly under the phone header; matches
//// the desktop self-controls bar in shape (channel name on the left,
//// mic / headphones / leave icon buttons on the right) but uses the
//// accent palette so the in-call state is visually distinct from
//// the rest of the chrome.
////
//// Tapping anywhere on the panel opens the user's own voice sheet
//// where the actual mute / deafen / leave controls live. The icon
//// buttons here are status affordances only — the row's onClick
//// dispatches the same Msg regardless of which child was tapped.

import lustre/attribute
import lustre/element.{type Element}
import lustre/element/html
import lustre/event
import sunset_web/theme.{type Palette}
import sunset_web/ui

pub fn view(
  palette p: Palette,
  channel_name channel_name: String,
  on_open on_open: msg,
  on_mute on_mute: msg,
  on_deafen on_deafen: msg,
  on_leave on_leave: msg,
  self_muted self_muted: Bool,
  self_deafened self_deafened: Bool,
) -> Element(msg) {
  // Soft accent fill + a 3px solid accent ribbon on the left so the
  // minibar still reads as "you are in a call" without flooding the
  // chrome with the brand color.
  html.div(
    [
      attribute.attribute("data-testid", "voice-minibar"),
      ui.css([
        #("display", "flex"),
        #("align-items", "center"),
        #("gap", "8px"),
        #("box-sizing", "border-box"),
        #("width", "100%"),
        #("padding", "8px 12px"),
        #("background", p.accent_soft),
        #("color", p.accent_deep),
        #("border-bottom", "1px solid " <> p.border),
        #("border-left", "3px solid " <> p.accent),
        #("font-size", "14px"),
        #("font-weight", "600"),
        #("flex-shrink", "0"),
      ]),
    ],
    [
      label_button(p, channel_name, on_open),
      icon_button(
        p,
        case self_muted {
          True -> "Unmute mic"
          False -> "Mute mic"
        },
        mic_icon(self_muted),
        on_mute,
        self_muted,
      ),
      icon_button(
        p,
        case self_deafened {
          True -> "Undeafen"
          False -> "Deafen"
        },
        headphones_icon(self_deafened),
        on_deafen,
        self_deafened,
      ),
      leave_button(p, on_leave),
    ],
  )
}

fn label_button(_p: Palette, channel_name: String, on_open: msg) -> Element(msg) {
  html.button(
    [
      attribute.attribute("aria-label", "Voice controls for " <> channel_name),
      event.on_click(on_open),
      ui.css([
        #("flex", "1"),
        #("min-width", "0"),
        #("display", "flex"),
        #("align-items", "center"),
        #("gap", "6px"),
        #("white-space", "nowrap"),
        #("overflow", "hidden"),
        #("text-overflow", "ellipsis"),
        #("border", "none"),
        #("background", "transparent"),
        #("color", "currentColor"),
        #("font-family", "inherit"),
        #("font-size", "inherit"),
        #("font-weight", "inherit"),
        #("padding", "0"),
        #("cursor", "pointer"),
        #("text-align", "left"),
      ]),
    ],
    [
      // The minibar's accent-colored bar IS the "you are in a call"
      // indicator — a redundant white dot beside the channel name was
      // adding noise without information.
      html.span([], [html.text(channel_name)]),
    ],
  )
}

fn icon_button(
  p: Palette,
  title: String,
  icon: Element(msg),
  on_click: msg,
  active: Bool,
) -> Element(msg) {
  // Active state surfaces a danger tint + danger-colored icon so being
  // muted or deafened in a call reads at a glance — matches the universal
  // convention (Discord/Slack/Zoom/Teams). The slashed icon variant the
  // caller passes carries the same signal in shape, so colorblind users
  // still distinguish active from inactive without relying on hue.
  let bg = case active {
    True -> p.danger_soft
    False -> "rgba(0, 0, 0, 0.05)"
  }
  let fg = case active {
    True -> p.danger
    False -> "currentColor"
  }
  html.button(
    [
      attribute.title(title),
      attribute.attribute("aria-pressed", case active {
        True -> "true"
        False -> "false"
      }),
      event.on_click(on_click),
      ui.css([
        #("width", "30px"),
        #("height", "30px"),
        #("display", "inline-flex"),
        #("align-items", "center"),
        #("justify-content", "center"),
        #("padding", "0"),
        #("border", "none"),
        #("border-radius", "6px"),
        #("background", bg),
        #("color", fg),
        #("flex-shrink", "0"),
        #("cursor", "pointer"),
        #("font-family", "inherit"),
      ]),
    ],
    [icon],
  )
}

fn leave_button(p: Palette, on_click: msg) -> Element(msg) {
  html.button(
    [
      attribute.title("Leave call"),
      attribute.attribute("data-testid", "voice-leave"),
      event.on_click(on_click),
      ui.css([
        #("width", "30px"),
        #("height", "30px"),
        #("display", "inline-flex"),
        #("align-items", "center"),
        #("justify-content", "center"),
        #("padding", "0"),
        #("border", "none"),
        #("border-radius", "6px"),
        #("background", p.danger),
        #("color", "#ffffff"),
        #("flex-shrink", "0"),
        #("cursor", "pointer"),
        #("font-family", "inherit"),
      ]),
    ],
    [phone_hangup_icon()],
  )
}

fn mic_icon(muted: Bool) -> Element(msg) {
  let base = [
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
  ]
  let children = case muted {
    True -> [slash_line(), ..base]
    False -> base
  }
  element.namespaced(
    "http://www.w3.org/2000/svg",
    "svg",
    [
      attribute.attribute("width", "16"),
      attribute.attribute("height", "16"),
      attribute.attribute("viewBox", "0 0 16 16"),
      attribute.attribute("fill", "none"),
    ],
    children,
  )
}

// A 45° line corner-to-corner, used by the muted / deafened icon
// variants. Rendered first so the rest of the glyph sits over it.
fn slash_line() -> Element(msg) {
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

fn headphones_icon(deafened: Bool) -> Element(msg) {
  let base = [
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
  ]
  let children = case deafened {
    True -> [slash_line(), ..base]
    False -> base
  }
  element.namespaced(
    "http://www.w3.org/2000/svg",
    "svg",
    [
      attribute.attribute("width", "16"),
      attribute.attribute("height", "16"),
      attribute.attribute("viewBox", "0 0 16 16"),
      attribute.attribute("fill", "none"),
    ],
    children,
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
