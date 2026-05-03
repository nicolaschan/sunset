//// Landing screen — shown at `/` when the user has no joined rooms
//// (or has explicitly navigated back to the empty state).
////
//// A single centred card prompts for a room name. Submitting (button
//// click or Enter) calls back to JoinRoom, which adds the name to
//// joined_rooms, persists, and updates the URL fragment.

import gleam/dynamic/decode
import lustre/attribute
import lustre/element.{type Element}
import lustre/element/html
import lustre/event
import sunset_web/domain
import sunset_web/theme.{type Mode, type Palette, Dark, Light}
import sunset_web/ui

pub fn view(
  palette p: Palette,
  mode mode: Mode,
  viewport viewport: domain.Viewport,
  input input: String,
  noop noop: msg,
  on_input on_input: fn(String) -> msg,
  on_join on_join: fn(String) -> msg,
  on_toggle_mode on_toggle_mode: msg,
) -> Element(msg) {
  case viewport {
    domain.Phone ->
      phone_view(p, mode, input, noop, on_input, on_join, on_toggle_mode)
    domain.Desktop ->
      desktop_view(p, mode, input, noop, on_input, on_join, on_toggle_mode)
  }
}

fn desktop_view(
  p: Palette,
  mode: Mode,
  input: String,
  noop: msg,
  on_input: fn(String) -> msg,
  on_join: fn(String) -> msg,
  on_toggle_mode: msg,
) -> Element(msg) {
  html.div(
    [
      attribute.attribute("data-testid", "landing-view"),
      ui.css([
        #("position", "fixed"),
        #("inset", "0"),
        #("background", p.bg),
        #("color", p.text),
        #("font-family", theme.font_sans),
        #("font-size", "16.875px"),
        #("line-height", "1.45"),
        #("display", "flex"),
        #("align-items", "center"),
        #("justify-content", "center"),
        #("padding", "24px"),
        #("overflow", "hidden"),
      ]),
    ],
    [
      reset_style(),
      card(p, input, noop, on_input, on_join),
      mode_toggle_button(p, mode, on_toggle_mode),
    ],
  )
}

fn phone_view(
  p: Palette,
  mode: Mode,
  input: String,
  noop: msg,
  on_input: fn(String) -> msg,
  on_join: fn(String) -> msg,
  on_toggle_mode: msg,
) -> Element(msg) {
  html.div(
    [
      attribute.attribute("data-testid", "landing-view"),
      ui.css([
        #("position", "fixed"),
        #("inset", "0"),
        #("display", "flex"),
        #("flex-direction", "column"),
        #("justify-content", "center"),
        #("padding", "24px"),
        #("padding-top", "calc(env(safe-area-inset-top) + 24px)"),
        #("padding-bottom", "calc(env(safe-area-inset-bottom) + 24px)"),
        #("background", p.bg),
        #("color", p.text),
        #("font-family", theme.font_sans),
      ]),
    ],
    [
      html.h1(
        [
          ui.css([
            #("font-size", "44px"),
            #("font-weight", "700"),
            #("margin", "0 0 12px 0"),
            #("color", p.text),
          ]),
        ],
        [html.text("sunset.chat")],
      ),
      html.p(
        [
          ui.css([
            #("font-size", "18px"),
            #("color", p.text_muted),
            #("margin", "0 0 32px 0"),
          ]),
        ],
        [html.text("Pick a room name to join.")],
      ),
      html.input([
        attribute.attribute("data-testid", "landing-input"),
        attribute.attribute("type", "text"),
        attribute.value(input),
        attribute.placeholder("room-name"),
        event.on_input(on_input),
        on_enter_with_value(noop, on_join),
        ui.css([
          #("width", "100%"),
          #("box-sizing", "border-box"),
          #("padding", "14px 16px"),
          #("font-size", "18px"),
          #("font-family", "inherit"),
          #("border", "1px solid " <> p.border),
          #("border-radius", "10px"),
          #("background", p.surface),
          #("color", p.text),
          #("margin-bottom", "12px"),
        ]),
      ]),
      html.button(
        [
          attribute.attribute("data-testid", "landing-join"),
          attribute.disabled(input == ""),
          event.on_click(on_join(input)),
          ui.css([
            #("width", "100%"),
            #("padding", "14px"),
            #("font-size", "18px"),
            #("font-weight", "600"),
            #("font-family", "inherit"),
            #("border", "none"),
            #("border-radius", "10px"),
            #("background", p.accent),
            #("color", p.accent_ink),
            #("cursor", case input {
              "" -> "default"
              _ -> "pointer"
            }),
          ]),
        ],
        [html.text("Join")],
      ),
      html.button(
        [
          attribute.attribute("data-testid", "theme-toggle"),
          event.on_click(on_toggle_mode),
          ui.css([
            #("position", "fixed"),
            #("top", "calc(env(safe-area-inset-top) + 12px)"),
            #("right", "12px"),
            #("padding", "8px 12px"),
            #("border", "1px solid " <> p.border),
            #("background", p.surface),
            #("color", p.text_muted),
            #("border-radius", "999px"),
            #("font-family", "inherit"),
            #("font-size", "13px"),
            #("cursor", "pointer"),
          ]),
        ],
        [
          html.text(case mode {
            Light -> "🌙"
            Dark -> "☀"
          }),
        ],
      ),
    ],
  )
}

