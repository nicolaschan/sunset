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

import gleam/list
import gleam/result
import gleam/string

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
    /// Per-author display-name hues. Picked deterministically from
    /// each author's identity (see `author_color` in `main_panel`)
    /// so a chat with N participants reads as N distinct colors —
    /// without falling back to plain `text` (which makes the bold
    /// author name visually disappear into the body copy).
    ///
    /// Carefully disjoint from accent / ok / warn / danger so a
    /// per-author hue can never be confused for a status color. All
    /// hues stay sunset-tinted (no icy blues / pure violets) so the
    /// palette feels coherent.
    author_hues: List(String),
    shadow: String,
    shadow_lg: String,
  )
}

pub const font_sans = "Inter, system-ui, sans-serif"

pub const font_mono = "JetBrains Mono, ui-monospace, monospace"

/// Pick a stable hue from `palette.author_hues` for the given identity
/// string. Same string → same color across renders, so each
/// participant in a chat keeps a consistent color in the chat
/// scrollback AND in the members rail. Falls back to `palette.text`
/// when the palette ships an empty hue list (defensive — current
/// themes always populate it).
pub fn hue_for_identity(palette: Palette, identity: String) -> String {
  case list.length(palette.author_hues) {
    0 -> palette.text
    n -> {
      let i = identity_hash(identity) % n
      list.drop(palette.author_hues, i)
      |> list.first
      |> result.unwrap(palette.text)
    }
  }
}

/// djb2-style stable hash of an identity string. Multiplier 33,
/// modulo a prime to keep the running accumulator bounded on the
/// JS target (where Number is double-precision float — large enough
/// for this not to matter, but the modulo costs nothing and makes
/// the hash stable across runtimes too).
fn identity_hash(s: String) -> Int {
  s
  |> string.to_utf_codepoints
  |> list.fold(5381, fn(acc, cp) {
    { acc * 33 + string.utf_codepoint_to_int(cp) } % 1_000_003
  })
}

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
    // Slightly cool warm-gray — the sunset undertone is now carried by
    // the magenta accent, so the surfaces read as crisp neutral rather
    // than as cream.
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
    // Deep magenta-rose — the "purple hour" of a sunset, where pink
    // gives way to violet. Keeps the brand warm without competing with
    // the green/amber/red status palette. Used sparingly: brand mark,
    // primary CTAs, unread badges, own-message author name.
    accent: "#a83565",
    accent_soft: "#f3dde6",
    accent_deep: "#7a2046",
    accent_ink: "#ffffff",
    ok: "#2f7d3f",
    ok_soft: "#dfeede",
    warn: "#a06410",
    warn_soft: "#f5e4c5",
    danger: "#a82828",
    danger_soft: "#f5dada",
    live: "#2f7d3f",
    author_hues: [
      "#3a5680",
      "#2a6e7e",
      "#4d4d8e",
      "#6b3870",
      "#6b4a35",
      "#5a4878",
    ],
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
    // Lifted toward pink for dark mode so the magenta-rose reads on
    // near-black without losing chroma. Same role as the light accent:
    // brand mark, primary CTAs, unread badges.
    accent: "#e283ad",
    accent_soft: "#36202a",
    accent_deep: "#f198bd",
    accent_ink: "#17171a",
    ok: "#56b66a",
    ok_soft: "#1a2b1d",
    warn: "#d28a3a",
    warn_soft: "#2e2114",
    danger: "#e06464",
    danger_soft: "#2e1818",
    live: "#56b66a",
    author_hues: [
      "#7e9bca",
      "#5fa8b5",
      "#9494d0",
      "#b075b3",
      "#c08d6e",
      "#a08cc4",
    ],
    shadow: "0 1px 2px rgba(0,0,0,0.5)",
    shadow_lg: "0 20px 60px rgba(0,0,0,0.6)",
  )
}
