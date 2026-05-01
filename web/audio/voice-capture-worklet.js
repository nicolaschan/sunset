// AudioWorkletProcessor that buffers raw 128-sample quanta from the
// browser's audio engine into 960-sample (20 ms) mono frames and posts
// each completed frame to the main thread.
//
// Used by web/voice-demo.html (C2a) and the eventual production
// wiring. The main thread receives Float32Array(960) and forwards it
// to client.voice_input.

class VoiceCaptureProcessor extends AudioWorkletProcessor {
  constructor() {
    super();
    this.buf = new Float32Array(960);
    this.idx = 0;
  }

  process(inputs) {
    // First input, first channel (mono). May be undefined briefly
    // during stream start/end transitions.
    const ch = inputs[0]?.[0];
    if (!ch) return true;

    let i = 0;
    while (i < ch.length) {
      const room = 960 - this.idx;
      const take = Math.min(room, ch.length - i);
      this.buf.set(ch.subarray(i, i + take), this.idx);
      this.idx += take;
      i += take;

      if (this.idx === 960) {
        // Transfer the buffer's underlying storage so we don't copy.
        // Allocate a fresh one for the next frame.
        const out = this.buf;
        this.port.postMessage(out, [out.buffer]);
        this.buf = new Float32Array(960);
        this.idx = 0;
      }
    }
    return true;
  }
}

registerProcessor("voice-capture", VoiceCaptureProcessor);
