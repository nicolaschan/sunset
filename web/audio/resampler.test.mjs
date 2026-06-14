// Unit tests for the streaming Resampler. Zero dependencies — run with
// `node web/audio/resampler.test.mjs`. In CI the voice test runner
// (`nix run .#web-test-voice`) runs this before the browser suite. Exits
// non-zero on the first failed assertion.
import { Resampler } from "./resampler.js";

let failures = 0;
function check(name, ok, detail = "") {
  if (ok) {
    console.log(`ok   - ${name}`);
  } else {
    failures++;
    console.error(`FAIL - ${name}${detail ? ": " + detail : ""}`);
  }
}

// Feed `input` through a resampler in fixed-size blocks (mimicking the
// 128-sample render quanta the worklet sees) and concatenate the output.
function runBlocked(rs, input, block) {
  const chunks = [];
  let total = 0;
  for (let i = 0; i < input.length; i += block) {
    const out = rs.process(input.subarray(i, Math.min(i + block, input.length)));
    chunks.push(out.slice());
    total += out.length;
  }
  const all = new Float32Array(total);
  let off = 0;
  for (const ch of chunks) {
    all.set(ch, off);
    off += ch.length;
  }
  return all;
}

function sine(freq, rate, n, amp = 0.5) {
  const a = new Float32Array(n);
  for (let i = 0; i < n; i++) a[i] = amp * Math.sin((2 * Math.PI * freq * i) / rate);
  return a;
}

// ---- 1. Equal rates are lossless (the 48 kHz fast path everyone hits) ----
{
  const rs = new Resampler(48000, 48000);
  const input = sine(440, 48000, 48000 * 0.2); // 0.2 s
  const out = runBlocked(rs, input, 128);
  // Output equals input value-for-value; only the last couple of samples
  // lag pending (constant 2-sample group delay), so compare the prefix.
  let maxErr = 0;
  for (let k = 0; k < out.length; k++) maxErr = Math.max(maxErr, Math.abs(out[k] - input[k]));
  check("equal-rate output reproduces input exactly", maxErr < 1e-6, `maxErr=${maxErr}`);
  check("equal-rate lag is at most 2 samples", input.length - out.length <= 2, `lag=${input.length - out.length}`);
}

// ---- 2. Block size is irrelevant: streaming state makes the output
//         identical whether fed in 1 chunk or many (no boundary clicks) ----
{
  const input = sine(440, 44100, 44100 * 0.1);
  const big = runBlocked(new Resampler(44100, 48000), input, input.length);
  const small = runBlocked(new Resampler(44100, 48000), input, 128);
  const odd = runBlocked(new Resampler(44100, 48000), input, 37);
  let maxErr = 0;
  const m = Math.min(big.length, small.length, odd.length);
  for (let k = 0; k < m; k++) {
    maxErr = Math.max(maxErr, Math.abs(big[k] - small[k]), Math.abs(big[k] - odd[k]));
  }
  check("output is independent of block size", maxErr < 1e-6, `maxErr=${maxErr}`);
  check("block-size variants agree on length (±1)", Math.abs(big.length - small.length) <= 1 && Math.abs(big.length - odd.length) <= 1);
}

// Best-fit RMS of `out` against an analytic sine at `rate`/`freq`, scanning
// a small fractional delay to absorb the resampler's constant group delay.
function bestFitRms(out, freq, rate, amp = 0.5) {
  let best = Infinity;
  for (let shift = -4; shift <= 4; shift += 0.02) {
    let sse = 0;
    for (let k = 0; k < out.length; k++) {
      const ref = amp * Math.sin((2 * Math.PI * freq * (k - shift)) / rate);
      const e = out[k] - ref;
      sse += e * e;
    }
    best = Math.min(best, Math.sqrt(sse / out.length));
  }
  return best;
}

