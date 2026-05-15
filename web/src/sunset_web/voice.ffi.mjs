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
// How long the level meter waits past the last delivered frame before
// it starts decaying toward zero on a timer. Frames arrive every
// ~20 ms when audio is flowing, so a small grace window prevents the
// decay tick from racing real audio. Past this, the periodic decay
// runs even with zero packets arriving — matches user intuition that
// "no audio = level drops".
const LEVEL_DECAY_AFTER_MS = 30;
// Decay tick interval. 20 ms matches the natural frame cadence; with
// alpha=0.35 the EMA halves roughly every two ticks (~40 ms) and
// reaches sub-0.05 from a 0.5 peak in ~150 ms, which is below the
// 3-second budget the level-meter e2e test asserts.
const LEVEL_DECAY_TICK_MS = 20;
const peerLevelEma = new Map(); // peerHex -> last EMA value (0..1)
const peerLevelLastDispatchMs = new Map(); // peerHex -> ms
const peerLastFrameMs = new Map(); // peerHex -> ms of last delivered frame
let selfLevelEma = 0;
let selfLevelLastDispatchMs = 0;
let levelDecayTimer = null;

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
  startLevelDecayTimer();
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
  peerLastFrameMs.set(peerHex, Date.now());
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

// Periodic decay tick. Without the old Rust-side jitter pump, the
// peer level EMA stops receiving updates the instant frames stop
// arriving — which would leave the meter stuck at the last value
// instead of dropping when the peer falls silent. This ticker
// applies the same single-pole decay (toward zero) the EMA used to
// get from silence-padded frames at the pump cadence.
function tickPeerLevelDecay() {
  const now = Date.now();
  for (const [hex, prev] of peerLevelEma) {
    const lastFrame = peerLastFrameMs.get(hex) ?? 0;
    if (now - lastFrame < LEVEL_DECAY_AFTER_MS) continue;
    if (prev < 0.001) continue; // already effectively zero
    const next = (1 - LEVEL_EMA_ALPHA) * prev;
    peerLevelEma.set(hex, next);
    const lastDispatch = peerLevelLastDispatchMs.get(hex) ?? 0;
    if (now - lastDispatch >= LEVEL_DISPATCH_INTERVAL_MS) {
      peerLevelLastDispatchMs.set(hex, now);
      if (window.__voicePeerLevelHandler) {
        try {
          window.__voicePeerLevelHandler(hex, next);
        } catch (err) {
          console.warn("peer level handler failed", err);
        }
      }
    }
  }
}

function startLevelDecayTimer() {
  if (levelDecayTimer !== null) return;
  levelDecayTimer = setInterval(tickPeerLevelDecay, LEVEL_DECAY_TICK_MS);
}

function stopLevelDecayTimer() {
  if (levelDecayTimer !== null) {
    clearInterval(levelDecayTimer);
    levelDecayTimer = null;
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
  peerLastFrameMs.clear();
  stopLevelDecayTimer();
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

export function deliverFrame(peerHex, seq, pcm) {
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
  // Compute the peer's playback level *before* transferring the buffer
  // — postMessage with the [pcm.buffer] transfer list neuters the
  // Float32Array on this side, so any later reads would see length 0.
  updatePeerLevel(peerHex, pcm);
  // The worklet maintains a sequence-indexed jitter buffer; pass seq
  // so it can detect gaps and absorb reordering. The buffer is
  // transferred to avoid a copy.
  slot.worklet.port.postMessage({ seq, pcm }, [pcm.buffer]);
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
  peerLastFrameMs.delete(peerHex);
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
          (peerId, seq, pcm) => {
            const hex = uint8ToHex(new Uint8Array(peerId));
            deliverFrame(hex, seq >>> 0, new Float32Array(pcm));
          },
          (peerId) => {
            const hex = uint8ToHex(new Uint8Array(peerId));
            dropPeer(hex);
          },
          (peerId, inCall, talking, isMuted, inVoiceChannel) => {
            const hex = uint8ToHex(new Uint8Array(peerId));
            if (window.__voicePeerStateHandler) {
              window.__voicePeerStateHandler(
                hex,
                inCall,
                talking,
                isMuted,
                inVoiceChannel,
              );
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

// Start voice in *observer* mode — no mic permission, no audio
// context, no outbound voice traffic. Just enough to subscribe to
// the durable voice-presence stream so the channels rail can
// render who is in the voice channel before the local user joins.
// Mirrors `wasmVoiceStart`'s callback shape so the Rust side wires
// the same three handlers (PCM delivery, peer-drop, peer-state-changed)
// — none of which can fire until `wasmVoiceActivate` opens the gate.
export function wasmVoiceObserveStart(client, roomHandle, callback) {
  try {
    client.voice_observe_start(
      roomHandle,
      (peerId, seq, pcm) => {
        const hex = uint8ToHex(new Uint8Array(peerId));
        deliverFrame(hex, seq >>> 0, new Float32Array(pcm));
      },
      (peerId) => {
        const hex = uint8ToHex(new Uint8Array(peerId));
        dropPeer(hex);
      },
      (peerId, inCall, talking, isMuted, inVoiceChannel) => {
        const hex = uint8ToHex(new Uint8Array(peerId));
        if (window.__voicePeerStateHandler) {
          window.__voicePeerStateHandler(
            hex,
            inCall,
            talking,
            isMuted,
            inVoiceChannel,
          );
        }
      },
    );
    callback(new Ok(null));
  } catch (e) {
    callback(new GError(String(e?.message || e)));
  }
}

// Bring up mic capture, then flip the runtime out of observer mode
// so heartbeats / presence publishes / auto-connect resume. Pairs
// with `wasmVoiceObserveStart`. The user-quality preset is re-applied
// here (rather than in observe-start) because it's a property of the
// active encoder, not the observer.
export function wasmVoiceActivate(client, callback) {
  startCapture(client)
    .then(() => {
      try {
        client.voice_activate();
        try {
          const stored = window.localStorage?.getItem("sunset/voice-quality");
          const label =
            stored === "voice" || stored === "high" || stored === "maximum"
              ? stored
              : "maximum";
          client.voice_set_quality(label);
        } catch (qe) {
          console.warn("voice_set_quality on activate failed", qe);
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

// Inverse of `wasmVoiceActivate`. Returns to observer mode so the
// roster stays populated for the local user; stops mic capture so
// no audio leaves the device. Does *not* drop the runtime — use
// `wasmVoiceStop` for that (on room exit).
export function wasmVoiceDeactivate(client) {
  try {
    client.voice_deactivate();
  } catch (e) {
    console.warn("voice_deactivate failed", e);
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

export function wasmVoiceSetDenoise(client, on) {
  try {
    client.voice_set_denoise(!!on);
  } catch (e) {
    console.warn("voice_set_denoise failed", e);
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
