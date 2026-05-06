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

// Per-peer audio-level state. RMS of each delivered PCM frame is fed
// into a single-pole EMA so the rendered bar doesn't visibly twitch on
// every 20 ms frame, then dispatched to the Gleam UI on a fixed cadence
// (see LEVEL_DISPATCH_INTERVAL_MS). Reset on stopCapture / dropPeer so
// stale levels don't linger in the voice rail.
const LEVEL_EMA_ALPHA = 0.35;
const LEVEL_DISPATCH_INTERVAL_MS = 80;
// Speech RMS sits in 0.05–0.2 territory; multiply so realistic speech
// reaches ~1.0 on the bar without clipping for louder bursts.
const LEVEL_RMS_GAIN = 4.0;
const peerLevelEma = new Map(); // peerHex -> last EMA value (0..1)
const peerLevelLastDispatchMs = new Map(); // peerHex -> ms
let selfLevelEma = 0;
let selfLevelLastDispatchMs = 0;

// Test-only handle. The bundled module is otherwise unreachable from
// page.evaluate() because Lustre's prod build inlines it into sunset_web.js
// (no `/javascript/...` URL to dynamic-import). With `window.SUNSET_TEST`
// set we expose `setPeerVolume` / `getPeerGain` for per-peer GainNode
// drive + inspect, plus `stopCaptureSource` which detaches the mic
// capture worklet from the live MediaStream so deterministic
// `voice_inject_pcm` tests aren't polluted by fake-mic noise. No-op
// in production. `getPeerLevel` exposes the latest smoothed playback
// level for a peer so e2e specs can assert the "who is talking"
// waveform actually reflects audio (not just a fixture animation).
if (typeof window !== "undefined" && window.SUNSET_TEST) {
  window.__voiceFfi = {
    setPeerVolume: (peerHex, gain) => setPeerVolume(peerHex, gain),
    getPeerGain: (peerHex) => getPeerGain(peerHex),
    getPeerLevel: (peerHex) => peerLevelEma.get(peerHex) ?? 0,
    getSelfLevel: () => selfLevelEma,
    stopCaptureSource: () => stopCaptureSource(),
  };
}

function computeRms(pcm) {
  if (!pcm || pcm.length === 0) return 0;
  let sum = 0;
  for (let i = 0; i < pcm.length; i++) sum += pcm[i] * pcm[i];
  return Math.sqrt(sum / pcm.length);
}

function clamp01(x) {
  return x < 0 ? 0 : x > 1 ? 1 : x;
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
  captureNode.port.onmessage = (e) => {
    if (e.data instanceof Float32Array && e.data.length === 960) {
      try {
        client.voice_input(e.data);
      } catch (err) {
        console.warn("voice_input failed", err);
      }
      // Smooth and dispatch self mic level so the local row's waveform
      // reflects what the user is actually saying, not a fixture
      // animation. Runs on every captured 20 ms frame regardless of
      // mute state — muting filters the outgoing audio downstream but
      // the user still expects to see their own meter move.
      updateSelfLevel(e.data);
    }
  };
  src.connect(captureNode);
}

function updateSelfLevel(pcm) {
  const rms = computeRms(pcm);
  const normalized = clamp01(rms * LEVEL_RMS_GAIN);
  selfLevelEma = LEVEL_EMA_ALPHA * normalized + (1 - LEVEL_EMA_ALPHA) * selfLevelEma;
  const now = Date.now();
  if (now - selfLevelLastDispatchMs >= LEVEL_DISPATCH_INTERVAL_MS) {
    selfLevelLastDispatchMs = now;
    if (window.__voiceSelfLevelHandler) {
      try {
        window.__voiceSelfLevelHandler(selfLevelEma);
      } catch (err) {
        console.warn("self level handler failed", err);
      }
    }
  }
}

function updatePeerLevel(peerHex, pcm) {
  const rms = computeRms(pcm);
  const normalized = clamp01(rms * LEVEL_RMS_GAIN);
  const prev = peerLevelEma.get(peerHex) ?? 0;
  const next = LEVEL_EMA_ALPHA * normalized + (1 - LEVEL_EMA_ALPHA) * prev;
  peerLevelEma.set(peerHex, next);
  const now = Date.now();
  const last = peerLevelLastDispatchMs.get(peerHex) ?? 0;
  if (now - last >= LEVEL_DISPATCH_INTERVAL_MS) {
    peerLevelLastDispatchMs.set(peerHex, now);
    if (window.__voicePeerLevelHandler) {
      try {
        window.__voicePeerLevelHandler(peerHex, next);
      } catch (err) {
        console.warn("peer level handler failed", err);
      }
    }
  }
}

function flushPeerLevelToZero(peerHex) {
  peerLevelEma.set(peerHex, 0);
  peerLevelLastDispatchMs.set(peerHex, 0);
  if (window.__voicePeerLevelHandler) {
    try {
      window.__voicePeerLevelHandler(peerHex, 0);
    } catch (err) {
      console.warn("peer level handler failed", err);
    }
  }
}

function flushSelfLevelToZero() {
  selfLevelEma = 0;
  selfLevelLastDispatchMs = 0;
  if (window.__voiceSelfLevelHandler) {
    try {
      window.__voiceSelfLevelHandler(0);
    } catch (err) {
      console.warn("self level handler failed", err);
    }
  }
}

export function stopCapture() {
  if (captureStream) {
    for (const t of captureStream.getTracks()) t.stop();
    captureStream = null;
  }
  captureNode = null;
  for (const [peerHex, slot] of peers) {
    try {
      slot.worklet.disconnect();
      slot.gain.disconnect();
    } catch (_e) {
      // ignore disconnect errors
    }
    flushPeerLevelToZero(peerHex);
  }
  peers.clear();
  peerLevelEma.clear();
  peerLevelLastDispatchMs.clear();
  flushSelfLevelToZero();
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
    const w = new AudioWorkletNode(ctx, "voice-playback");
    const g = ctx.createGain();
    g.gain.value = 1.0;
    w.connect(g).connect(ctx.destination);
    slot = { worklet: w, gain: g };
    peers.set(peerHex, slot);
  }
  // Compute the peer's playback level *before* transferring the buffer
  // — postMessage with the [pcm.buffer] transfer list neuters the
  // Float32Array on this side, so any later reads would see length 0.
  updatePeerLevel(peerHex, pcm);
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
  flushPeerLevelToZero(peerHex);
  peerLevelEma.delete(peerHex);
  peerLevelLastDispatchMs.delete(peerHex);
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

// Smoothed playback-level updates per peer (0..1, normalised so realistic
// speech reaches ~1.0 without clipping). Fires at LEVEL_DISPATCH_INTERVAL_MS
// cadence so the rail's waveform drives off real audio energy without
// stalling Lustre on every 20 ms PCM frame.
export function installVoicePeerLevelHandler(cb) {
  window.__voicePeerLevelHandler = cb;
}

// Smoothed local mic-level updates (0..1). Drives the self row's
// waveform so the local user can see their own meter respond to their
// voice.
export function installVoiceSelfLevelHandler(cb) {
  window.__voiceSelfLevelHandler = cb;
}
