import gleeunit/should
import sunset_web/theme

pub fn light_accent_is_deep_magenta_rose_test() {
  // Sunset accent — the "purple hour" of a sunset, where pink gives way
  // to violet. Saturated enough to be the primary CTA, warm enough to
  // still read as sunset rather than as cool grape.
  theme.palette_for(theme.Light).accent
  |> should.equal("#a83565")
}

pub fn dark_accent_is_lifted_pink_test() {
  // Same magenta-rose role as light, lifted toward pink so the brand
  // hue keeps its chroma against a near-black surface.
  theme.palette_for(theme.Dark).accent
  |> should.equal("#e283ad")
}

pub fn live_is_aliased_to_ok_test() {
  // Speaking / live state uses the same green as the rest of the
  // "healthy / connected" semantic — preventing a third hue from
  // splitting the user's mental model of "what does that color mean".
  let light = theme.palette_for(theme.Light)
  light.live |> should.equal(light.ok)

  let dark = theme.palette_for(theme.Dark)
  dark.live |> should.equal(dark.ok)
}

pub fn ok_is_universally_green_test() {
  // Any user, regardless of brand familiarity, expects "good / connected"
  // to be green. We pin the hex so a future palette tweak can't silently
  // shift it back into a brand-adjacent hue.
  theme.palette_for(theme.Light).ok
  |> should.equal("#2f7d3f")
  theme.palette_for(theme.Dark).ok
  |> should.equal("#56b66a")
}

pub fn warn_is_amber_test() {
  theme.palette_for(theme.Light).warn
  |> should.equal("#a06410")
  theme.palette_for(theme.Dark).warn
  |> should.equal("#d28a3a")
}

pub fn danger_is_red_test() {
  theme.palette_for(theme.Light).danger
  |> should.equal("#a82828")
  theme.palette_for(theme.Dark).danger
  |> should.equal("#e06464")
}

pub fn accent_does_not_collide_with_status_colors_test() {
  // The whole point of the redesign was that brand color (sunset
  // accent) and status colors (ok/warn/danger) live in disjoint
  // semantic spaces. If they ever collide, popovers and CTAs become
  // ambiguous with status indicators.
  let p = theme.palette_for(theme.Light)
  p.accent |> should.not_equal(p.ok)
  p.accent |> should.not_equal(p.warn)
  p.accent |> should.not_equal(p.danger)
}

pub fn fonts_use_inter_and_jetbrains_mono_test() {
  // Inter for the body sans + JetBrains Mono for code/IDs. Noto Color
  // Emoji terminates both stacks so emoji codepoints render the same
  // on every host rather than falling back to OS-installed emoji
  // fonts (Apple Color Emoji / Segoe UI Emoji / mono-glyph stubs).
  // All three faces ship via the Google Fonts stylesheet pinned in
  // `web/gleam.toml`; keep these constants in sync with that link or
  // the second-tier system fallback will paint instead.
  theme.font_sans
  |> should.equal("Inter, system-ui, sans-serif, \"Noto Color Emoji\"")

  theme.font_mono
  |> should.equal(
    "JetBrains Mono, ui-monospace, monospace, \"Noto Color Emoji\"",
  )
}
