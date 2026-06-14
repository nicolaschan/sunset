// AudioWorkletProcessor that resamples the mic to the fixed 48 kHz codec
// rate and buffers it into 960-sample (20 ms) interleaved L/R stereo
// frames, posting each completed frame to the main thread.
//
// The main thread receives Float32Array(1920) (960 per channel × 2)
// and forwards it to client.voice_input. The Rust runtime downmixes
// to mono inside `send_pcm` when the active quality preset is Voice.
//
// Why resample here: the capture AudioContext runs at the audio *device*
// rate (so the mic MediaStream always connects — see voice.ffi.mjs), but
// Opus and the JS<->Rust frame contract are fixed at 48 kHz. The
// per-channel Resampler converts the device rate (= `sampleRate`, the
// worklet global) up to 48 kHz so a posted frame is always exactly
// FRAME_SAMPLES_PER_CHANNEL @ 48 kHz no matter the hardware. At 48 kHz
// hardware the resampler is a lossless passthrough.
//
// Why always stereo even for the Voice preset:
//
//   - The capture worklet's AudioContext is created once per voice
//     session; switching channel count would require tearing down the
//     mic stream + worklet node and re-running getUserMedia, which on
//     mobile triggers a permission re-prompt and audible glitching.
//   - The downmix (L+R)/2 in Rust costs ~960 multiply-adds per 20 ms
//     frame — well below 1% of a single core.

import { Resampler } from "./resampler.js";

// 48 kHz codec rate. Must agree with `SAMPLE_RATE` in
// crates/sunset-voice/src/lib.rs (the rate the Opus encoder runs at).
const CODEC_SAMPLE_RATE = 48000;

const FRAME_SAMPLES_PER_CHANNEL = 960;
const CHANNELS = 2;
const FRAME_TOTAL = FRAME_SAMPLES_PER_CHANNEL * CHANNELS;

class VoiceCaptureProcessor extends AudioWorkletProcessor {
  constructor() {
    super();
    this.buf = new Float32Array(FRAME_TOTAL);
    this.idx = 0;
    // One converter per channel, device rate -> 48 kHz. `sampleRate` is
    // the AudioWorkletGlobalScope global (the capture context's rate).
    this.rsL = new Resampler(sampleRate, CODEC_SAMPLE_RATE);
    this.rsR = new Resampler(sampleRate, CODEC_SAMPLE_RATE);
  }

  process(inputs) {
    // First input. May be undefined briefly during stream start/end
    // transitions. Channels:
    //   - Mono mic: inputs[0] = [Float32Array]
    //   - Stereo mic: inputs[0] = [Float32Array(L), Float32Array(R)]
    const channels = inputs[0];
    if (!channels || channels.length === 0 || !channels[0]) return true;
    const left = channels[0];
    const right = channels[1] ?? channels[0]; // mono input → duplicate L into R

    // Resample each channel to 48 kHz. Both converters are fed an
    // equal-length quantum on EVERY call (left and right are the same
    // render-quantum length — a mono quantum feeds both the same buffer),
    // so their fractional phases advance in lockstep and the outputs are
    // always the same length and sample-aligned. We must never skip
    // feeding one converter (e.g. on a mono frame): that would desync
    // their phases permanently and misalign L/R for the rest of the
    // session.
    const lo = this.rsL.process(left);
    const ro = this.rsR.process(right);

    for (let j = 0; j < lo.length; j++) {
      this.buf[this.idx++] = lo[j];
      this.buf[this.idx++] = ro[j];
      if (this.idx === FRAME_TOTAL) {
        // Transfer the buffer's underlying storage so we don't copy.
        // Allocate a fresh one for the next frame.
        const out = this.buf;
        this.port.postMessage(out, [out.buffer]);
        this.buf = new Float32Array(FRAME_TOTAL);
        this.idx = 0;
      }
    }
    return true;
  }
}

registerProcessor("voice-capture", VoiceCaptureProcessor);
