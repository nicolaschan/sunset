//// Volume curve for the per-peer playback slider.
////
//// Slider position is expressed as a percent (the user-facing
//// "200%"-style label). The audio engine wants a linear gain
//// multiplier on the `GainNode`. The mapping is *not* one-to-one
//// across the full slider range:
////
//// * Below 100%: linear. `gain = percent / 100`, so 50% slider ⇒
////   0.5 gain, matching the intuitive "half-volume" expectation.
//// * Above 100%: exponential with base 2 anchored at the boundary.
////   `gain = 2 ^ ((percent - 100) / 100)`. So 200% ⇒ 2×, 300% ⇒ 4×,
////   400% ⇒ 8×, 500% ⇒ 16×.
////
//// Two reasons for the split:
////   1. Below unity, linear matches how loud-vs-quiet *feels* in the
////      0..1 range — halving gain to 0.5 reads as "noticeably quieter
////      but still clearly there".
////   2. Above unity, additional linear gain runs out of headroom
////      fast. A single doubling at the 200% mark is not enough for
////      peers whose mic captures very quietly. Each +100% slider tick
////      now doubles gain, which lines up with how loudness is
////      perceived (roughly logarithmic) and gives many more dB of
////      boost on tap without a clipping cliff at the slider's max.
////
//// The curve is continuous at 100% (`gain = 1.0` on both sides).
//// The *slope* is not continuous — the linear segment has slope
//// `1/100` per percent and the exponential segment has slope
//// `(ln 2)/100` per percent — but that subtle "fine-grained boost"
//// feel above 100% is intentional.

import gleam/float
import gleam/int

/// Minimum slider value. Slider goes 0 → max.
pub const min_percent: Int = 0

/// Cap for non-self peers. Higher than 100 unlocks the exponential
/// boost segment. Picked so the top of the slider lands at 16× gain
/// (2 ^ ((500 - 100) / 100) = 2^4) — enough headroom for very quiet
/// peers without dropping straight to "loud enough to clip on bursts".
pub const max_percent_other: Int = 500

/// Cap for the local user's own playback row. The local-side volume
/// slider only attenuates what *you* hear of yourself (e.g. for
/// monitoring); there's no use case for boosting your own playback
/// above unity, so we keep the slider in the linear 0..100 segment.
pub const max_percent_self: Int = 100

/// Maximum gain the FFI is willing to apply. `setPeerVolume` clamps
/// to this on the JS side so the AudioContext's `GainNode` never
/// receives a value past the slider's top, even if a caller hands in
/// a stale or out-of-range value. Must agree with the JS clamp in
/// `voice.ffi.mjs` (`setPeerVolume`).
pub const max_gain: Float = 16.0

/// Convert a slider percent (0..max_percent_other) to a linear gain
/// multiplier suitable for `GainNode.gain.value`.
///
/// Negative inputs clamp to 0; values above `max_percent_other`
/// clamp to `max_gain`. Always returns a finite, non-negative value.
pub fn percent_to_gain(percent: Int) -> Float {
  let clamped = clamp_percent(percent)
  case clamped <= 100 {
    True -> int.to_float(clamped) /. 100.0
    False -> {
      let exponent = int.to_float(clamped - 100) /. 100.0
      case float.power(2.0, exponent) {
        Ok(g) -> g
        // Unreachable: base 2 with a non-negative finite exponent
        // never errors. The branch exists only so the call site
        // doesn't have to plumb a Result through the FFI bridge.
        Error(_) -> 1.0
      }
    }
  }
}

/// Inverse of `percent_to_gain`. Used when the model only retains a
/// raw gain value (e.g. an FFI caller dispatched `SetPeerVolume`
/// with a multiplier) and the popover needs to render the matching
/// slider position.
///
/// Inputs are clamped to `[0.0, max_gain]`. Output rounds to the
/// nearest integer percent on each segment.
pub fn gain_to_percent(gain: Float) -> Int {
  let clamped = clamp_gain(gain)
  case clamped <=. 1.0 {
    True -> float.round(clamped *. 100.0)
    False ->
      case float.logarithm(clamped) {
        // log base 2: log_2(x) = ln(x) / ln(2). ln(2) ≈ 0.6931472.
        Ok(ln_x) -> float.round(100.0 +. ln_x /. 0.6931471805599453 *. 100.0)
        // logarithm is only Error for x <= 0, and we just clamped
        // above 1.0; never reached.
        Error(_) -> 100
      }
  }
}

fn clamp_percent(percent: Int) -> Int {
  case percent < min_percent {
    True -> min_percent
    False ->
      case percent > max_percent_other {
        True -> max_percent_other
        False -> percent
      }
  }
}

fn clamp_gain(gain: Float) -> Float {
  case gain <. 0.0 {
    True -> 0.0
    False ->
      case gain >. max_gain {
        True -> max_gain
        False -> gain
      }
  }
}
