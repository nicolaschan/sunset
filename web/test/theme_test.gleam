import gleeunit/should
import sunset_web/theme

pub fn light_accent_is_deep_teal_test() {
  theme.palette_for(theme.Light).accent
  |> should.equal("#226d6f")
}

pub fn dark_accent_is_brighter_teal_test() {
  theme.palette_for(theme.Dark).accent
  |> should.equal("#5dbab0")
}

pub fn fonts_use_geist_family_test() {
  theme.font_sans
  |> should.equal("Geist, system-ui, sans-serif")

  theme.font_mono
  |> should.equal("Geist Mono, ui-monospace, monospace")
}
