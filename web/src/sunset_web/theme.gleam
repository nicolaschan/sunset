//// Sunset palette + font tokens.
////
//// Light: warm cream surfaces with a faint pink undertone, terracotta
//// accent for primary action, dusty amber for "live" / unread, ember
//// orange for warnings. Greens stay green for `ok` so success still
//// reads as success universally.
////
//// Dark: warm dusk ink (slight plum undertone), peach accent that
//// brightens on hover, warm gold "live" dot. Same role layout as
//// light, just shifted up the lightness scale.

pub type Mode {
  Light
  Dark
}

/// User-facing theme preference. `System` defers to the OS / browser
/// `prefers-color-scheme` query at render time; `Light` / `Dark`
/// override it explicitly. Persisted to localStorage as "" / "light" /
/// "dark"; the `read_saved_pref` / `write_saved_pref` helpers in
/// `storage.gleam` handle the round-trip.
pub type Pref {
  System
  LightPref
  DarkPref
}

/// Resolve a preference + the current OS dark-scheme signal into the
/// concrete `Mode` we render with. `System` follows the OS; `LightPref`
/// / `DarkPref` always force their respective mode.
pub fn resolve_mode(pref: Pref, os_prefers_dark: Bool) -> Mode {
  case pref {
    System ->
      case os_prefers_dark {
        True -> Dark
        False -> Light
      }
    LightPref -> Light
    DarkPref -> Dark
  }
}

pub type Palette {
  Palette(
    bg: String,
    surface: String,
    surface_alt: String,
    surface_sunk: String,
    /// Background tint used for the message-row hover / active state.
    /// A separate token from `surface_alt` so the YOU tag and reaction
    /// pills (which themselves use `surface_alt` as a background) stay
    /// visible against the highlight — using `surface_alt` here makes
    /// those nested chips blend into the highlighted row.
    row_highlight: String,
    border: String,
    border_soft: String,
    text: String,
    text_muted: String,
    text_faint: String,
    accent: String,
    accent_soft: String,
    accent_deep: String,
    accent_ink: String,
    ok: String,
    ok_soft: String,
    warn: String,
    warn_soft: String,
    live: String,
    shadow: String,
    shadow_lg: String,
  )
}

pub const font_sans = "Geist, system-ui, sans-serif"

pub const font_mono = "Geist Mono, ui-monospace, monospace"

pub fn palette_for(mode: Mode) -> Palette {
  case mode {
    Light -> light()
    Dark -> dark()
  }
}

/// CSS `color-scheme` keyword for the active mode. Setting this on the
/// root tells the UA to render its default chrome (scrollbars, form
/// controls, the iOS "below the page" gap) in tones that match the
/// app, instead of flashing the opposite-theme default.
pub fn color_scheme(mode: Mode) -> String {
  case mode {
    Light -> "light"
    Dark -> "dark"
  }
}

fn light() -> Palette {
  Palette(
    bg: "#f8f2ec",
    surface: "#ffffff",
    surface_alt: "#f4eadf",
    surface_sunk: "#ecdfcd",
    // Sits between `surface` and `surface_alt` — visible as a row-
    // hover tint without obscuring the YOU tag / reaction pills that
    // themselves render on `surface_alt`.
    row_highlight: "#fbf6ed",
    border: "#e5d4be",
    border_soft: "#ede0c9",
    text: "#1f1c1a",
    text_muted: "#5e574e",
    text_faint: "#9d958a",
    accent: "#bb5a3a",
    accent_soft: "#f6dccc",
    accent_deep: "#8a3a22",
    accent_ink: "#ffffff",
    ok: "#3a8a4a",
    ok_soft: "#dceadb",
    warn: "#a8641a",
    warn_soft: "#f5dfbc",
    live: "#c98a3a",
    shadow: "0 1px 2px rgba(60,30,15,0.05)",
    shadow_lg: "0 16px 50px rgba(60,30,15,0.07)",
  )
}

fn dark() -> Palette {
  Palette(
    bg: "#13110f",
    surface: "#1c1814",
    surface_alt: "#181410",
    surface_sunk: "#100c08",
    // Slightly *lighter* than `surface` so a hovered row reads as
    // raised in dark mode (the rest of the chrome — including
    // surface_alt — sinks darker).
    row_highlight: "#241f1a",
    border: "#2a221c",
    border_soft: "#221c17",
    text: "#ece4d6",
    text_muted: "#a09489",
    text_faint: "#5e564d",
    accent: "#e8997a",
    accent_soft: "#3a2520",
    accent_deep: "#f2b095",
    accent_ink: "#1c1814",
    ok: "#5fbe6e",
    ok_soft: "#1a2c1d",
    warn: "#d4884a",
    warn_soft: "#2e2014",
    live: "#e6b85a",
    shadow: "0 1px 2px rgba(0,0,0,0.5)",
    shadow_lg: "0 20px 60px rgba(0,0,0,0.6)",
  )
}
