//// 4-column app shell (rooms · channels · main · members).
////
//// Children come in as four already-rendered Lustre elements; this module
//// only owns the outer grid + body chrome + theme-toggle button.

import lustre/attribute
import lustre/element.{type Element}
import lustre/element/html
import lustre/event
import sunset_web/theme.{type Mode, type Palette, Dark, Light}
import sunset_web/ui

pub fn view(
  mode: Mode,
  palette: Palette,
  rooms_collapsed: Bool,
  detail_open: Bool,
  toggle_mode: msg,
  rooms: Element(msg),
  channels: Element(msg),
  main: Element(msg),
  right_rail: Element(msg),
  overlay: Element(msg),
) -> Element(msg) {
  let rooms_col = case rooms_collapsed {
    True -> "54px"
    False -> "260px"
  }
  let right_col = case detail_open {
    True -> "320px"
    False -> "220px"
  }
  let grid_template = rooms_col <> " 230px 1fr " <> right_col

  html.div(
    [
      ui.css([
        #("position", "fixed"),
        #("inset", "0"),
        #("background", palette.bg),
        #("color", palette.text),
        #("font-family", theme.font_sans),
        #("font-size", "16.875px"),
        #("line-height", "1.45"),
        #("overflow", "hidden"),
      ]),
    ],
    [
      global_reset(),
      html.div(
        [
          ui.css([
            #("display", "grid"),
            #("grid-template-columns", grid_template),
            #("grid-template-rows", "100vh"),
            #("height", "100vh"),
            #("overflow", "hidden"),
            #("transition", "grid-template-columns 220ms ease"),
          ]),
        ],
        [
          rooms,
          channels,
          main,
          right_rail,
        ],
      ),
      theme_toggle(mode, palette, toggle_mode),
      overlay,
    ],
  )
}

/// Inline browser-default reset. Lustre dev tools' generated HTML doesn't
/// include `body { margin: 0 }`, and our shell uses `position: fixed; inset:
/// 0` to claim the full viewport, so the default 8px body margin would
/// otherwise show up as a window-wide gap (and a vertical scrollbar where
/// the viewport overflows).
///
/// Also defines a couple of hover-state rules — Lustre's inline `style`
/// attributes can't express :hover, so the rules live here and views
/// opt in via `attribute.class("...")`.
fn global_reset() -> Element(msg) {
  html.style(
    [],
    "html, body { margin: 0; padding: 0; height: 100%; overflow: hidden; }
     #app { height: 100%; }
     *, *::before, *::after { box-sizing: border-box; }
     .msg-row .msg-actions {
       opacity: 0;
       pointer-events: none;
       transition: opacity 120ms ease;
     }
     .msg-row:hover .msg-actions,
     .msg-row.is-active .msg-actions {
       opacity: 1;
       pointer-events: auto;
     }
     .room-row .room-delete {
       opacity: 0;
       pointer-events: none;
       transition: opacity 120ms ease;
     }
     .room-row:hover .room-delete,
     .room-row:focus-within .room-delete {
       opacity: 1;
       pointer-events: auto;
     }",
  )
}

fn theme_toggle(mode: Mode, palette: Palette, toggle_mode: msg) -> Element(msg) {
  html.button(
    [
      // Pinned to the bottom-right of the viewport so it never collides
      // with the message-details panel header (which lives at the top of
      // the right column). Icon-only — the label was both redundant with
      // the icon and a horizontal-overflow risk in the corner.
      ui.css([
        #("position", "fixed"),
        #("bottom", "14px"),
        #("right", "14px"),
        #("display", "inline-flex"),
        #("align-items", "center"),
        #("justify-content", "center"),
        #("width", "32px"),
        #("height", "32px"),
        #("padding", "0"),
        #("background", palette.surface),
        #("color", palette.text_muted),
        #("border", "1px solid " <> palette.border),
        #("border-radius", "999px"),
        #("font-family", "inherit"),
        #("line-height", "1"),
        #("cursor", "pointer"),
        #("box-shadow", palette.shadow),
        #("z-index", "10"),
      ]),
      event.on_click(toggle_mode),
      attribute.title(case mode {
        Light -> "Switch to dark mode"
        Dark -> "Switch to light mode"
      }),
      attribute.attribute("aria-label", case mode {
        Light -> "Switch to dark mode"
        Dark -> "Switch to light mode"
      }),
      attribute.attribute("data-testid", "theme-toggle"),
    ],
    [icon_for_mode(mode)],
  )
}

fn icon_for_mode(mode: Mode) -> Element(msg) {
  case mode {
    Light -> sun_icon()
    Dark -> moon_icon()
  }
}

fn sun_icon() -> Element(msg) {
  element.namespaced(
    "http://www.w3.org/2000/svg",
    "svg",
    [
      attribute.attribute("width", "12"),
      attribute.attribute("height", "12"),
      attribute.attribute("viewBox", "0 0 16 16"),
      attribute.attribute("fill", "none"),
    ],
    [
      element.namespaced(
        "http://www.w3.org/2000/svg",
        "circle",
        [
          attribute.attribute("cx", "8"),
          attribute.attribute("cy", "8"),
          attribute.attribute("r", "3"),
          attribute.attribute("stroke", "currentColor"),
          attribute.attribute("stroke-width", "1.4"),
        ],
        [],
      ),
      element.namespaced(
        "http://www.w3.org/2000/svg",
        "path",
        [
          attribute.attribute(
            "d",
            "M8 1.5v1.6M8 12.9v1.6M14.5 8h-1.6M3.1 8H1.5M12.6 3.4l-1.1 1.1M4.5 11.5l-1.1 1.1M12.6 12.6l-1.1-1.1M4.5 4.5L3.4 3.4",
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

fn moon_icon() -> Element(msg) {
  element.namespaced(
    "http://www.w3.org/2000/svg",
    "svg",
    [
      attribute.attribute("width", "12"),
      attribute.attribute("height", "12"),
      attribute.attribute("viewBox", "0 0 16 16"),
      attribute.attribute("fill", "none"),
    ],
    [
      element.namespaced(
        "http://www.w3.org/2000/svg",
        "path",
        [
          attribute.attribute("d", "M13.5 9.5A6 6 0 016.5 2.5a6 6 0 107 7z"),
          attribute.attribute("stroke", "currentColor"),
          attribute.attribute("stroke-width", "1.4"),
          attribute.attribute("stroke-linejoin", "round"),
        ],
        [],
      ),
    ],
  )
}
