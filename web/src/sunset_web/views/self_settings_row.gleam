//// Self / settings row — pinned at the bottom of the right (members)
//// rail. Click anywhere on the row to open the settings popover (theme,
//// reset, display name). The avatar + display name reads as "this is
//// you"; the row doubles as the entry point to your own preferences.
////
//// Previously lived at the bottom of the rooms (left) rail. Moved here
//// so the user's own affordance sits in the same column as the rest of
//// the member roster, which is conceptually where "people" — including
//// yourself — belong.

import gleam/list
import gleam/option.{type Option}
import lustre/attribute
import lustre/element.{type Element}
import lustre/element/html
import lustre/event
import sunset_web/domain.{type Member}
import sunset_web/theme.{type Palette}
import sunset_web/ui

/// Render the row for the current user, or nothing if the local member
/// hasn't been resolved yet (model bootstrap race). 64px tall to match
/// the matching bottom rows in the other rails / composer.
pub fn view(
  palette p: Palette,
  members ms: List(Member),
  on_open_settings on_open_settings: msg,
) -> Element(msg) {
  let you = ms |> list.find(fn(m) { m.you }) |> option.from_result
  let your_name = you |> option.map(fn(a) { a.name }) |> option.unwrap("?")
  row(p, you, your_name, on_open_settings)
}

fn row(
  p: Palette,
  _you: Option(Member),
  your_name: String,
  on_open_settings: msg,
) -> Element(msg) {
  html.button(
    [
      attribute.attribute("data-testid", "you-row"),
      attribute.title("Settings"),
      attribute.attribute("aria-label", "Open settings"),
      event.on_click(on_open_settings),
      ui.css([
        #("box-sizing", "border-box"),
        #("height", "64px"),
        #("flex-shrink", "0"),
        #("display", "flex"),
        #("align-items", "center"),
        #("justify-content", "flex-start"),
        #("gap", "8px"),
        #("padding", "0 14px"),
        #("border", "none"),
        #("border-top", "1px solid " <> p.border_soft),
        #("background", p.surface),
        #("color", p.text),
        #("font-family", "inherit"),
        #("font-size", "16.25px"),
        #("text-align", "left"),
        #("cursor", "pointer"),
        #("width", "100%"),
      ]),
    ],
    [
      user_avatar(p),
      html.span(
        [
          ui.css([
            #("flex", "1"),
            #("min-width", "0"),
            #("font-weight", "500"),
            #("color", p.text),
            #("white-space", "nowrap"),
            #("overflow", "hidden"),
            #("text-overflow", "ellipsis"),
          ]),
        ],
        [html.text(your_name)],
      ),
    ],
  )
}

fn user_avatar(p: Palette) -> Element(msg) {
  html.span(
    [
      ui.css([
        #("display", "inline-flex"),
        #("align-items", "center"),
        #("justify-content", "center"),
        #("width", "28px"),
        #("height", "28px"),
        #("border-radius", "999px"),
        #("background", p.surface_alt),
        #("color", p.text_muted),
        #("flex-shrink", "0"),
      ]),
    ],
    [user_icon()],
  )
}

fn user_icon() -> Element(msg) {
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
        "circle",
        [
          attribute.attribute("cx", "8"),
          attribute.attribute("cy", "6"),
          attribute.attribute("r", "2.6"),
          attribute.attribute("stroke", "currentColor"),
          attribute.attribute("stroke-width", "1.4"),
        ],
        [],
      ),
      element.namespaced(
        "http://www.w3.org/2000/svg",
        "path",
        [
          attribute.attribute("d", "M2.5 13.5a5.5 5.5 0 0111 0"),
          attribute.attribute("stroke", "currentColor"),
          attribute.attribute("stroke-width", "1.4"),
          attribute.attribute("stroke-linecap", "round"),
        ],
        [],
      ),
    ],
  )
}
