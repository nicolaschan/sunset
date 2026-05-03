//// Settings popover, opened by clicking the "you" row at the bottom of
//// the rooms rail. Renders a small panel with three theme buttons
//// (System / Light / Dark) and a destructive "reset local state"
//// button that wipes localStorage + reloads.
////
//// On desktop the popover is anchored above the rail (Floating); on
//// phone it's wrapped in a `bottom_sheet.view` by the host.

import lustre/attribute
import lustre/element.{type Element}
import lustre/element/html
import lustre/event
import sunset_web/theme.{
  type Palette, type Pref, DarkPref, LightPref, System,
}
import sunset_web/ui

pub type Placement {
  Floating
  InSheet
}

pub fn view(
  palette p: Palette,
  pref pref: Pref,
  placement placement: Placement,
  on_select_pref on_select_pref: fn(Pref) -> msg,
  on_reset on_reset: msg,
  on_close on_close: msg,
) -> Element(msg) {
  let body = body_view(p, pref, on_select_pref, on_reset, on_close)
  case placement {
    Floating ->
      html.div(
        [
          attribute.attribute("data-testid", "settings-popover"),
          ui.css([
            #("position", "fixed"),
            // Anchor the popover above the rooms-rail you_row (which
            // is 64px tall, pinned to the bottom of the rail). Leaving
            // a small gap above the row keeps the popover visually
            // tied to the trigger without overlapping it.
            #("bottom", "76px"),
            #("left", "12px"),
            #("width", "240px"),
            #("background", p.surface),
            #("color", p.text),
            #("border", "1px solid " <> p.border),
            #("border-radius", "10px"),
            #("box-shadow", p.shadow_lg),
            #("z-index", "20"),
          ]),
        ],
        [body],
      )
    InSheet ->
      html.div(
        [
          attribute.attribute("data-testid", "settings-popover"),
          ui.css([
            #("display", "flex"),
            #("flex-direction", "column"),
            #("background", p.surface),
            #("color", p.text),
          ]),
        ],
        [body],
      )
  }
}

fn body_view(
  p: Palette,
  pref: Pref,
  on_select_pref: fn(Pref) -> msg,
  on_reset: msg,
  on_close: msg,
) -> Element(msg) {
  html.div(
    [
      ui.css([
        #("display", "flex"),
        #("flex-direction", "column"),
        #("gap", "12px"),
        #("padding", "14px 16px 16px 16px"),
      ]),
    ],
    [
      header(p, on_close),
      theme_section(p, pref, on_select_pref),
      reset_section(p, on_reset),
    ],
  )
}

fn header(p: Palette, on_close: msg) -> Element(msg) {
  html.div(
    [
      ui.css([
        #("display", "flex"),
        #("align-items", "center"),
        #("justify-content", "space-between"),
        #("gap", "8px"),
      ]),
    ],
    [
      html.span(
        [
          ui.css([
            #("font-weight", "600"),
            #("font-size", "16px"),
            #("color", p.text),
          ]),
        ],
        [html.text("Settings")],
      ),
      html.button(
        [
          attribute.attribute("data-testid", "settings-popover-close"),
          attribute.title("Close settings"),
          attribute.attribute("aria-label", "Close settings"),
          event.on_click(on_close),
          ui.css([
            #("background", "transparent"),
            #("border", "none"),
            #("color", p.text_faint),
            #("cursor", "pointer"),
            #("font-size", "16px"),
            #("padding", "0 4px"),
          ]),
        ],
        [html.text("×")],
      ),
    ],
  )
}

fn theme_section(
  p: Palette,
  pref: Pref,
  on_select_pref: fn(Pref) -> msg,
) -> Element(msg) {
  html.div(
    [
      ui.css([
        #("display", "flex"),
        #("flex-direction", "column"),
        #("gap", "6px"),
      ]),
    ],
    [
      section_label(p, "Theme"),
      html.div(
        [
          ui.css([
            #("display", "grid"),
            #("grid-template-columns", "repeat(3, 1fr)"),
            #("gap", "6px"),
          ]),
        ],
        [
          theme_button(p, "System", "system", System, pref, on_select_pref),
          theme_button(p, "Light", "light", LightPref, pref, on_select_pref),
          theme_button(p, "Dark", "dark", DarkPref, pref, on_select_pref),
        ],
      ),
    ],
  )
}

fn section_label(p: Palette, label: String) -> Element(msg) {
  html.span(
    [
      ui.css([
        #("font-size", "13px"),
        #("font-weight", "600"),
        #("text-transform", "uppercase"),
        #("letter-spacing", "0.04em"),
        #("color", p.text_faint),
      ]),
    ],
    [html.text(label)],
  )
}

fn theme_button(
  p: Palette,
  label: String,
  test_id_suffix: String,
  this_pref: Pref,
  current: Pref,
  on_select_pref: fn(Pref) -> msg,
) -> Element(msg) {
  let active = pref_eq(this_pref, current)
  let bg = case active {
    True -> p.accent_soft
    False -> p.surface_alt
  }
  let color = case active {
    True -> p.accent_deep
    False -> p.text_muted
  }
  let border = case active {
    True -> p.accent
    False -> p.border_soft
  }
  html.button(
    [
      attribute.attribute("data-testid", "settings-theme-" <> test_id_suffix),
      attribute.attribute("aria-pressed", case active {
        True -> "true"
        False -> "false"
      }),
      event.on_click(on_select_pref(this_pref)),
      ui.css([
        #("padding", "8px 6px"),
        #("border", "1px solid " <> border),
        #("background", bg),
        #("color", color),
        #("border-radius", "6px"),
        #("cursor", "pointer"),
        #("font-family", "inherit"),
        #("font-size", "14px"),
        #("font-weight", case active {
          True -> "600"
          False -> "500"
        }),
      ]),
    ],
    [html.text(label)],
  )
}

fn pref_eq(a: Pref, b: Pref) -> Bool {
  case a, b {
    System, System -> True
    LightPref, LightPref -> True
    DarkPref, DarkPref -> True
    _, _ -> False
  }
}

fn reset_section(p: Palette, on_reset: msg) -> Element(msg) {
  html.div(
    [
      ui.css([
        #("display", "flex"),
        #("flex-direction", "column"),
        #("gap", "6px"),
      ]),
    ],
    [
      section_label(p, "Local state"),
      html.button(
        [
          attribute.attribute("data-testid", "settings-reset"),
          attribute.title("Wipe all local data and reload"),
          event.on_click(on_reset),
          ui.css([
            #("padding", "8px 12px"),
            #("border", "1px solid " <> p.warn),
            #("background", p.warn_soft),
            #("color", p.warn),
            #("border-radius", "6px"),
            #("cursor", "pointer"),
            #("font-family", "inherit"),
            #("font-size", "14px"),
            #("font-weight", "600"),
            #("text-align", "center"),
          ]),
        ],
        [html.text("Reset all local state")],
      ),
      html.span(
        [
          ui.css([
            #("font-size", "12px"),
            #("color", p.text_faint),
            #("line-height", "1.4"),
          ]),
        ],
        [
          html.text(
            "Clears your identity, joined rooms, and theme. Reloads the page.",
          ),
        ],
      ),
    ],
  )
}
