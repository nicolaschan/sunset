// Streaming sample-rate converter — one instance per audio channel.
//
// Why this exists: Opus and the JS<->Rust voice frame contract are fixed
// at 48 kHz, but the browser's *capture* AudioContext has to run at the
// audio device's native rate. 44.1 kHz hardware is common, and some
// browsers (notably Firefox) refuse `createMediaStreamSource` when the
// mic stream's rate differs from the AudioContext's — so we let the
// capture context adopt the device rate (the mic always matches it) and
// convert device-rate samples up to 48 kHz here instead.
//
// `process` is fed successive render quanta (128 samples on most engines)
// and returns the output samples producible so far, carrying the
// fractional read position and three samples of interpolation history
// across calls so block boundaries join seamlessly (no periodic clicks).
//
// Interpolation is Catmull-Rom cubic: cheap (four taps), and accurate
// enough that speech-band artifacts stay inaudible at the common
// 44.1<->48 kHz ratio. At t=0 the kernel returns its on-grid sample
// exactly, so when input and output rates are equal the converter is
// lossless — it reproduces the input values (a constant sub-sample group
// delay aside). That keeps the overwhelmingly common already-48 kHz case
// — and the whole existing test suite — bit-for-bit unchanged.
export class Resampler {
  constructor(inputRate, outputRate) {
    // Input samples consumed per output sample.
    this.step = inputRate / outputRate;
    // The three most-recent input samples, oldest -> newest. The cubic
    // kernel reads one sample behind and two ahead of each interpolation
    // point; retaining three carries the "behind" neighbour across block
    // boundaries and covers the small negative read offset the fractional
    // phase can leave pending between blocks.
    this.h0 = 0;
    this.h1 = 0;
    this.h2 = 0;
    // Fractional position of the next output sample, in input-sample units
    // relative to the start of the next input block. 0 => the first output
    // samples the first input sample.
    this.pos = 0;
  }

  // Convert one block of input samples to the output rate. Output length
  // varies block to block; the fractional remainder is retained for the
  // next call.
  process(input) {
    const n = input.length;
    const step = this.step;

    // Combined stream c = [h0, h1, h2, ...input], so c[0..2] are history
    // and c[3 + j] === input[j]. Indexing through one array keeps the
    // kernel's four-tap reads (c[i-1..i+2]) branch-free.
    const c = new Float32Array(n + 3);
    c[0] = this.h0;
    c[1] = this.h1;
    c[2] = this.h2;
    c.set(input, 3);

    // At most one output per `step` input samples; round up and trim.
    const out = new Float32Array(Math.ceil((n + 3) / step) + 2);
    let count = 0;

    // Position in c-coordinates (input coordinates + 3 history samples).
    let cpos = this.pos + 3;
    let i = Math.floor(cpos);
    // Emit while the kernel's four taps c[i-1 .. i+2] are all in range.
    while (i >= 1 && i + 2 <= n + 2) {
      const t = cpos - i;
      const p0 = c[i - 1];
      const p1 = c[i];
      const p2 = c[i + 1];
      const p3 = c[i + 2];
      // Catmull-Rom; at t=0 this collapses to p1 exactly.
      out[count++] =
        0.5 *
        (2 * p1 +
          (p2 - p0) * t +
          (2 * p0 - 5 * p1 + 4 * p2 - p3) * t * t +
          (3 * p1 - 3 * p2 + p3 - p0) * t * t * t);
      cpos += step;
      i = Math.floor(cpos);
    }

    // Carry state: the last three samples of the combined stream become
    // the new history, and the read position shifts back by this block's
    // length so it is relative to the next block's input[0].
    this.h0 = c[n];
    this.h1 = c[n + 1];
    this.h2 = c[n + 2];
    this.pos = cpos - 3 - n;

    return out.subarray(0, count);
  }
}
