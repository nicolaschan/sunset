//// Reusable side-drawer primitive. Always rendered in the DOM —
//// closed state translates the wrapper offscreen so CSS transitions
//// handle the slide. The host owns drawer state; this module only
//// renders a wrapper + backdrop.

import lustre/attribute
import lustre/element.{type Element}
import lustre/element/html
import lustre/event
import sunset_web/theme.{type Palette}
import sunset_web/ui

pub type Side {
  Left
  Right
}

pub fn view(
  palette p: Palette,
  open open: Bool,
  side side: Side,
  on_close on_close: msg,
  test_id test_id: String,
  label label: String,
  content content: Element(msg),
) -> Element(msg) {
  let translate_closed = case side {
    Left -> "translateX(-100%)"
    Right -> "translateX(100%)"
  }
  let transform = case open {
    True -> "translateX(0)"
    False -> translate_closed
  }
  let edge_anchor = case side {
    Left -> #("left", "0")
    Right -> #("right", "0")
  }

  html.div([], [
    backdrop(open, on_close),
    html.aside(
      [
        attribute.attribute("data-testid", test_id),
        attribute.attribute("aria-label", label),
        attribute.attribute("aria-hidden", case open {
          True -> "false"
          False -> "true"
        }),
        ui.css([
          #("position", "fixed"),
          #("top", "0"),
          edge_anchor,
          #("height", "100dvh"),
          #("width", "84vw"),
          #("max-width", "320px"),
          // Pad the drawer's interior by the iOS safe-area insets so
          // the status bar / home indicator never sit over interactive
          // chrome inside the drawer (rooms search, you-row settings
          // trigger, etc.). The drawer's own background still extends
          // edge-to-edge under the inset, which gives the cleaner look
          // the `apple-mobile-web-app-status-bar-style: black-
          // translucent` meta is designed for. Mirrors the safe-area
          // handling already used by phone_header for the main shell.
          #("padding-top", "env(safe-area-inset-top)"),
          #("padding-bottom", "env(safe-area-inset-bottom)"),
          #("background", p.surface),
          #("color", p.text),
          #("border-right", case side {
            Left -> "1px solid " <> p.border
            Right -> "0"
          }),
          #("border-left", case side {
            Right -> "1px solid " <> p.border
            Left -> "0"
          }),
          #("z-index", "30"),
          #("transform", transform),
          #("transition", "transform 220ms ease"),
          #("display", "flex"),
          #("flex-direction", "column"),
          #("overflow", "hidden"),
          #("overscroll-behavior", "contain"),
        ]),
      ],
      [content],
    ),
  ])
}

fn backdrop(open: Bool, on_close: msg) -> Element(msg) {
  html.div(
    [
      attribute.attribute("data-testid", "drawer-backdrop"),
      event.on_click(on_close),
      ui.css([
        #("position", "fixed"),
        #("inset", "0"),
        #("background", "rgba(0, 0, 0, 0.4)"),
        #("opacity", case open {
          True -> "1"
          False -> "0"
        }),
        #("pointer-events", case open {
          True -> "auto"
          False -> "none"
        }),
        #("transition", "opacity 220ms ease"),
        #("z-index", "29"),
      ]),
    ],
    [],
  )
}
