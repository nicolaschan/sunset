//// Sunset palette + font tokens.
////
//// Neutral-first palette: surfaces are warm-tinted gray/white (light)
//// and near-black (dark), text is plain neutral. The sunset accent is
//// reserved for branding, primary actions, and unread badges — it is
//// *not* used as a status color. Status uses universal semantics:
//// `ok` = green (connected/healthy), `warn` = amber (needs attention),
//// `danger` = red (error), `live` = green (actively speaking), and
//// neutral grays for offline/idle. Surfaces still carry a subtle warm
//// undertone so the brand stays present without dominating the chrome.

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
    /// Sunset accent. Reserved for branding (logo, app name), primary
    /// CTA buttons, and unread badges. Do NOT use to convey status.
    accent: String,
    accent_soft: String,
    accent_deep: String,
    accent_ink: String,
    /// Green — connected / healthy / success. Universal semantics.
    ok: String,
    ok_soft: String,
    /// Amber — needs attention. Reserved for genuine warnings, not
    /// for "everything-is-fine but slightly different" states.
    warn: String,
    warn_soft: String,
    /// Red — error / failure. Used for hangup buttons and error toasts.
    danger: String,
    danger_soft: String,
    /// Active "speaking" / live state. Aliased to `ok` so a peer who
    /// is actively talking reads as the same green as a connected peer
    /// (rather than introducing a third status hue).
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
    // Cool-leaning warm gray — a hint of the sunset undertone is still
    // there (~3deg toward orange) but the eye reads it as neutral
    // off-white rather than as cream.
    bg: "#f6f6f5",
    surface: "#ffffff",
    surface_alt: "#f0efed",
    surface_sunk: "#e6e5e2",
    // A whisper above surface — the row hover stands out without
    // muddying the YOU tag / reaction pills that sit on surface_alt.
    row_highlight: "#f7f6f4",
    border: "#dddbd6",
    border_soft: "#e8e6e1",
    text: "#1c1c1b",
    text_muted: "#5b5a57",
    text_faint: "#9a9893",
    // Deep clay — saturated enough to be the primary CTA, restrained
    // enough to feel professional. Used sparingly: brand mark, primary
    // buttons, unread badges.
    accent: "#b04a2a",
    accent_soft: "#f5e4dc",
    accent_deep: "#7e2f15",
    accent_ink: "#ffffff",
    ok: "#2f7d3f",
    ok_soft: "#dfeede",
    warn: "#a06410",
    warn_soft: "#f5e4c5",
    danger: "#a82828",
    danger_soft: "#f5dada",
    live: "#2f7d3f",
    shadow: "0 1px 2px rgba(20,20,20,0.06)",
    shadow_lg: "0 16px 50px rgba(20,20,20,0.10)",
  )
}

fn dark() -> Palette {
  Palette(
    bg: "#101012",
    surface: "#17171a",
    surface_alt: "#131316",
    surface_sunk: "#0c0c0e",
    // Slightly *lighter* than surface so a hovered row reads as
    // raised in dark mode (the rest of the chrome sinks darker).
    row_highlight: "#1f1f23",
    border: "#26262b",
    border_soft: "#1d1d20",
    text: "#e8e8ea",
    text_muted: "#9a9a9f",
    text_faint: "#5c5c61",
    accent: "#e0805e",
    accent_soft: "#3a201a",
    accent_deep: "#f29c7e",
    accent_ink: "#17171a",
    ok: "#56b66a",
    ok_soft: "#1a2b1d",
    warn: "#d28a3a",
    warn_soft: "#2e2114",
    danger: "#e06464",
    danger_soft: "#2e1818",
    live: "#56b66a",
    shadow: "0 1px 2px rgba(0,0,0,0.5)",
    shadow_lg: "0 20px 60px rgba(0,0,0,0.6)",
  )
}
