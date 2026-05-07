//// Shared helpers for voice waveform/meter rendering.
////
//// Both the channels-rail row's 12-bar VU meter and the voice-popover's
//// 36-bar waveform strip drive off the same audio-level signal (the
//// FFI's per-peer / self EMA). The threshold and CSS-attribute encoders
//// live here so the two surfaces stay in lockstep — tuning the
//// "speaking" threshold or the data-voice-level format only ever needs
//// to happen in one place.

import gleam/float
import gleam/int

/// RMS level above which the UI flips a row/header to "speaking" (bold
/// name, live dot). Mirrors the threshold the voice e2e suite uses to
/// distinguish real Opus-decoded audio from silence underrun padding.
pub fn speaking_threshold() -> Float {
  0.05
}

/// Render a positive float as integer-pixel CSS (`"NNpx"`). CSS
/// tolerates fractional pixels but rounding produces tighter, less
/// visually noisy bar heights.
pub fn float_to_px(f: Float) -> String {
  int.to_string(float.round(f)) <> "px"
}

/// Render a 0..1 float with two decimal places ("0.42"). Used for
/// `data-voice-level` attributes that e2e selectors compare against;
/// two decimals is enough to distinguish silence (~0.00) from speech
/// (≥ 0.05) without noisy attribute churn on every smoothed update.
pub fn level_to_attribute(f: Float) -> String {
  let scaled = float.round(f *. 100.0)
  let int_part = scaled / 100
  let frac = scaled % 100
  let frac_part = case frac < 10 {
    True -> "0" <> int.to_string(frac)
    False -> int.to_string(frac)
  }
  int.to_string(int_part) <> "." <> frac_part
}
