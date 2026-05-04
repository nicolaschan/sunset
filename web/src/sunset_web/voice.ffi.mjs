// JS-side voice wiring. Owns:
// - the AudioContext (created lazily on first start)
// - per-peer { workletNode, gainNode } table
// - capture worklet stream
// - GainNode value updates from voice_set_peer_volume

import { Ok, Error as GError } from "../../prelude.mjs";

let ctx = null;
const peers = new Map(); // peerHex -> { worklet, gain }
let captureNode = null;
let captureStream = null;

// Test-only handle. The bundled module is otherwise unreachable from
// page.evaluate() because Lustre's prod build inlines it into sunset_web.js
// (no `/javascript/...` URL to dynamic-import). With `window.SUNSET_TEST`
// set we expose `setPeerVolume` and `getPeerGain` on `window.__voiceFfi`
// so Playwright specs can drive and inspect per-peer GainNodes the same
// way the popover's mute-for-me toggle does. No-op in production.
if (typeof window !== "undefined" && window.SUNSET_TEST) {
  window.__voiceFfi = {
    setPeerVolume: (peerHex, gain) => setPeerVolume(peerHex, gain),
    getPeerGain: (peerHex) => getPeerGain(peerHex),
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
  captureNode.port.onmessage = (e) => {
    if (e.data instanceof Float32Array && e.data.length === 960) {
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
          (_peerId, _gain) => {
            // on_set_peer_volume: JS-side GainNode is managed by deliverFrame/setPeerVolume
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
