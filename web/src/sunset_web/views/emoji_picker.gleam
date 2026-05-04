//// Wraps the `emoji-picker-element` web component as a Lustre element.
//// Lazy-loaded on first picker open; the `register_emoji_picker` FFI
//// dynamically imports the package and registers the custom element.
////
//// Theming hooks:
////   * The custom element auto-detects light/dark via prefers-color-scheme
////     by default. We override that with an explicit `class="light"` /
////     `class="dark"` so the picker tracks the app's resolved theme
////     (System/Light/Dark preference) instead of the OS scheme.
////   * Sunset palette colours are pushed in via the documented CSS
////     custom properties on the host element (`--background`,
////     `--border-color`, `--button-hover-background`, etc.) so the
////     picker visually integrates with the surrounding chrome.

import gleam/dynamic/decode
import lustre/attribute
import lustre/element.{type Element}
import lustre/element/html
import lustre/event
import sunset_web/theme.{type Mode, type Palette, Dark}
import sunset_web/ui

/// Picker view. Dispatches `on_pick(emoji)` when the user clicks an
/// emoji in the picker. The wrapper (popover or bottom sheet) is
/// responsible for outside-click / Escape handling — the picker only
/// emits picks.
pub fn view(
  palette p: Palette,
  mode mode: Mode,
  on_pick on_pick: fn(String) -> msg,
) -> Element(msg) {
  let theme_class = case mode {
    Dark -> "dark"
    _ -> "light"
  }
  // The emoji-picker host has an intrinsic `width: min-content` (~400px).
  // On a phone bottom sheet that's narrower than 400px the picker would
  // overflow to the right; on one wider than 400px it would sit
  // left-aligned. A flex-row container with `justify-content: center`
  // anchors the picker in the middle on every viewport, and the inner
  // `max-width: 100%` clamps the picker so it doesn't push past the
  // sheet's edges on a narrow phone.
  html.div(
    [
      attribute.attribute("data-testid", "full-emoji-picker-container"),
      ui.css([
        #("display", "flex"),
        #("justify-content", "center"),
        #("width", "100%"),
        #("max-width", "100%"),
      ]),
    ],
    [
      element.element(
        "emoji-picker",
        [
          attribute.attribute("data-testid", "full-emoji-picker"),
          attribute.class(theme_class),
          // Palette-tinted CSS custom properties. The picker honours
          // these on the host element directly (per its docs). Setting
          // `--background` and friends on both modes ensures the
          // picker's chrome reads the same regardless of which class
          // (light/dark) we forced above.
          ui.css([
            #("max-width", "100%"),
            #("--background", p.surface),
            #("--border-color", p.border),
            #("--input-border-color", p.border),
            #("--input-font-color", p.text),
            #("--input-placeholder-color", p.text_muted),
            #("--button-hover-background", p.surface_alt),
            #("--button-active-background", p.accent_soft),
            #("--indicator-color", p.accent),
            #("--outline-color", p.accent),
          ]),
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
