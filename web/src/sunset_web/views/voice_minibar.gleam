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
) -> Element(msg) {
  html.button(
    [
      attribute.attribute("data-testid", "voice-minibar"),
      attribute.attribute("aria-label", "Voice controls for " <> channel_name),
      event.on_click(on_open),
      ui.css([
        #("display", "flex"),
        #("align-items", "center"),
        #("gap", "8px"),
        #("box-sizing", "border-box"),
        #("width", "100%"),
        #("padding", "8px 12px"),
        #("background", p.accent),
        #("color", p.accent_ink),
        #("border", "none"),
        #("border-bottom", "1px solid " <> p.border),
        #("font-family", "inherit"),
        #("font-size", "14px"),
        #("font-weight", "600"),
        #("text-align", "left"),
        #("cursor", "pointer"),
        #("flex-shrink", "0"),
      ]),
    ],
    [
      label(channel_name),
      icon_button("Mute mic", mic_icon()),
      icon_button("Deafen", headphones_icon()),
      leave_button(),
    ],
  )
}

fn label(channel_name: String) -> Element(msg) {
  html.span(
    [
      ui.css([
        #("flex", "1"),
        #("min-width", "0"),
        #("display", "flex"),
        #("align-items", "center"),
        #("gap", "6px"),
        #("white-space", "nowrap"),
        #("overflow", "hidden"),
        #("text-overflow", "ellipsis"),
      ]),
    ],
    [
      live_dot(),
      html.span([], [html.text(channel_name)]),
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

/// Icon affordance — visual only. The whole row is the click target;
/// these are rendered inside the row's <button> as styled spans.
fn icon_button(title: String, icon: Element(msg)) -> Element(msg) {
  html.span(
    [
      attribute.title(title),
      ui.css([
        #("width", "30px"),
        #("height", "30px"),
        #("display", "inline-flex"),
        #("align-items", "center"),
        #("justify-content", "center"),
        #("border-radius", "6px"),
        #("background", "rgba(255, 255, 255, 0.18)"),
        #("color", "currentColor"),
        #("flex-shrink", "0"),
      ]),
    ],
    [icon],
  )
}

fn leave_button() -> Element(msg) {
  html.span(
    [
      attribute.title("Leave call"),
      ui.css([
        #("width", "30px"),
        #("height", "30px"),
        #("display", "inline-flex"),
        #("align-items", "center"),
        #("justify-content", "center"),
        #("border-radius", "6px"),
        #("background", "#a8242c"),
        #("color", "#ffffff"),
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

fn headphones_icon() -> Element(msg) {
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
