//// Full-screen image overlay ("theater mode") shown when the user clicks
//// a chat image. A click anywhere on the backdrop or pressing Escape
//// dismisses it; a click on the enlarged image itself is intercepted so
//// it stays open. The overlay autofocuses on open so the Escape key works
//// regardless of what was focused before.

import gleam/dynamic/decode
import lustre/attribute
import lustre/element.{type Element}
import lustre/element/html
import lustre/event
import sunset_web/sunset
import sunset_web/theme.{type Palette}
import sunset_web/ui

pub fn view(
  palette p: Palette,
  mime_type mime_type: String,
  data_base64 data_base64: String,
  on_close on_close: msg,
  noop noop: msg,
) -> Element(msg) {
  html.div(
    [
      attribute.attribute("data-testid", "image-theater"),
      attribute.attribute("role", "dialog"),
      attribute.attribute("aria-modal", "true"),
      attribute.attribute("aria-label", "Enlarged image"),
      attribute.attribute("tabindex", "-1"),
      attribute.autofocus(True),
      event.on_click(on_close),
      event.advanced("keydown", {
        use key <- decode.subfield(["key"], decode.string)
        decode.success(case key {
          "Escape" ->
            event.handler(
              on_close,
              prevent_default: True,
              stop_propagation: True,
            )
          _ ->
            event.handler(noop, prevent_default: False, stop_propagation: False)
        })
      }),
      ui.css([
        #("position", "fixed"),
        #("inset", "0"),
        #("z-index", "100"),
        #("display", "flex"),
        #("align-items", "center"),
        #("justify-content", "center"),
        #("background", "rgba(0, 0, 0, 0.85)"),
        #("padding", "24px"),
        #("box-sizing", "border-box"),
        // Hide the focus ring on the overlay itself — the dialog has
        // no visible focusable affordance, the focus is a keyboard
        // plumbing detail so Escape works.
        #("outline", "none"),
        // Pad below the iOS home-indicator / Android nav bar so the
        // image doesn't run underneath system UI on phones.
        #("padding-bottom", "max(24px, env(safe-area-inset-bottom))"),
        #("padding-top", "max(24px, env(safe-area-inset-top))"),
      ]),
    ],
    [
      html.img([
        attribute.src(sunset.image_data_url(mime_type, data_base64)),
        attribute.alt("enlarged image"),
        attribute.attribute("data-testid", "image-theater-image"),
        // Stop click propagation so a click on the image doesn't bubble
        // to the overlay's on_close handler — only clicks on the
        // backdrop close the theater.
        event.advanced("click", {
          decode.success(event.handler(
            noop,
            prevent_default: False,
            stop_propagation: True,
          ))
        }),
        ui.css([
          #("max-width", "100%"),
          #("max-height", "100%"),
          #("object-fit", "contain"),
          #("display", "block"),
          #("border-radius", "4px"),
          #("box-shadow", p.shadow_lg),
          // Keep the cursor as default over the image so users see it's
          // a distinct surface from the dismissible backdrop.
          #("cursor", "default"),
        ]),
      ]),
    ],
  )
}
