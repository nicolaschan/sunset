import gleam/option
import gleeunit/should
import sunset_web/domain
import sunset_web/views/peer_status_popover

pub fn humanize_age_never_test() {
  peer_status_popover.humanize_age(1000, option.None)
  |> should.equal("never")
}

pub fn humanize_age_just_now_test() {
  peer_status_popover.humanize_age(100, option.Some(50))
  |> should.equal("just now")
}

pub fn humanize_age_seconds_test() {
  peer_status_popover.humanize_age(5500, option.Some(500))
  |> should.equal("5s ago")
}

pub fn humanize_age_minutes_test() {
  peer_status_popover.humanize_age(125_000, option.Some(0))
  |> should.equal("2m ago")
}

pub fn humanize_age_hours_test() {
  peer_status_popover.humanize_age(7200_000, option.Some(0))
  |> should.equal("2h ago")
}

pub fn humanize_age_clock_skew_test() {
  // Future timestamp (e.g., clock skew) clamps to 0 → "just now".
  peer_status_popover.humanize_age(100, option.Some(500))
  |> should.equal("just now")
}

pub fn transport_label_direct_test() {
  peer_status_popover.transport_label(domain.Direct)
  |> should.equal("Direct (WebRTC)")
}

pub fn transport_label_via_relay_test() {
  peer_status_popover.transport_label(domain.OneHop)
  |> should.equal("Via relay")
}

pub fn transport_label_self_test() {
  peer_status_popover.transport_label(domain.SelfRelay)
  |> should.equal("Self")
}

pub fn transport_label_unknown_test() {
  peer_status_popover.transport_label(domain.NoRelay)
  |> should.equal("Unknown")
}
