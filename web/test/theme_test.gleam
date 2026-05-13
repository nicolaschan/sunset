import gleeunit/should
import sunset_web/theme

pub fn light_accent_is_deep_clay_test() {
  // Sunset accent — deep clay / terracotta, kept saturated enough to be
  // the primary CTA, restrained enough to feel professional.
  theme.palette_for(theme.Light).accent
  |> should.equal("#b04a2a")
}

pub fn dark_accent_is_warm_peach_test() {
  theme.palette_for(theme.Dark).accent
  |> should.equal("#e0805e")
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

pub fn fonts_use_geist_family_test() {
  theme.font_sans
  |> should.equal("Geist, system-ui, sans-serif")

  theme.font_mono
  |> should.equal("Geist Mono, ui-monospace, monospace")
}
