// JS-side voice wiring. Owns:
// - the AudioContext (created lazily on first start)
// - per-peer { workletNode, gainNode } table
// - capture worklet stream
// - per-peer GainNode updates (setPeerVolume), called directly from
//   the Gleam UI; not threaded through Rust (the GainNode is a
//   browser-shaped concept — see voice/mod.rs for the spec rationale).

import { Ok, Error as GError } from "../../prelude.mjs";

let ctx = null;
const peers = new Map(); // peerHex -> { worklet, gain }
let captureNode = null;
let captureStream = null;

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
  };
}

export function ensureCtx() {
  if (!ctx) ctx = new AudioContext({ sampleRate: 48000 });
  return ctx;
}

// Frame size constants must agree with the Rust constants in
// `crates/sunset-voice/src/lib.rs`. The capture worklet always
// posts interleaved L/R stereo (PER_CHANNEL × 2) regardless of the
// active send-side quality preset; the Rust runtime downmixes when
// the encoder is mono. The playback worklet always receives the
// same interleaved stereo shape because the decoder is fixed at
// 2-channel.
const FRAME_SAMPLES_PER_CHANNEL = 960;
const STEREO_FRAME_TOTAL = FRAME_SAMPLES_PER_CHANNEL * 2;

export async function startCapture(client) {
  ensureCtx();
  await ctx.audioWorklet.addModule("/audio/voice-capture-worklet.js");
  await ctx.audioWorklet.addModule("/audio/voice-playback-worklet.js");
  captureStream = await navigator.mediaDevices.getUserMedia({
    audio: {
      echoCancellation: true,
      noiseSuppression: true,
      autoGainControl: true,
      // Always request stereo capture. If the platform only has a
      // mono mic the worklet duplicates L into R so we get the same
      // interleaved 1920-sample frame shape regardless. See
      // `voice-capture-worklet.js` for the rationale on not
      // reconfiguring channelCount per quality preset.
      channelCount: 2,
    },
  });
  const src = ctx.createMediaStreamSource(captureStream);
  captureNode = new AudioWorkletNode(ctx, "voice-capture", {
    // The capture worklet reads from a stereo source; tell the audio
    // graph not to downmix to mono before the worklet sees it.
    channelCount: 2,
    channelCountMode: "explicit",
    channelInterpretation: "speakers",
  });
  captureNode.port.onmessage = (e) => {
    if (e.data instanceof Float32Array && e.data.length === STEREO_FRAME_TOTAL) {
      try {
        client.voice_input(e.data);
      } catch (err) {
        console.warn("voice_input failed", err);
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
  for (const [_peer, slot] of peers) {
    try {
      slot.worklet.disconnect();
      slot.gain.disconnect();
    } catch (_e) {
      // ignore disconnect errors
    }
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

export function deliverFrame(peerHex, pcm) {
  if (!ctx) return;
  let slot = peers.get(peerHex);
  if (!slot) {
    const w = new AudioWorkletNode(ctx, "voice-playback", {
      // Decoder always emits stereo; configure the worklet's output
      // node accordingly so the audio graph routes L/R to the
      // appropriate destination channels rather than downmixing.
      outputChannelCount: [2],
    });
    const g = ctx.createGain();
    g.gain.value = 1.0;
    w.connect(g).connect(ctx.destination);
    slot = { worklet: w, gain: g };
    peers.set(peerHex, slot);
  }
  slot.worklet.port.postMessage(pcm, [pcm.buffer]);
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
          (peerId, pcm) => {
            const hex = uint8ToHex(new Uint8Array(peerId));
            deliverFrame(hex, new Float32Array(pcm));
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
        // Re-apply the user's preferred quality preset. localStorage
        // is the canonical persistence; default `"maximum"` matches
        // the Rust default. Failures are non-fatal — the encoder
        // already constructed itself with the default.
        try {
          const stored = window.localStorage?.getItem("sunset/voice-quality");
          const label =
            stored === "voice" || stored === "high" || stored === "maximum"
              ? stored
              : "maximum";
          client.voice_set_quality(label);
        } catch (qe) {
          console.warn("voice_set_quality on start failed", qe);
        }
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

// Persists the new quality preset to localStorage and pushes it
// down to the active encoder if voice is started. The setting is
// re-applied on every voice_start so a user who changes it before
// joining a call sees the right preset on rejoin.
export function wasmVoiceSetQuality(client, label) {
  try {
    window.localStorage?.setItem("sunset/voice-quality", label);
  } catch (_e) {
    // Private browsing or full quota — non-fatal.
  }
  try {
    client.voice_set_quality(label);
  } catch (e) {
    // "voice not started" is fine — the value persists in
    // localStorage and applies on the next start.
    if (!String(e?.message || e).includes("voice not started")) {
      console.warn("voice_set_quality failed", e);
    }
  }
}

// Read the persisted preset (or the Rust default if nothing saved).
export function wasmVoiceGetQuality() {
  try {
    const stored = window.localStorage?.getItem("sunset/voice-quality");
    if (stored === "voice" || stored === "high" || stored === "maximum") {
      return stored;
    }
  } catch (_e) {
    // ignore
  }
  return "maximum";
}

export function installVoiceStateHandler(cb) {
  window.__voicePeerStateHandler = cb;
}
