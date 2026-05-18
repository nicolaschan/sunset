import gleam/float
import gleeunit/should
import sunset_web/voice_volume

// Round-trip helper: nearest-int percent the inverse maps back to.
// The percent->gain->percent chain is the contract the popover
// relies on so it doesn't drift away from a user-selected position
// on a model rehydrate.
fn round_trip(percent: Int) -> Int {
  percent
  |> voice_volume.percent_to_gain
  |> voice_volume.gain_to_percent
}

fn assert_close(actual: Float, expected: Float, tol: Float) -> Nil {
  case float.absolute_value(actual -. expected) <=. tol {
    True -> Nil
    False -> {
      // Force a gleeunit failure so the diff shows up in the report.
      should.equal(actual, expected)
      Nil
    }
  }
}

// --- percent → gain ---

pub fn percent_zero_is_gain_zero_test() {
  voice_volume.percent_to_gain(0)
  |> should.equal(0.0)
}

pub fn percent_fifty_is_half_gain_test() {
  voice_volume.percent_to_gain(50)
  |> should.equal(0.5)
}

pub fn percent_one_hundred_is_unity_gain_test() {
  voice_volume.percent_to_gain(100)
  |> should.equal(1.0)
}

pub fn percent_two_hundred_is_two_x_gain_test() {
  // Anchors the exponential segment: 200% maps to 2× gain, preserving
  // the boundary behaviour of the original linear-only slider so users
  // who previously dialed to 200% don't suddenly hear a different
  // level after the curve change.
  assert_close(voice_volume.percent_to_gain(200), 2.0, 0.0001)
}

pub fn percent_three_hundred_is_four_x_gain_test() {
  assert_close(voice_volume.percent_to_gain(300), 4.0, 0.0001)
}

pub fn percent_four_hundred_is_eight_x_gain_test() {
  assert_close(voice_volume.percent_to_gain(400), 8.0, 0.0001)
}

pub fn percent_five_hundred_is_sixteen_x_gain_test() {
  assert_close(voice_volume.percent_to_gain(500), 16.0, 0.0001)
}

pub fn percent_negative_clamps_to_zero_test() {
  voice_volume.percent_to_gain(-50)
  |> should.equal(0.0)
}

pub fn percent_above_max_clamps_to_max_gain_test() {
  assert_close(voice_volume.percent_to_gain(9999), voice_volume.max_gain, 0.0001)
}

// --- gain → percent ---

pub fn gain_zero_inverts_to_zero_test() {
  voice_volume.gain_to_percent(0.0)
  |> should.equal(0)
}

pub fn gain_half_inverts_to_fifty_test() {
  voice_volume.gain_to_percent(0.5)
  |> should.equal(50)
}

pub fn gain_one_inverts_to_one_hundred_test() {
  voice_volume.gain_to_percent(1.0)
  |> should.equal(100)
}

pub fn gain_two_inverts_to_two_hundred_test() {
  voice_volume.gain_to_percent(2.0)
  |> should.equal(200)
}

pub fn gain_sixteen_inverts_to_five_hundred_test() {
  voice_volume.gain_to_percent(16.0)
  |> should.equal(500)
}

pub fn gain_negative_clamps_to_zero_test() {
  voice_volume.gain_to_percent(-2.0)
  |> should.equal(0)
}

pub fn gain_above_max_clamps_to_max_percent_test() {
  voice_volume.gain_to_percent(9999.0)
  |> should.equal(voice_volume.max_percent_other)
}

// --- round-trips ---

pub fn round_trip_zero_test() {
  round_trip(0) |> should.equal(0)
}

pub fn round_trip_fifty_test() {
  round_trip(50) |> should.equal(50)
}

pub fn round_trip_one_hundred_test() {
  round_trip(100) |> should.equal(100)
}

pub fn round_trip_two_hundred_test() {
  round_trip(200) |> should.equal(200)
}

pub fn round_trip_three_hundred_test() {
  round_trip(300) |> should.equal(300)
}

pub fn round_trip_five_hundred_test() {
  round_trip(500) |> should.equal(500)
}

// --- boundary continuity ---

pub fn boundary_is_continuous_at_one_hundred_test() {
  // The two segments meet at percent 100, gain 1.0. Subtle but
  // important: a one-step jump across the boundary must not produce
  // a perceptible discontinuity in gain.
  let just_below = voice_volume.percent_to_gain(95)
  let at_boundary = voice_volume.percent_to_gain(100)
  let just_above = voice_volume.percent_to_gain(105)
  // Below should be < unity, boundary == unity, above should be > unity
  // and tighter on the boundary side than the linear segment would
  // suggest — exponential rises slower than the linear slope at 100.
  let _ =
    should.be_true({
      just_below <. at_boundary && at_boundary <. just_above
    })
  // Continuity check: a 5-percent step on either side of the boundary
  // shouldn't differ by more than the linear-segment step (0.05).
  assert_close(at_boundary -. just_below, 0.05, 0.0001)
  let above_step = just_above -. at_boundary
  // Exponential step is smaller than the linear-segment step at this
  // distance from the boundary — that's the whole point of "finer
  // control above 100%".
  let _ =
    should.be_true(above_step <. 0.05)
  Nil
}
