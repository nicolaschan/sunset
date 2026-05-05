// Browser-side voice codec wrapper around WebCodecs.
//
// Maps `client.voice_input(payload, codec_id)` ←→ `client.voice_start`'s
// `on_frame(peerId, payload, codec_id)` to the browser's WebCodecs
// `AudioEncoder` / `AudioDecoder` (Opus) so two peers can talk without
// shipping a wasm-side codec at all. Falls back to a `pcm-f32-le`
// passthrough when WebCodecs Opus encoder is unavailable
// (e.g. older Firefox builds — see
// docs/superpowers/specs/2026-04-30-sunset-voice-codec-decision.md).
//
// API surface:
//   * `await detectOpusSupport()` → `{encoder, decoder}` booleans.
//   * `new CaptureCodec({ sampleRate, frameSamples, bitrate, onEncoded })`
//     → `await codec.start(); codec.encode(pcm); codec.stop()`.
//     `onEncoded(payloadBytes, codec_id)` fires per encoded frame.
//   * `new PlaybackCodec({ sampleRate, frameSamples, onPcm })`
//     → `codec.decode(payloadBytes, codec_id); codec.stop()`.
//     `onPcm(Float32Array(frameSamples))` fires per decoded frame.
//
// PCM fallback contract: `payload` for `pcm-f32-le` is the raw
// little-endian bytes of `frameSamples` IEEE-754 f32 samples
// (3840 bytes for a 20 ms / 960-sample frame). Matches the Rust
// `sunset_voice::CODEC_ID` constant.

const CODEC_OPUS = "opus";
const CODEC_PCM = "pcm-f32-le";

const OPUS_BITRATE = 24_000; // 24 kbps — VoIP-quality at 48 kHz mono.
const SAMPLE_RATE = 48_000;
const FRAME_SAMPLES = 960; // 20 ms at 48 kHz.

let _opusSupportPromise = null;

// Cache the result so repeated callers don't pay an `isConfigSupported`
// round-trip every time. The browser's WebCodecs feature surface is
// constant for the lifetime of the page.
export function detectOpusSupport() {
  if (_opusSupportPromise) return _opusSupportPromise;
  _opusSupportPromise = (async () => {
    if (typeof AudioEncoder === "undefined" || typeof AudioDecoder === "undefined") {
      return { encoder: false, decoder: false };
    }
    const encConfig = {
      codec: CODEC_OPUS,
      sampleRate: SAMPLE_RATE,
      numberOfChannels: 1,
      bitrate: OPUS_BITRATE,
    };
    const decConfig = {
      codec: CODEC_OPUS,
      sampleRate: SAMPLE_RATE,
      numberOfChannels: 1,
    };
    let encoder = false;
    let decoder = false;
    try {
      const r = await AudioEncoder.isConfigSupported(encConfig);
      encoder = !!(r && r.supported);
    } catch (_e) {
      encoder = false;
    }
    try {
      const r = await AudioDecoder.isConfigSupported(decConfig);
      decoder = !!(r && r.supported);
    } catch (_e) {
      decoder = false;
    }
    return { encoder, decoder };
  })();
  return _opusSupportPromise;
}

// Test-only override hook: lets Playwright force the PCM-passthrough
// fallback path so the deterministic counter-based byte-equality tests
// don't have to reason about Opus's lossy output. Setting
// `window.__SUNSET_VOICE_FORCE_PCM = true` before the AudioContext
// is created makes both `CaptureCodec` and `PlaybackCodec` skip
// WebCodecs entirely.
function forcePcmFromGlobal() {
  return typeof window !== "undefined" && !!window.__SUNSET_VOICE_FORCE_PCM;
}

// One encoder per local mic. Stateful — Opus relies on the encoder
// retaining inter-frame entropy coder state for compression efficiency.
export class CaptureCodec {
  constructor({ onEncoded, sampleRate = SAMPLE_RATE, frameSamples = FRAME_SAMPLES, bitrate = OPUS_BITRATE } = {}) {
    this.onEncoded = onEncoded;
    this.sampleRate = sampleRate;
    this.frameSamples = frameSamples;
    this.bitrate = bitrate;
    this.encoder = null;
    this.codecId = CODEC_PCM;
    // AudioData timestamps are in microseconds. We accumulate frame
    // durations rather than reading a wall-clock so the encoder sees
    // a perfectly monotonic stream regardless of host scheduling.
    this.timestampUs = 0;
    this.framesEnqueued = 0;
  }

  async start() {
    if (forcePcmFromGlobal()) {
      this.codecId = CODEC_PCM;
      return;
    }
    const supp = await detectOpusSupport();
    if (!supp.encoder) {
      this.codecId = CODEC_PCM;
      return;
    }
    this.codecId = CODEC_OPUS;
    this.encoder = new AudioEncoder({
      output: (chunk) => {
        const buf = new Uint8Array(chunk.byteLength);
        chunk.copyTo(buf);
        try {
          this.onEncoded(buf, this.codecId);
        } catch (e) {
          console.warn("CaptureCodec onEncoded threw", e);
        }
      },
      error: (e) => {
        console.warn("AudioEncoder error", e);
        // Stay on whatever codec we configured; further encode() calls
        // may fail but the receive side will still work.
      },
    });
    this.encoder.configure({
      codec: CODEC_OPUS,
      sampleRate: this.sampleRate,
      numberOfChannels: 1,
      bitrate: this.bitrate,
    });
  }

