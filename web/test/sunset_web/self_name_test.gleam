import gleam/dict
import gleeunit/should
import sunset_web

pub fn display_name_returns_dict_value_when_present_test() {
  let pk = <<0x01, 0x02, 0x03>>
  let key = sunset_web.hex_encode(pk)
  let map = dict.new() |> dict.insert(key, "alice")
  sunset_web.display_name(map, pk) |> should.equal("alice")
}

pub fn display_name_falls_back_to_short_pubkey_when_absent_test() {
  let pk = <<0x01, 0x02, 0x03>>
  let map = dict.new()
  let actual = sunset_web.display_name(map, pk)
  actual |> should.not_equal("alice")
}
