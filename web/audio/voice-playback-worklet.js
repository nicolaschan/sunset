// AudioWorkletProcessor that receives 1920-sample (20 ms × 2 ch)
// interleaved L/R stereo PCM frames via postMessage and drains them
// at the audio device clock, smoothing over network jitter and
// dropped packets so the listener doesn't hear clicks.
//
// Why this lives in the worklet and not the Rust side:
// `process()` is paced by the device sample-rate crystal. A
// JS / Rust wall-clock timer (e.g. tokio::time::sleep(20ms)) drifts
// against that crystal — when it fires late under load or in a
// backgrounded tab, the worklet's queue empties between ticks and
// the listener hears the gap. Driving consumption from `process()`
// makes that whole class of bug impossible.
//
// Design (spec: docs/superpowers/specs/2026-05-10-voice-smooth-jitter-buffer-design.md):
//
//   - Buffer: Map<seq, Float32Array>. Sequence numbers come from the
//     wire (`VoicePacket::Frame::seq`, truncated to u32). Frames
//     arrive nearly in order over WebRTC but the seq-indexed map
//     handles reorders inside the playout-depth window for free.
//   - States: Warmup | Playing | Underrun.
//   - Cosine fades at every Playing↔Underrun transition kill the
//     phase / step discontinuities that produce clicks. The
//     fade-out fades the *last emitted sample value* down to zero
//     (not the lastFrame's samples) so the fade is unambiguously
//     continuous with what the listener just heard.
//   - Target playout depth = 3 frames (60 ms). Trades 60 ms of
//     latency for click-free playback under typical network jitter.
//   - Max depth = 10 frames (200 ms). Beyond this we drop oldest
//     because we're either receiving a burst-catch-up or the
//     sender's clock is meaningfully faster than ours; better to
//     drop than grow unbounded.

const FRAME_SAMPLES_PER_CHANNEL = 960;
const CHANNELS = 2;
const FRAME_TOTAL = FRAME_SAMPLES_PER_CHANNEL * CHANNELS;

// Number of buffered frames before playback starts (Warmup → Playing)
// and the same threshold for recovering from Underrun → Playing.
const TARGET_PLAYOUT_DEPTH = 3;

// Hard cap on buffered frames. If we hit it, the lowest-seq entry is
// dropped — handles sustained sender-faster-than-receiver clock drift
// and rare burst arrivals where multiple frames land in one event-loop
// turn.
const MAX_DEPTH = 10;

// Cosine fade window in *sample pairs* (one pair = one L+R sample).
// 240 pairs at 48 kHz = 5 ms. Short enough to not noticeably stretch
// gaps, long enough to be inaudible as a transient (the ear's
// transient threshold for music is ~10 ms; speech is even more
// forgiving).
const FADE_SAMPLES = 240;

const STATE_WARMUP = 0;
const STATE_PLAYING = 1;
const STATE_UNDERRUN = 2;

// Modular comparison for u32 wire seqs. Treat `a` and `b` as living
// on a number line; ((a - b) | 0) is the signed 32-bit difference
// (JS bitwise ops sign-extend). Positive means a is "after" b.
function seqGt(a, b) {
  return ((a - b) | 0) > 0;
}