  // pcm: Float32Array(frameSamples) mono @ sampleRate.
  encode(pcm) {
    if (!(pcm instanceof Float32Array) || pcm.length !== this.frameSamples) {
      // Reject silently — the worklet only ever sends right-shaped frames;
      // anything else is a bug we'd rather not crash for.
      return;
    }
    if (!this.encoder) {
      // PCM passthrough: bytewise view of the same Float32Array buffer.
      // Copy because the worklet transfers the underlying ArrayBuffer.
      const bytes = new Uint8Array(pcm.byteLength);
      bytes.set(new Uint8Array(pcm.buffer, pcm.byteOffset, pcm.byteLength));
      try {
        this.onEncoded(bytes, this.codecId);
      } catch (e) {
        console.warn("CaptureCodec onEncoded threw", e);
      }
      return;
    }
    const audioData = new AudioData({
      format: "f32",
      sampleRate: this.sampleRate,
      numberOfFrames: pcm.length,
      numberOfChannels: 1,
      timestamp: this.timestampUs,
      data: pcm,
    });
    this.timestampUs += Math.round((pcm.length * 1_000_000) / this.sampleRate);
    this.framesEnqueued += 1;
    try {
      this.encoder.encode(audioData);
    } catch (e) {
      console.warn("AudioEncoder.encode threw", e);
    }
    audioData.close();
  }

  stop() {
    if (this.encoder) {
      try {
        this.encoder.close();
      } catch (_e) {
        // noop
      }
      this.encoder = null;
    }
  }
}

// One decoder per peer. Created lazily on first packet because
// `codec_id` is delivered with each frame (and may legitimately differ
// from the local capture codec — we negotiate per-direction, not per-room).
export class PlaybackCodec {
  constructor({ onPcm, sampleRate = SAMPLE_RATE, frameSamples = FRAME_SAMPLES } = {}) {
    this.onPcm = onPcm;
    this.sampleRate = sampleRate;
    this.frameSamples = frameSamples;
    this.decoder = null;
    this.activeCodecId = null;
    // Synthesised monotonic timestamp for AudioDecoder. Wire packets
    // carry seq + sender_time_ms but the decoder only requires
    // monotonicity — we accumulate frame durations.
    this.timestampUs = 0;
  }

  // Lazily build the decoder for `codecId`. Idempotent for the same id;
  // re-init if the peer switches codecs mid-stream (rare but allowed).
  ensureFor(codecId) {
    if (this.activeCodecId === codecId && (codecId === CODEC_PCM || this.decoder)) {
      return;
    }
    if (this.decoder) {
      try {
        this.decoder.close();
      } catch (_e) {
        // noop
      }
      this.decoder = null;
    }
    this.activeCodecId = codecId;
    if (codecId === CODEC_PCM) {
      // No decoder needed; bytes are already PCM.
      return;
    }
    if (codecId === CODEC_OPUS) {
      if (forcePcmFromGlobal() || typeof AudioDecoder === "undefined") {
        // Caller will hit decode() below and we'll skip the chunk.
        return;
      }
      this.decoder = new AudioDecoder({
        output: (audioData) => {
          try {
            // Mono → planeIndex 0; format "f32" is interleaved-equivalent
            // for 1-channel data and works on all current browsers.
            const out = new Float32Array(audioData.numberOfFrames);
            audioData.copyTo(out, { planeIndex: 0, format: "f32" });
            this.onPcm(out);
          } catch (e) {
            console.warn("PlaybackCodec onPcm threw", e);
          } finally {
            audioData.close();
          }
        },
        error: (e) => {
          console.warn("AudioDecoder error", e);
        },
      });
      this.decoder.configure({
        codec: CODEC_OPUS,
        sampleRate: this.sampleRate,
        numberOfChannels: 1,
      });
    }
  }

  // payload: Uint8Array, codecId: string.
  decode(payload, codecId) {
    this.ensureFor(codecId);
    if (codecId === CODEC_PCM) {
      const expected = this.frameSamples * 4;
      if (payload.byteLength !== expected) {
        // Wrong size — drop silently rather than feeding partial samples
        // to the playback worklet.
        return;
      }
      // Build a fresh Float32Array view that aligns with payload's bytes.
      // Copy to dodge alignment issues with the underlying buffer.
      const out = new Float32Array(this.frameSamples);
      const view = new DataView(payload.buffer, payload.byteOffset, payload.byteLength);
      for (let i = 0; i < this.frameSamples; i++) {
        out[i] = view.getFloat32(i * 4, true);
      }
      try {
        this.onPcm(out);
      } catch (e) {
        console.warn("PlaybackCodec onPcm threw", e);
      }
      return;
    }
    if (codecId === CODEC_OPUS) {
      if (!this.decoder) {
        // Decoder not available (forced PCM, or AudioDecoder undefined).
        // Drop the chunk; the playback worklet pads silence.
        return;
      }
      const chunk = new EncodedAudioChunk({
        type: "key",
        timestamp: this.timestampUs,
        // 20 ms in microseconds.
        duration: Math.round((this.frameSamples * 1_000_000) / this.sampleRate),
        data: payload,
      });
      this.timestampUs += Math.round((this.frameSamples * 1_000_000) / this.sampleRate);
      try {
        this.decoder.decode(chunk);
      } catch (e) {
        console.warn("AudioDecoder.decode threw", e);
      }
      return;
    }
    // Unknown codec — drop. Not throwing keeps a misbehaving peer from
    // crashing the local audio path.
  }

  stop() {
    if (this.decoder) {
      try {
        this.decoder.close();
      } catch (_e) {
        // noop
      }
      this.decoder = null;
    }
    this.activeCodecId = null;
    this.timestampUs = 0;
  }
}
