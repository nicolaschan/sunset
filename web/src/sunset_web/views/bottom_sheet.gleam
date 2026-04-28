//// Reusable bottom-sheet primitive. Slides up from the bottom edge
//// when open; offscreen via translateY(100%) when closed. Tap-backdrop
//// dismisses. Used for message-details and voice-popover sheets on
//// phone, and for the reaction picker.

import lustre/attribute
import lustre/element.{type Element}
import lustre/element/html
import lustre/event
import sunset_web/theme.{type Palette}
import sunset_web/ui

pub fn view(
  palette p: Palette,
  open open: Bool,
  on_close on_close: msg,
  test_id test_id: String,
  content content: Element(msg),
) -> Element(msg) {
  let transform = case open {
    True -> "translateY(0)"
    False -> "translateY(100%)"
  }
  html.div([], [
    backdrop(open, on_close),
    html.div(
      [
        attribute.attribute("data-testid", test_id),
        attribute.attribute("role", "dialog"),
        attribute.attribute("aria-modal", "true"),
        attribute.attribute("aria-hidden", case open {
          True -> "false"
          False -> "true"
        }),
        ui.css([
          #("position", "fixed"),
          #("left", "0"),
          #("right", "0"),
          #("bottom", "0"),
          #("max-height", "75dvh"),
          #("background", p.surface),
          #("color", p.text),
          #("border-top", "1px solid " <> p.border),
          #("border-radius", "16px 16px 0 0"),
          #("box-shadow", p.shadow_lg),
          #("z-index", "40"),
          #("transform", transform),
          #("transition", "transform 220ms ease"),
          #("display", "flex"),
          #("flex-direction", "column"),
          #("overflow", "hidden"),
          #("padding-bottom", "env(safe-area-inset-bottom)"),
          #("overscroll-behavior", "contain"),
        ]),
      ],
      [
        drag_handle(p),
        html.div(
          [
            ui.css([
              #("flex", "1"),
              #("min-height", "0"),
              #("overflow-y", "auto"),
              #("overscroll-behavior", "contain"),
            ]),
          ],
          [content],
        ),
      ],
    ),
  ])
}

fn drag_handle(p: Palette) -> Element(msg) {
  // Cosmetic only — there's no swipe-down gesture in v1.
  html.div(
    [
      ui.css([
        #("display", "flex"),
        #("justify-content", "center"),
        #("padding", "8px 0 4px 0"),
      ]),
    ],
    [
      html.span(
        [
          ui.css([
            #("display", "inline-block"),
            #("width", "36px"),
            #("height", "4px"),
            #("border-radius", "999px"),
            #("background", p.border),
          ]),
        ],
        [],
      ),
    ],
  )
}

fn backdrop(open: Bool, on_close: msg) -> Element(msg) {
  html.div(
    [
      attribute.attribute("data-testid", "sheet-backdrop"),
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
        #("z-index", "39"),
      ]),
    ],
    [],
  )
}
