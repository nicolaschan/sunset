//// Wraps the `emoji-picker-element` web component as a Lustre element.
//// Lazy-loaded on first picker open; the `register_emoji_picker` FFI
//// dynamically imports the package and registers the custom element.

import gleam/dynamic/decode
import lustre/attribute
import lustre/element.{type Element}
import lustre/element/html
import lustre/event

/// Picker view. Dispatches `on_pick(emoji)` when the user clicks an
/// emoji in the picker. The wrapper (popover or bottom sheet) is
/// responsible for outside-click / Escape handling — the picker only
/// emits picks.
pub fn view(on_pick: fn(String) -> msg) -> Element(msg) {
  html.div(
    [
      attribute.attribute("data-testid", "full-emoji-picker-container"),
    ],
    [
      element.element(
        "emoji-picker",
        [
          attribute.attribute("data-testid", "full-emoji-picker"),
          // emoji-click is the picker's CustomEvent. The decoder pulls
          // event.detail.unicode (the emoji string).
          event.on("emoji-click", {
            use unicode <- decode.subfield(["detail", "unicode"], decode.string)
            decode.success(on_pick(unicode))
          }),
        ],
        [],
      ),
    ],
  )
}