fn card(
  p: Palette,
  input: String,
  noop: msg,
  on_input: fn(String) -> msg,
  on_join: fn(String) -> msg,
) -> Element(msg) {
  html.div(
    [
      ui.css([
        #("width", "100%"),
        #("max-width", "440px"),
        #("background", p.surface),
        #("border", "1px solid " <> p.border),
        #("border-radius", "12px"),
        #("box-shadow", p.shadow_lg),
        #("padding", "32px 32px 28px 32px"),
        #("display", "flex"),
        #("flex-direction", "column"),
        #("gap", "20px"),
      ]),
    ],
    [
      branding(p),
      tagline(p),
      join_form(p, input, noop, on_input, on_join),
      hint(p),
    ],
  )
}

fn branding(p: Palette) -> Element(msg) {
  html.div(
    [
      ui.css([
        #("display", "flex"),
        #("align-items", "center"),
        #("gap", "12px"),
      ]),
    ],
    [
      html.span([ui.css([#("color", p.accent), #("display", "inline-flex")])], [
        logo(36),
      ]),
      html.span(
        [
          ui.css([
            #("font-weight", "600"),
            #("font-size", "26px"),
            #("letter-spacing", "-0.02em"),
            #("color", p.text),
          ]),
        ],
        [html.text("sunset.chat")],
      ),
    ],
  )
}

fn tagline(p: Palette) -> Element(msg) {
  html.div(
    [
      ui.css([
        #("font-size", "16px"),
        #("color", p.text_muted),
        #("line-height", "1.5"),
      ]),
    ],
    [
      html.text("Peer-to-peer chat — type a room name to enter or create one."),
    ],
  )
}

fn join_form(
  p: Palette,
  input: String,
  noop: msg,
  on_input: fn(String) -> msg,
  on_join: fn(String) -> msg,
) -> Element(msg) {
  let trimmed_empty = input == "" || input == " "
  html.div(
    [
      ui.css([
        #("display", "flex"),
        #("gap", "8px"),
        #("align-items", "center"),
      ]),
    ],
    [
      html.input([
        attribute.attribute("data-testid", "landing-input"),
        attribute.value(input),
        attribute.placeholder("Room name (e.g. dusk-collective)"),
        attribute.attribute("autofocus", ""),
        event.on_input(on_input),
        on_enter_with_value(noop, on_join),
        ui.css([
          #("flex", "1"),
          #("min-width", "0"),
          #("box-sizing", "border-box"),
          #("background", p.surface_alt),
          #("border", "1px solid " <> p.border),
          #("border-radius", "8px"),
          #("padding", "12px 14px"),
          #("font-family", "inherit"),
          #("font-size", "16.875px"),
          #("color", p.text),
          #("outline", "none"),
        ]),
      ]),
      html.button(
        [
          attribute.attribute("data-testid", "landing-join"),
          attribute.disabled(trimmed_empty),
          event.on_click(on_join(input)),
          ui.css([
            #("padding", "12px 18px"),
            #("background", case trimmed_empty {
              True -> p.surface_alt
              False -> p.accent
            }),
            #("color", case trimmed_empty {
              True -> p.text_faint
              False -> p.accent_ink
            }),
            #(
              "border",
              "1px solid "
                <> case trimmed_empty {
                True -> p.border_soft
                False -> p.accent_deep
              },
            ),
            #("border-radius", "8px"),
            #("font-family", "inherit"),
            #("font-size", "15.625px"),
            #("font-weight", "600"),
            #("cursor", case trimmed_empty {
              True -> "not-allowed"
              False -> "pointer"
            }),
          ]),
        ],
        [html.text("Join")],
      ),
    ],
  )
}

fn hint(p: Palette) -> Element(msg) {
  html.div(
    [
      ui.css([
        #("font-size", "13.75px"),
        #("color", p.text_faint),
      ]),
    ],
    [html.text("Press ⏎ to join. Your rooms list is stored locally.")],
  )
}

