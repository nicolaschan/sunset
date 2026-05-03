import gleam/dict
import gleam/option.{None}
import gleeunit/should
import sunset_web
import sunset_web/domain
import sunset_web/sunset.{type IntentSnapshot, IntentSnapshot}

fn snap(id: Float, state: String) -> IntentSnapshot {
  IntentSnapshot(
    id: id,
    state: state,
    label: "test",
    peer_pubkey: None,
    kind: None,
    attempt: 0,
  )
}

pub fn empty_dict_is_offline_test() {
  sunset_web.relay_status_pill(dict.new())
  |> should.equal(domain.Offline)
}

pub fn any_connected_wins_test() {
  let intents =
    dict.new()
    |> dict.insert(1.0, snap(1.0, "backoff"))
    |> dict.insert(2.0, snap(2.0, "connected"))
  sunset_web.relay_status_pill(intents)
  |> should.equal(domain.Connected)
}

pub fn connecting_is_reconnecting_test() {
  let intents =
    dict.new()
    |> dict.insert(1.0, snap(1.0, "connecting"))
  sunset_web.relay_status_pill(intents)
  |> should.equal(domain.Reconnecting)
}

pub fn backoff_is_reconnecting_test() {
  let intents =
    dict.new()
    |> dict.insert(1.0, snap(1.0, "backoff"))
  sunset_web.relay_status_pill(intents)
  |> should.equal(domain.Reconnecting)
}

pub fn cancelled_only_is_offline_test() {
  let intents =
    dict.new()
    |> dict.insert(1.0, snap(1.0, "cancelled"))
  sunset_web.relay_status_pill(intents)
  |> should.equal(domain.Offline)
}
