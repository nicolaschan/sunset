// JS-side voice wiring. Owns:
// - the AudioContext (created lazily on first start)
// - per-peer { workletNode, gainNode, decoder } table
// - capture worklet stream + a shared CaptureCodec (WebCodecs Opus
//   encoder when supported, PCM passthrough otherwise)
// - per-peer GainNode updates (setPeerVolume), called directly from
//   the Gleam UI; not threaded through Rust (the GainNode is a
//   browser-shaped concept — see voice/mod.rs for the spec rationale).
//
// The runtime is codec-agnostic: `client.voice_input(payload, codec_id)`
// accepts opaque encoded bytes plus a `codec_id` string. The capture
// codec produces one `(payload, codec_id)` tuple per 20 ms PCM frame
// from the worklet; the receive callback hands `(peer_id, payload,
// codec_id)` back to JS so a per-peer `PlaybackCodec` can decode it
// and feed PCM to the per-peer `voice-playback` worklet.

import { Ok, Error as GError } from "../../prelude.mjs";

// Load the codec wrapper lazily at runtime — the dev/prod web server
// serves it from `/audio/voice-codec.js` but Lustre's bundler (esbuild
// /bun) tries to resolve any `import(...)` whose argument is a string
// literal at build time, which fails for absolute URLs. Building the
// URL through a non-literal expression hides it from the bundler so
// the import is left as a runtime-evaluated dynamic import. (Same
// trick as the worklet `addModule` calls below — those use string
// literals because `addModule` is a runtime API the bundler ignores.)
let _codecModulePromise = null;
function codecModuleUrl() {
  // Concatenated at runtime so the bundler can't statically constant-fold it.
  const root = "/audio/";
  const file = "voice-codec.js";
  return root + file;
}
function loadCodecModule() {
  if (!_codecModulePromise) {
    _codecModulePromise = import(/* @vite-ignore */ codecModuleUrl());
  }
  return _codecModulePromise;
}

let ctx = null;
const peers = new Map(); // peerHex -> { worklet, gain, codec }
let captureNode = null;
let captureStream = null;
let captureCodec = null;
let PlaybackCodecCtor = null;

// Test-only handle. The bundled module is otherwise unreachable from
// page.evaluate() because Lustre's prod build inlines it into sunset_web.js
// (no `/javascript/...` URL to dynamic-import). With `window.SUNSET_TEST`
// set we expose `setPeerVolume` / `getPeerGain` for per-peer GainNode
// drive + inspect, plus `stopCaptureSource` which detaches the mic
// capture worklet from the live MediaStream so deterministic
// `voice_inject_pcm` tests aren't polluted by fake-mic noise. No-op
// in production.
if (typeof window !== "undefined" && window.SUNSET_TEST) {
  window.__voiceFfi = {
    setPeerVolume: (peerHex, gain) => setPeerVolume(peerHex, gain),
    getPeerGain: (peerHex) => getPeerGain(peerHex),
    stopCaptureSource: () => stopCaptureSource(),
    activeCodecId: () => (captureCodec ? captureCodec.codecId : null),
  };
}

export function ensureCtx() {
  if (!ctx) ctx = new AudioContext({ sampleRate: 48000 });
  return ctx;
}

export async function startCapture(client) {
  ensureCtx();
  await ctx.audioWorklet.addModule("/audio/voice-capture-worklet.js");
  await ctx.audioWorklet.addModule("/audio/voice-playback-worklet.js");
  captureStream = await navigator.mediaDevices.getUserMedia({
    audio: {
      echoCancellation: true,
      noiseSuppression: true,
      autoGainControl: true,
      channelCount: 1,
    },
  });
  const src = ctx.createMediaStreamSource(captureStream);
  captureNode = new AudioWorkletNode(ctx, "voice-capture");

  // Build the encoder before wiring the worklet so the first PCM frame
  // doesn't race the async configure() inside CaptureCodec.start().
  const codecModule = await loadCodecModule();
  PlaybackCodecCtor = codecModule.PlaybackCodec;
  captureCodec = new codecModule.CaptureCodec({
    onEncoded: (payload, codecId) => {
      try {
        client.voice_input(payload, codecId);
      } catch (err) {
        console.warn("voice_input failed", err);
      }
    },
  });
  await captureCodec.start();

  captureNode.port.onmessage = (e) => {
    if (e.data instanceof Float32Array && e.data.length === 960) {
      try {
        captureCodec.encode(e.data);
      } catch (err) {
        console.warn("captureCodec.encode failed", err);
      }
    }
  };
  src.connect(captureNode);
}

export function stopCapture() {
  if (captureStream) {
    for (const t of captureStream.getTracks()) t.stop();
    captureStream = null;
  }
  captureNode = null;
  if (captureCodec) {
    captureCodec.stop();
    captureCodec = null;
  }
  for (const [_peer, slot] of peers) {
    try {
      slot.worklet.disconnect();
      slot.gain.disconnect();
    } catch (_e) {
      // ignore disconnect errors
    }
    if (slot.codec) slot.codec.stop();
  }
  peers.clear();
}