// ---- 3. Upsample 44.1 -> 48 kHz: correct length, clean sine, no clipping ----
{
  const seconds = 0.25;
  const input = sine(440, 44100, Math.round(44100 * seconds));
  const out = runBlocked(new Resampler(44100, 48000), input, 128);
  const expected = 48000 * seconds;
  check("upsample length matches rate ratio (±0.2%)", Math.abs(out.length - expected) / expected < 0.002, `len=${out.length} expected≈${expected}`);
  const rms = bestFitRms(out, 440, 48000);
  check("upsampled 440 Hz sine is accurate (RMS err < 2e-3)", rms < 2e-3, `rms=${rms}`);
  let peak = 0;
  for (let k = 0; k < out.length; k++) peak = Math.max(peak, Math.abs(out[k]));
  check("upsample does not clip (peak ≤ 0.51)", peak <= 0.51, `peak=${peak}`);
}

// ---- 4. Downsample 48 -> 44.1 kHz: correct length, clean sine ----
{
  const seconds = 0.25;
  const input = sine(440, 48000, Math.round(48000 * seconds));
  const out = runBlocked(new Resampler(48000, 44100), input, 128);
  const expected = 44100 * seconds;
  check("downsample length matches rate ratio (±0.2%)", Math.abs(out.length - expected) / expected < 0.002, `len=${out.length} expected≈${expected}`);
  const rms = bestFitRms(out, 440, 44100);
  check("downsampled 440 Hz sine is accurate (RMS err < 2e-3)", rms < 2e-3, `rms=${rms}`);
}

// ---- 5. Long-run length stays on-ratio (no drift / no sample drops) ----
for (const [inR, outR] of [[44100, 48000], [48000, 44100], [16000, 48000], [48000, 48000]]) {
  const input = sine(300, inR, inR * 2); // 2 s
  const out = runBlocked(new Resampler(inR, outR), input, 128);
  const expected = outR * 2;
  check(`long-run length on ratio ${inR}->${outR} (±0.1%)`, Math.abs(out.length - expected) / expected < 0.001, `len=${out.length} expected≈${expected}`);
}

// ---- 6. Two converters fed an equal-length block on every call stay
//         length-aligned. This is the invariant the stereo capture worklet
//         relies on (it interleaves lo[j]/ro[j] up to lo.length); if the
//         two per-channel converters' output lengths ever diverged, the
//         interleave would read out of bounds and misalign L/R. ----
{
  const a = new Resampler(44100, 48000);
  const b = new Resampler(44100, 48000);
  let aligned = true;
  let detail = "";
  for (let k = 0; k < 1000; k++) {
    const L = sine(440, 44100, 128, 0.5);
    // Mix "mono" calls (both fed the same buffer) with "stereo" calls
    // (different content, same length) — the worklet always feeds both.
    const R = k % 3 === 0 ? L : sine(330, 44100, 128, 0.4);
    const lo = a.process(L);
    const ro = b.process(R);
    if (lo.length !== ro.length) {
      aligned = false;
      detail = `call ${k}: lo=${lo.length} ro=${ro.length}`;
      break;
    }
  }
  check("two converters fed equal-length input stay length-aligned", aligned, detail);
}

// ---- 7. Empty and short input blocks (start/stop transitions can deliver
//         fewer than 128 samples) are handled without error and preserve
//         continuity: feeding ragged block sizes equals feeding the same
//         samples in uniform blocks. ----
{
  const full = sine(440, 44100, 1000);
  const rs = new Resampler(44100, 48000);
  const streamed = [];
  let consumed = 0;
  for (const sz of [0, 1, 2, 0, 128, 3, 0, 200, 1, 128]) {
    const blk = full.subarray(consumed, consumed + sz);
    consumed += sz;
    for (const v of rs.process(blk)) streamed.push(v);
  }
  const ref = runBlocked(new Resampler(44100, 48000), full.subarray(0, consumed), 64);
  const m = Math.min(streamed.length, ref.length);
  let maxErr = 0;
  for (let k = 0; k < m; k++) maxErr = Math.max(maxErr, Math.abs(streamed[k] - ref[k]));
  check(
    "empty/short blocks don't throw and preserve continuity",
    streamed.length > 0 && maxErr < 1e-6,
    `len=${streamed.length} maxErr=${maxErr}`,
  );
}

if (failures > 0) {
  console.error(`\n${failures} assertion(s) failed`);
  process.exit(1);
}
console.log("\nall resampler assertions passed");
