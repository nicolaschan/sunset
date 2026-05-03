import gleam/list
import gleeunit/should
import sunset_web/fixture

pub fn rooms_has_six_entries_test() {
  fixture.rooms()
  |> list.length
  |> should.equal(6)
}

pub fn channels_include_text_and_voice_test() {
  fixture.channels()
  |> list.length
  |> should.equal(6)
}

pub fn members_has_eight_entries_with_one_self_test() {
  let ms = fixture.members()
  ms
  |> list.length
  |> should.equal(8)

  ms
  |> list.filter(fn(m) { m.you })
  |> list.length
  |> should.equal(1)
}

pub fn messages_has_seven_entries_with_one_pending_test() {
  let ms = fixture.messages()
  ms
  |> list.length
  |> should.equal(7)

  ms
  |> list.filter(fn(m) { m.pending })
  |> list.length
  |> should.equal(1)
}
