//// Floating mini-bar shown in the phone chat view when the user is
//// in a voice call. PiP-style pill showing the active channel + a
//// mic icon. Tapping opens the user's own voice sheet so they can
//// mute / leave from anywhere.

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
) -> Element(msg) {
  html.button(
    [
      attribute.attribute("data-testid", "voice-minibar"),
      attribute.attribute("aria-label", "Voice controls for " <> channel_name),
      event.on_click(on_open),
      ui.css([
        #("position", "fixed"),
        #("right", "12px"),
        #("bottom", "calc(env(safe-area-inset-bottom) + 76px)"),
        #("display", "inline-flex"),
        #("align-items", "center"),
        #("gap", "8px"),
        #("padding", "8px 14px"),
        #("background", p.accent),
        #("color", p.accent_ink),
        #("border", "none"),
        #("border-radius", "999px"),
        #("box-shadow", p.shadow_lg),
        #("font-family", "inherit"),
        #("font-size", "14px"),
        #("font-weight", "600"),
        #("cursor", "pointer"),
        #("z-index", "20"),
      ]),
    ],
    [
      live_dot(),
      html.span([], [html.text(channel_name)]),
      mic_icon(),
    ],
  )
}

fn live_dot() -> Element(msg) {
  html.span(
    [
      ui.css([
        #("width", "8px"),
        #("height", "8px"),
        #("border-radius", "999px"),
        #("background", "#ffffff"),
        #("flex-shrink", "0"),
      ]),
    ],
    [],
  )
}

fn mic_icon() -> Element(msg) {
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
