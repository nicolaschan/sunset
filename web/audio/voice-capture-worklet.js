// AudioWorkletProcessor that buffers raw 128-sample quanta from the
// browser's audio engine into 960-sample (20 ms) interleaved L/R
// stereo frames and posts each completed frame to the main thread.
//
// The main thread receives Float32Array(1920) (960 per channel × 2)
// and forwards it to client.voice_input. The Rust runtime downmixes
// to mono inside `send_pcm` when the active quality preset is Voice.
//
// Why always stereo even for the Voice preset:
//
//   - The capture worklet's AudioContext is created once per voice
//     session; switching channel count would require tearing down the
//     mic stream + worklet node and re-running getUserMedia, which on
//     mobile triggers a permission re-prompt and audible glitching.
//   - The downmix (L+R)/2 in Rust costs ~960 multiply-adds per 20 ms
//     frame — well below 1% of a single core.

const FRAME_SAMPLES_PER_CHANNEL = 960;
const CHANNELS = 2;
const FRAME_TOTAL = FRAME_SAMPLES_PER_CHANNEL * CHANNELS;

class VoiceCaptureProcessor extends AudioWorkletProcessor {
  constructor() {
    super();
    this.buf = new Float32Array(FRAME_TOTAL);
    this.idx = 0;
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

    let i = 0;
    const n = left.length;
    while (i < n) {
      // Per quanta-step: we have `left.length` samples available; each
      // captured sample becomes 2 interleaved entries in `buf`. Cap at
      // whatever room is left in `buf` before we'd post the frame.
      const remainingPerCh = FRAME_SAMPLES_PER_CHANNEL - (this.idx >> 1);
      const take = Math.min(remainingPerCh, n - i);
      for (let k = 0; k < take; k++) {
        this.buf[this.idx++] = left[i + k];
        this.buf[this.idx++] = right[i + k];
      }
      i += take;

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
