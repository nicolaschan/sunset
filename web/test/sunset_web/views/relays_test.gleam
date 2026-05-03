import gleam/dict
import gleam/list
import gleam/option
import gleeunit/should
import sunset_web/domain.{
  RelayBackoff, RelayCancelled, RelayConnected, RelayConnecting,
}
import sunset_web/sunset.{type IntentSnapshot, IntentSnapshot}
import sunset_web/views/relays

pub fn is_relay_label_bare_hostname_test() {
  relays.is_relay_label("relay.sunset.chat") |> should.be_true()
}

pub fn is_relay_label_wss_test() {
  relays.is_relay_label("wss://relay.sunset.chat#x25519=ab") |> should.be_true()
}

pub fn is_relay_label_ws_test() {
  relays.is_relay_label("ws://127.0.0.1:8080") |> should.be_true()
}

pub fn is_relay_label_webrtc_test() {
  relays.is_relay_label("webrtc://abcdef#x25519=11") |> should.be_false()
}

pub fn parse_host_bare_hostname_test() {
  relays.parse_host("relay.sunset.chat") |> should.equal("relay.sunset.chat")
}

pub fn parse_host_wss_test() {
  relays.parse_host("wss://relay.sunset.chat") |> should.equal("relay.sunset.chat")
}

pub fn parse_host_with_port_test() {
  relays.parse_host("ws://127.0.0.1:8080") |> should.equal("127.0.0.1:8080")
}

pub fn parse_host_full_url_test() {
  relays.parse_host("wss://relay.sunset.chat:443/api?token=foo#x25519=abc")
  |> should.equal("relay.sunset.chat:443")
}

pub fn parse_state_known_test() {
  relays.parse_state("connected") |> should.equal(RelayConnected)
  relays.parse_state("connecting") |> should.equal(RelayConnecting)
  relays.parse_state("backoff") |> should.equal(RelayBackoff)
  relays.parse_state("cancelled") |> should.equal(RelayCancelled)
}

pub fn parse_state_unknown_falls_back_to_backoff_test() {
  relays.parse_state("eldritch_state") |> should.equal(RelayBackoff)
}

pub fn format_status_connected_test() {
  relays.format_status(RelayConnected, 0) |> should.equal("Connected")
}

pub fn format_status_connecting_test() {
  relays.format_status(RelayConnecting, 0) |> should.equal("Connecting")
}

pub fn format_status_backoff_zero_test() {
  relays.format_status(RelayBackoff, 0) |> should.equal("Backoff")
}

pub fn format_status_backoff_with_attempt_test() {
  relays.format_status(RelayBackoff, 3) |> should.equal("Backoff (attempt 3)")
}

pub fn format_status_cancelled_test() {
  relays.format_status(RelayCancelled, 7) |> should.equal("Cancelled")
}

pub fn format_rtt_present_test() {
  relays.format_rtt(option.Some(42)) |> should.equal("RTT 42 ms")
}

pub fn format_rtt_absent_test() {
  relays.format_rtt(option.None) |> should.equal("RTT —")
}

pub fn humanize_age_just_now_test() {
  relays.humanize_age(1000, option.Some(800)) |> should.equal("just now")
}

pub fn humanize_age_seconds_test() {
  relays.humanize_age(5500, option.Some(500)) |> should.equal("5s ago")
}

pub fn humanize_age_never_test() {
  relays.humanize_age(0, option.None) |> should.equal("never")
}

pub fn short_peer_id_short_unchanged_test() {
  relays.short_peer_id("abcdef") |> should.equal("abcdef")
}

pub fn short_peer_id_truncates_test() {
  relays.short_peer_id("0123456789abcdef0123456789abcdef")
  |> should.equal("01234567…89abcdef")
}

fn snap(id: Float, label: String, state: String) -> IntentSnapshot {
  IntentSnapshot(
    id: id,
    state: state,
    label: label,
    peer_pubkey: option.None,
    kind: option.None,
    attempt: 0,
    last_pong_at_ms: option.None,
    last_rtt_ms: option.None,
  )
}

pub fn from_intent_basic_test() {
  let s = snap(7.0, "relay.sunset.chat", "connected")
  let r = relays.from_intent(s)
  r.id |> should.equal(7.0)
  r.host |> should.equal("relay.sunset.chat")
  r.raw_label |> should.equal("relay.sunset.chat")
  r.state |> should.equal(RelayConnected)
  r.attempt |> should.equal(0)
  r.peer_id_short |> should.equal(option.None)
  r.last_pong_at_ms |> should.equal(option.None)
  r.last_rtt_ms |> should.equal(option.None)
}

pub fn relays_for_view_filters_webrtc_test() {
  let intents =
    dict.new()
    |> dict.insert(1.0, snap(1.0, "relay.sunset.chat", "connected"))
    |> dict.insert(2.0, snap(2.0, "webrtc://abc#x25519=11", "connected"))
    |> dict.insert(3.0, snap(3.0, "wss://other.example", "connecting"))
  let out = relays.relays_for_view(intents)
  out |> list.length() |> should.equal(2)
  case out {
    [a, b] -> {
      a.id |> should.equal(1.0)
      b.id |> should.equal(3.0)
    }
    _ -> should.fail()
  }
}
