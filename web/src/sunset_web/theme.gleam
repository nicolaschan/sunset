//// Quiet (D1) palette + font tokens.
////
//// Light: warm cream surfaces, deep teal accent.
//// Dark: warm-tinted ink, brighter teal accent.
////
//// Source values are taken verbatim from `quiet.jsx` (`QUIET_PALETTES.geist`).

pub type Mode {
  Light
  Dark
}

pub type Palette {
  Palette(
    bg: String,
    surface: String,
    surface_alt: String,
    surface_sunk: String,
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

fn light() -> Palette {
  Palette(
    bg: "#f7f5f1",
    surface: "#ffffff",
    surface_alt: "#f4f1ec",
    surface_sunk: "#eeeae3",
    border: "#e3ddd2",
    border_soft: "#ebe6dc",
    text: "#1c1d20",
    text_muted: "#5e5b54",
    text_faint: "#9c9890",
    accent: "#226d6f",
    accent_soft: "#dde9e8",
    accent_deep: "#15514f",
    accent_ink: "#ffffff",
    ok: "#3a8a4a",
    ok_soft: "#dceadb",
    warn: "#b07a1f",
    warn_soft: "#f4e7cd",
    live: "#3a8a4a",
    shadow: "0 1px 2px rgba(40,30,15,0.04)",
    shadow_lg: "0 16px 50px rgba(40,30,15,0.06)",
  )
}

fn dark() -> Palette {
  Palette(
    bg: "#0e1314",
    surface: "#161c1d",
    surface_alt: "#11171a",
    surface_sunk: "#0a1011",
    border: "#222a2c",
    border_soft: "#1c2426",
    text: "#e5e3dd",
    text_muted: "#9aa19c",
    text_faint: "#5c6663",
    accent: "#5dbab0",
    accent_soft: "#1a2e2d",
    accent_deep: "#7ed4c8",
    accent_ink: "#0e1314",
    ok: "#5fbe6e",
    ok_soft: "#1a2c1d",
    warn: "#d4a455",
    warn_soft: "#2e2516",
    live: "#5fbe6e",
    shadow: "0 1px 2px rgba(0,0,0,0.5)",
    shadow_lg: "0 20px 60px rgba(0,0,0,0.6)",
  )
}