class VoicePlaybackProcessor extends AudioWorkletProcessor {
  constructor() {
    super();
    // Map<seq, Float32Array(FRAME_TOTAL)>. Key is u32.
    this.buf = new Map();
    // Smallest seq we'll accept. Below this is "already played";
    // null until first pop.
    this.expectedSeq = null;
    // The frame we're currently emitting. `headIdx` is the within-
    // frame sample-pair offset.
    this.head = null;
    this.headIdx = 0;
    // State machine.
    this.state = STATE_WARMUP;
    // Fade window positions (in sample pairs). FADE_SAMPLES = no
    // fade active. fadeIn ramps 0..1 across the first FADE_SAMPLES
    // samples after a Warmup/Underrun → Playing transition.
    // fadeOut ramps 1..0 across the first FADE_SAMPLES samples
    // after a Playing → Underrun transition.
    this.fadeInPos = FADE_SAMPLES;
    this.fadeOutPos = FADE_SAMPLES;
    // Last L/R sample actually written. Anchors the fade-out so it
    // starts continuous with what the listener just heard.
    this.lastL = 0;
    this.lastR = 0;

    this.port.onmessage = (e) => {
      const msg = e.data;
      if (
        !msg ||
        typeof msg.seq !== "number" ||
        !(msg.pcm instanceof Float32Array) ||
        msg.pcm.length !== FRAME_TOTAL
      ) {
        return;
      }
      const seq = msg.seq >>> 0;
      const pcm = msg.pcm;

      // Drop frames we've already played past.
      if (this.expectedSeq !== null && !seqGt(seq, this.expectedSeq - 1)) {
        return;
      }

      this.buf.set(seq, pcm);

      // Cap at MAX_DEPTH by evicting smallest seq. Map preserves
      // insertion order but we want smallest-by-seq; scan once on
      // overflow (rare).
      while (this.buf.size > MAX_DEPTH) {
        let minSeq = null;
        for (const k of this.buf.keys()) {
          if (minSeq === null || seqGt(minSeq, k)) minSeq = k;
        }
        if (minSeq !== null) this.buf.delete(minSeq);
        else break;
      }

      if (
        (this.state === STATE_WARMUP || this.state === STATE_UNDERRUN) &&
        this.buf.size >= TARGET_PLAYOUT_DEPTH
      ) {
        this.state = STATE_PLAYING;
        this.fadeInPos = 0;
      }
    };
  }

  popSmallest() {
    if (this.buf.size === 0) return null;
    let minSeq = null;
    for (const k of this.buf.keys()) {
      if (minSeq === null || seqGt(minSeq, k)) minSeq = k;
    }
    const pcm = this.buf.get(minSeq);
    this.buf.delete(minSeq);
    this.expectedSeq = (minSeq + 1) >>> 0;
    return pcm;
  }

  // Cosine ramp 0..1. pos ∈ [0, FADE_SAMPLES] → m ∈ [0, 1].
  fadeMul(pos) {
    return 0.5 - 0.5 * Math.cos((Math.PI * pos) / FADE_SAMPLES);
  }

  process(_inputs, outputs) {
    const channels = outputs[0];
    if (!channels || channels.length === 0) return true;
    const left = channels[0];
    const right = channels[1] ?? channels[0];
    const n = left.length;

    for (let i = 0; i < n; i++) {
      let outL = 0;
      let outR = 0;

      if (this.state === STATE_WARMUP) {
        // Silent until enough buffered (handled in onmessage).
        // outL, outR remain 0.
      } else if (this.state === STATE_UNDERRUN) {
        // Fade the last actually-written sample value down to zero,
        // then silence. This is unambiguously continuous: at
        // fadeOutPos=0 the multiplier is 1 → outL=lastL exactly.
        if (this.fadeOutPos < FADE_SAMPLES) {
          const m = this.fadeMul(FADE_SAMPLES - this.fadeOutPos);
          outL = this.lastL * m;
          outR = this.lastR * m;
          this.fadeOutPos++;
        }
        // else: silence (0).
      } else {
        // STATE_PLAYING — need a head frame.
        if (!this.head) {
          if (this.buf.size === 0) {
            // Buffer dried up mid-playback. Transition and re-do
            // this sample under the Underrun branch.
            this.state = STATE_UNDERRUN;
            this.fadeOutPos = 0;
            i--;
            continue;
          }
          this.head = this.popSmallest();
          this.headIdx = 0;
        }

        const off = this.headIdx * CHANNELS;
        outL = this.head[off];
        outR = this.head[off + 1];

        if (this.fadeInPos < FADE_SAMPLES) {
          // Ramp 0 → 1 over FADE_SAMPLES. At fadeInPos=0 the
          // multiplier is 0, so the first sample after a transition
          // is silent — continuous with the silence we just
          // emitted in Warmup or post-fade-out Underrun.
          const m = this.fadeMul(this.fadeInPos);
          outL *= m;
          outR *= m;
          this.fadeInPos++;
        }

        this.headIdx++;
        if (this.headIdx === FRAME_SAMPLES_PER_CHANNEL) {
          this.head = null;
        }
      }

      left[i] = outL;
      if (right !== left) right[i] = outR;
      this.lastL = outL;
      this.lastR = outR;
    }
    return true;
  }
}

registerProcessor("voice-playback", VoicePlaybackProcessor);
