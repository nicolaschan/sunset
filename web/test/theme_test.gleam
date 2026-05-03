import gleeunit/should
import sunset_web/theme

pub fn light_accent_is_terracotta_test() {
  theme.palette_for(theme.Light).accent
  |> should.equal("#bb5a3a")
}

pub fn dark_accent_is_warm_peach_test() {
  theme.palette_for(theme.Dark).accent
  |> should.equal("#e8997a")
}

pub fn light_live_is_dusty_amber_test() {
  theme.palette_for(theme.Light).live
  |> should.equal("#c98a3a")
}

pub fn dark_live_is_warm_gold_test() {
  theme.palette_for(theme.Dark).live
  |> should.equal("#e6b85a")
}

pub fn fonts_use_geist_family_test() {
  theme.font_sans
  |> should.equal("Geist, system-ui, sans-serif")

  theme.font_mono
  |> should.equal("Geist Mono, ui-monospace, monospace")
}
