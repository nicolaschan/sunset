// AudioWorkletProcessor that receives 960-sample (20 ms) mono PCM
// frames via postMessage, queues them, and writes them out into the
// audio engine's 128-sample quanta as the rendering pipeline pulls.
//
// Underflow (queue empty when the engine pulls) is filled with zeros
// for the missing samples. C2c may add a jitter buffer; C2a accepts
// dropouts as audible feedback that something is misaligned.

class VoicePlaybackProcessor extends AudioWorkletProcessor {
  constructor() {
    super();
    this.queue = []; // array of Float32Array
    this.head = null;
    this.headIdx = 0;
    this.port.onmessage = (e) => {
      // Defensive: only accept Float32Arrays of the expected length.
      if (e.data instanceof Float32Array && e.data.length === 960) {
        this.queue.push(e.data);
      }
    };
  }

  process(_inputs, outputs) {
    const out = outputs[0]?.[0];
    if (!out) return true;

    let i = 0;
    while (i < out.length) {
      if (!this.head) {
        if (this.queue.length === 0) {
          // Underflow — pad the rest of this quantum with silence.
          out.fill(0, i);
          return true;
        }
        this.head = this.queue.shift();
        this.headIdx = 0;
      }
      const remaining = this.head.length - this.headIdx;
      const take = Math.min(remaining, out.length - i);
      out.set(this.head.subarray(this.headIdx, this.headIdx + take), i);
      this.headIdx += take;
      i += take;
      if (this.headIdx === this.head.length) {
        this.head = null;
      }
    }
    return true;
  }
}

registerProcessor("voice-playback", VoicePlaybackProcessor);
