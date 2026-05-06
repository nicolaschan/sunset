// AudioWorkletProcessor that receives 1920-sample (20 ms × 2 ch)
// interleaved L/R stereo PCM frames via postMessage, deinterleaves
// them, and writes them out into the audio engine's 128-sample
// stereo quanta as the rendering pipeline pulls.
//
// The producer is `sunset_voice::VoiceDecoder`, which is fixed at
// 2-channel decode regardless of the sender's quality preset (mono
// Opus packets are auto-upmixed to stereo at decode time). That
// means this worklet's input shape is constant, so we don't need to
// renegotiate channel counts when a peer changes their send-side
// quality.
//
// Underflow (queue empty when the engine pulls) is filled with zeros
// for the missing samples. The runtime's jitter pump tries to keep
// the queue topped up — sustained underflow means the network or
// decoder is behind the consumer, which is audible and is what we
// want a real user to hear.

const FRAME_SAMPLES_PER_CHANNEL = 960;
const CHANNELS = 2;
const FRAME_TOTAL = FRAME_SAMPLES_PER_CHANNEL * CHANNELS;

class VoicePlaybackProcessor extends AudioWorkletProcessor {
  constructor() {
    super();
    this.queue = []; // array of Float32Array (interleaved L/R)
    this.head = null;
    this.headIdx = 0; // sample-pair index into head (0..FRAME_SAMPLES_PER_CHANNEL)
    this.port.onmessage = (e) => {
      // Defensive: only accept Float32Arrays of the expected length.
      if (
        e.data instanceof Float32Array &&
        e.data.length === FRAME_TOTAL
      ) {
        this.queue.push(e.data);
      }
    };
  }

  process(_inputs, outputs) {
    // outputs[0] is an array of channels. With a 2-channel destination
    // (set by the AudioContext) we get [L, R].
    const channels = outputs[0];
    if (!channels || channels.length === 0) return true;
    const left = channels[0];
    const right = channels[1] ?? channels[0];

    const n = left.length;
    let i = 0;
    while (i < n) {
      if (!this.head) {
        if (this.queue.length === 0) {
          // Underflow — pad the rest of this quantum with silence on
          // both channels.
          left.fill(0, i);
          if (right !== left) right.fill(0, i);
          return true;
        }
        this.head = this.queue.shift();
        this.headIdx = 0;
      }
      const remainingPairs = FRAME_SAMPLES_PER_CHANNEL - this.headIdx;
      const take = Math.min(remainingPairs, n - i);
      for (let k = 0; k < take; k++) {
        const off = (this.headIdx + k) * CHANNELS;
        left[i + k] = this.head[off];
        if (right !== left) right[i + k] = this.head[off + 1];
      }
      this.headIdx += take;
      i += take;
      if (this.headIdx === FRAME_SAMPLES_PER_CHANNEL) {
        this.head = null;
      }
    }
    return true;
  }
}

registerProcessor("voice-playback", VoicePlaybackProcessor);