// Test-only: silence the capture worklet path so the fake mic
// (Chromium's --use-fake-device-for-media-stream supplies a continuous
// tone) stops feeding `client.voice_input`. We:
//   - stop all live MediaStream tracks (no new audio enters the worklet)
//   - clear the worklet's message handler (any already-queued postMessage
//     frames from the worklet drop on the floor instead of reaching
//     `voice_input`, which is what would otherwise leak into the
//     deterministic per-counter checksum assertion in tests).
// Keeps the per-peer playback chain alive so injected frames still
// flow. Idempotent. No-op in production (only the SUNSET_TEST handle
// calls this).
function stopCaptureSource() {
  if (captureStream) {
    for (const t of captureStream.getTracks()) t.stop();
    captureStream = null;
  }
  if (captureNode) {
    captureNode.port.onmessage = null;
  }
}

// Per-peer slot accessor that lazily builds the per-peer playback
// chain (GainNode + AudioWorkletNode + PlaybackCodec) on first frame.
// Decoders are per-peer because Opus is stateful across packets;
// sharing one decoder between peers would interleave their states and
// corrupt every output.
function ensurePeerSlot(peerHex) {
  let slot = peers.get(peerHex);
  if (slot) return slot;
  const w = new AudioWorkletNode(ctx, "voice-playback");
  const g = ctx.createGain();
  g.gain.value = 1.0;
  w.connect(g).connect(ctx.destination);
  // PlaybackCodecCtor is set during startCapture's async codec module
  // load. If a frame arrives before startCapture completed (race during
  // a fast reconnect, say) we'd be left without a decoder; guard with
  // a defensive null check so the audio path silently drops the frame
  // rather than throwing.
  const codec = PlaybackCodecCtor
    ? new PlaybackCodecCtor({
        onPcm: (pcm) => {
          // Transfer the buffer to the worklet to avoid a copy.
          w.port.postMessage(pcm, [pcm.buffer]);
        },
      })
    : null;
  slot = { worklet: w, gain: g, codec };
  peers.set(peerHex, slot);
  return slot;
}

export function deliverFrame(peerHex, payload, codecId) {
  if (!ctx) return;
  const slot = ensurePeerSlot(peerHex);
  if (!slot.codec) return; // codec module not yet loaded (race) — drop
  slot.codec.decode(payload, codecId);
}

export function dropPeer(peerHex) {
  const slot = peers.get(peerHex);
  if (!slot) return;
  try {
    slot.worklet.disconnect();
    slot.gain.disconnect();
  } catch (_e) {
    // ignore disconnect errors
  }
  if (slot.codec) slot.codec.stop();
  peers.delete(peerHex);
}

export function setPeerVolume(peerHex, gain) {
  const slot = peers.get(peerHex);
  if (!slot) return;
  slot.gain.gain.value = Math.max(0, Math.min(2.0, gain));
}

export function getPeerGain(peerHex) {
  const slot = peers.get(peerHex);
  return slot ? slot.gain.gain.value : null;
}

// --- Gleam UI helpers (used by voice.gleam FFI bindings) ---

function uint8ToHex(a) {
  return Array.from(a)
    .map((b) => b.toString(16).padStart(2, "0"))
    .join("");
}

// wasmVoiceStart uses the callback pattern (matching the rest of the Gleam
// bridge) so Gleam doesn't need a `gleam_javascript` dependency for Promises.
// `callback` receives `Ok(null)` or `Error(message)` as a Gleam Result.
export function wasmVoiceStart(client, roomHandle, callback) {
  startCapture(client)
    .then(() => {
      try {
        client.voice_start(
          roomHandle,
          (peerId, payload, codecId) => {
            const hex = uint8ToHex(new Uint8Array(peerId));
            // payload is a wasm-bindgen Uint8Array view; copy into a
            // detached Uint8Array because the buffer may be reused.
            const copy = new Uint8Array(payload.length);
            copy.set(payload);
            deliverFrame(hex, copy, codecId);
          },
          (peerId) => {
            const hex = uint8ToHex(new Uint8Array(peerId));
            dropPeer(hex);
          },
          (peerId, inCall, talking, isMuted) => {
            const hex = uint8ToHex(new Uint8Array(peerId));
            if (window.__voicePeerStateHandler) {
              window.__voicePeerStateHandler(hex, inCall, talking, isMuted);
            }
          },
        );
        callback(new Ok(null));
      } catch (e) {
        stopCapture();
        callback(new GError(String(e?.message || e)));
      }
    })
    .catch((e) => {
      callback(new GError(String(e?.message || e)));
    });
}

export function wasmVoiceStop(client) {
  try {
    client.voice_stop();
  } catch (_e) {
    // ignore stop errors
  }
  stopCapture();
}

export function wasmVoiceSetMuted(client, m) {
  try {
    client.voice_set_muted(!!m);
  } catch (e) {
    console.warn("voice_set_muted failed", e);
  }
}

export function wasmVoiceSetDeafened(client, d) {
  try {
    client.voice_set_deafened(!!d);
  } catch (e) {
    console.warn("voice_set_deafened failed", e);
  }
}

export function installVoiceStateHandler(cb) {
  window.__voicePeerStateHandler = cb;
}
