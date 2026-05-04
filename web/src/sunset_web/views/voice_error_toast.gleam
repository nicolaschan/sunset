//// Floating error toast shown when mic permission is denied or voice fails
//// to start. Appears top-right; dismissed via close button or `ResetVoiceError`.

import lustre/attribute
import lustre/element.{type Element}
import lustre/element/html
import lustre/event
import sunset_web/theme.{type Palette}
import sunset_web/ui

pub fn view(
  palette p: Palette,
  message msg: String,
  on_close on_close: m,
) -> Element(m) {
  html.div(
    [
      attribute.attribute("data-testid", "voice-error-toast"),
      attribute.attribute("role", "alert"),
      ui.css([
        #("position", "fixed"),
        #("top", "16px"),
        #("right", "16px"),
        #("z-index", "200"),
        #("display", "flex"),
        #("align-items", "center"),
        #("gap", "10px"),
        #("padding", "12px 14px"),
        #("background", p.surface),
        #("border", "1px solid " <> p.border),
        #("border-left", "4px solid " <> p.warn),
        #("border-radius", "8px"),
        #("box-shadow", p.shadow_lg),
        #("max-width", "360px"),
        #("font-size", "14px"),
        #("color", p.text),
      ]),
    ],
    [
      html.span(
        [
          ui.css([
            #("flex", "1"),
            #("line-height", "1.4"),
          ]),
        ],
        [html.text(msg)],
      ),
      html.button(
        [
          attribute.attribute("aria-label", "Dismiss"),
          attribute.attribute("data-testid", "voice-error-toast-close"),
          event.on_click(on_close),
          ui.css([
            #("padding", "0"),
            #("border", "none"),
            #("background", "transparent"),
            #("color", p.text_muted),
            #("cursor", "pointer"),
            #("font-family", "inherit"),
            #("font-size", "18px"),
            #("line-height", "1"),
            #("flex-shrink", "0"),
          ]),
        ],
        [html.text("×")],
      ),
    ],
  )
}
