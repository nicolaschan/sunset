import gleam/option
import gleeunit/should
import sunset_web
import sunset_web/domain.{
  type Member, Away, Direct, Member, MemberId, MutedP, NoRelay, NoRole, OfflineP,
  Online, Speaking,
}

fn member(id: String, status: domain.Presence) -> Member {
  Member(
    id: MemberId(id),
    name: id,
    initials: "",
    status: status,
    relay: NoRelay,
    you: False,
    in_call: False,
    role: NoRole,
    last_heartbeat_ms: option.None,
    raw_name: option.None,
    pubkey: <<>>,
  )
}

pub fn empty_list_is_zero_test() {
  sunset_web.count_online_members([])
  |> should.equal(0)
}

pub fn only_offline_is_zero_test() {
  sunset_web.count_online_members([
    member("a", OfflineP),
    member("b", OfflineP),
  ])
  |> should.equal(0)
}

pub fn online_speaking_muted_away_all_count_test() {
  // Anyone whose presence is not OfflineP counts as online — including
  // Speaking (in voice and talking), MutedP (in voice and muted), and
  // Away (idle but reachable). This matches the existing definition in
  // the members rail.
  sunset_web.count_online_members([
    member("a", Online),
    member("b", Speaking),
    member("c", MutedP),
    member("d", Away),
  ])
  |> should.equal(4)
}

pub fn mixed_online_and_offline_test() {
  sunset_web.count_online_members([
    member("a", Online),
    member("b", OfflineP),
    member("c", Away),
    member("d", OfflineP),
    member("e", Speaking),
  ])
  |> should.equal(3)
}

pub fn ignores_relay_field_test() {
  // The relay field describes how we *reach* a peer; it doesn't affect
  // whether the peer is online. A peer reached via Direct vs. NoRelay
  // is still online if presence says so.
  sunset_web.count_online_members([
    Member(..member("a", Online), relay: Direct),
    Member(..member("b", Online), relay: NoRelay),
  ])
  |> should.equal(2)
}