fn mode_toggle_button(p: Palette, mode: Mode, on_toggle: msg) -> Element(msg) {
  // Same style + position as the room view's toggle so the page chrome
  // stays consistent between the two top-level views.
  html.button(
    [
      attribute.attribute("data-testid", "theme-toggle"),
      attribute.title(case mode {
        Light -> "Switch to dark mode"
        Dark -> "Switch to light mode"
      }),
      event.on_click(on_toggle),
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
        #("background", p.surface),
        #("color", p.text_muted),
        #("border", "1px solid " <> p.border),
        #("border-radius", "999px"),
        #("font-family", "inherit"),
        #("line-height", "1"),
        #("cursor", "pointer"),
        #("box-shadow", p.shadow),
        #("z-index", "10"),
      ]),
    ],
    [
      case mode {
        Light -> sun_icon()
        Dark -> moon_icon()
      },
    ],
  )
}

/// Bind a keydown decoder that reads target.value live and dispatches
/// `on_join(value)` only on Enter (other keys yield `noop`). Reading
/// the value out of the event itself avoids any staleness from the
/// closure being captured against an older render's state.
///
/// Enter calls `preventDefault` on the keydown so the keystroke can't
/// bleed through into a different focused element after Lustre's
/// re-render replaces this input. Without this, pressing Enter on the
/// landing input on phone navigates to the room AND inserts a newline
/// into the (newly-mounted, autofocus'd) composer textarea, because
/// the browser's default keydown action runs after the DOM mutation
/// has moved focus.
fn on_enter_with_value(
  noop: msg,
  on_join: fn(String) -> msg,
) -> attribute.Attribute(msg) {
  event.advanced("keydown", {
    use key <- decode.subfield(["key"], decode.string)
    use value <- decode.subfield(["target", "value"], decode.string)
    decode.success(case key {
      "Enter" ->
        event.handler(
          on_join(value),
          prevent_default: True,
          stop_propagation: False,
        )
      _ -> event.handler(noop, prevent_default: False, stop_propagation: False)
    })
  })
}

fn reset_style() -> Element(msg) {
  // Same body reset as the room view, duplicated here so the landing
  // screen renders without margins/scrollbars even when the chat shell
  // isn't mounted.
  html.style(
    [],
    "html, body { margin: 0; padding: 0; height: 100%; overflow: hidden; }
     #app { height: 100%; }
     *, *::before, *::after { box-sizing: border-box; }",
  )
}

fn logo(size: Int) -> Element(msg) {
  let s = case size {
    36 -> "36"
    _ -> "22"
  }
  element.namespaced(
    "http://www.w3.org/2000/svg",
    "svg",
    [
      attribute.attribute("width", s),
      attribute.attribute("height", s),
      attribute.attribute("viewBox", "0 0 28 28"),
      attribute.attribute("fill", "none"),
    ],
    [
      element.namespaced(
        "http://www.w3.org/2000/svg",
        "circle",
        [
          attribute.attribute("cx", "14"),
          attribute.attribute("cy", "14"),
          attribute.attribute("r", "6.5"),
          attribute.attribute("fill", "currentColor"),
          attribute.attribute("opacity", "0.28"),
        ],
        [],
      ),
      element.namespaced(
        "http://www.w3.org/2000/svg",
        "circle",
        [
          attribute.attribute("cx", "14"),
          attribute.attribute("cy", "14"),
          attribute.attribute("r", "3.6"),
          attribute.attribute("fill", "currentColor"),
        ],
        [],
      ),
      element.namespaced(
        "http://www.w3.org/2000/svg",
        "line",
        [
          attribute.attribute("x1", "3"),
          attribute.attribute("y1", "20.5"),
          attribute.attribute("x2", "25"),
          attribute.attribute("y2", "20.5"),
          attribute.attribute("stroke", "currentColor"),
          attribute.attribute("stroke-width", "1.6"),
          attribute.attribute("stroke-linecap", "round"),
        ],
        [],
      ),
      element.namespaced(
        "http://www.w3.org/2000/svg",
        "line",
        [
          attribute.attribute("x1", "6"),
          attribute.attribute("y1", "24"),
          attribute.attribute("x2", "22"),
          attribute.attribute("y2", "24"),
          attribute.attribute("stroke", "currentColor"),
          attribute.attribute("stroke-width", "1.6"),
          attribute.attribute("stroke-linecap", "round"),
          attribute.attribute("opacity", "0.5"),
        ],
        [],
      ),
    ],
  )
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
