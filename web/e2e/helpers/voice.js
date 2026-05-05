// Shared voice test fixtures for Phase 5 Playwright specs.
//
// Exports:
//   spawnRelay()          — spawn a sunset-relay subprocess and capture its
//                           listen address from the banner; returns { proc, dir, addr }
//   teardownRelay(state)  — SIGTERM the relay and delete the temp data dir
//   freshSeedHex()        — 64-char hex seed from Math.random (one per peer/test)
//   syntheticPcm(counter) — Float32Array(960) of continuous 440 Hz sine
//                           (counter advances phase by one frame). Matches
//                           Rust `synth_pcm_with_counter` so JS-side and
//                           WASM-side fixtures are byte-equal pre-encode.
//   pcmRms(samples)       — RMS amplitude of a Float32Array (real audio
//                           lands ~0.35 on a 0.5-amplitude sine; silence ≈ 0).
//
// GainNode test affordance
//   installVoiceFfi(page) — call after page.goto() to expose window.__voiceFfi
//                           = { getPeerGain(peerHex) }. Tests that need to
//                           assert per-peer mute state should call this helper
//                           in their beforeEach / test setup.

import { spawn } from "child_process";
import { mkdtempSync, rmSync, writeFileSync } from "fs";
import { tmpdir } from "os";
import { join } from "path";

// ---------------------------------------------------------------------------
// Relay lifecycle
// ---------------------------------------------------------------------------

/**
 * Spawn a sunset-relay process with a fresh temp data dir and a random port.
 * Waits up to 15 s for the address banner then resolves with
 * `{ proc, dir, addr }` where `addr` is e.g. `"ws://127.0.0.1:12345"`.
 *
 * Pattern extracted from voice_protocol.spec.js beforeAll (lines 23-71).
 */
export async function spawnRelay() {
  const dir = mkdtempSync(join(tmpdir(), "sunset-relay-voice-"));
  const cfg = join(dir, "relay.toml");
  writeFileSync(
    cfg,
    [
      `listen_addr = "127.0.0.1:0"`,
      `data_dir = "${dir}"`,
      `interest_filter = "all"`,
      `identity_secret = "auto"`,
      `peers = []`,
      "",
    ].join("\n"),
  );

  const proc = spawn("sunset-relay", ["--config", cfg], {
    stdio: ["ignore", "pipe", "pipe"],
  });

  const addr = await new Promise((res, rej) => {
    const t = setTimeout(
      () => rej(new Error("relay didn't print address banner within 15s")),
      15_000,
    );
    let buf = "";
    proc.stdout.on("data", (c) => {
      buf += c.toString();
      const m = buf.match(/address:\s+(ws:\/\/[^\s]+)/);
      if (m) {
        clearTimeout(t);
        res(m[1]);
      }
    });
    proc.stderr.on("data", (c) => process.stderr.write(`[relay] ${c}`));
    proc.on("error", (e) => {
      clearTimeout(t);
      rej(e);
    });
    proc.on("exit", (code) => {
      if (code !== null && code !== 0) {
        clearTimeout(t);
        rej(new Error(`relay exited prematurely with code ${code}`));
      }
    });
  });

  return { proc, dir, addr };
}

/**
 * Kill the relay process (SIGTERM) and remove the temp data directory.
 * Safe to call multiple times; both checks are guarded.
 *
 * @param {{ proc: import("child_process").ChildProcess, dir: string }} state
 */
export function teardownRelay(state) {
  if (state?.proc?.exitCode === null) state.proc.kill("SIGTERM");
  if (state?.dir) rmSync(state.dir, { recursive: true, force: true });
}

// ---------------------------------------------------------------------------
// Identity helpers
// ---------------------------------------------------------------------------

/**
 * Generate a 64-hex-char seed (32 bytes) for a fresh peer identity.
 * Each test should use a fresh seed per peer so stale CRDT signaling entries
 * from previous tests don't pollute the relay's state for the next test.
 *
 * @returns {string}
 */
export function freshSeedHex() {
  // Math.random is fine for test fixtures — not security-critical.
  let s = "";
  for (let i = 0; i < 64; i++) s += Math.floor(Math.random() * 16).toString(16);
  return s;
}

// ---------------------------------------------------------------------------
// Synthetic PCM helpers
// ---------------------------------------------------------------------------

/**
 * Build one 20 ms PCM frame of continuous 440 Hz sine at amplitude 0.5.
 * `counter` advances the phase by exactly one frame so consecutive
 * `syntheticPcm(c)` outputs are continuous (no clicks). This matches the
 * Rust `synth_pcm_with_counter` and is what an Opus encoder is built to
 * compress + decode faithfully — pre-Opus we packed a counter into
 * `pcm[0]`, but Opus is lossy and individual sample values do not
 * survive, so we identify frames by their per-peer ordering and
 * checksum-distinctness in the recorder instead.
 *
 * @param {number} counter  Frame index (any integer; controls phase only).
 * @returns {Float32Array}  960-sample frame at 48 kHz.
 */
export function syntheticPcm(counter) {
  const FREQ_HZ = 440;
  const SR = 48_000;
  const FRAME = 960;
  const pcm = new Float32Array(FRAME);
  const offset = counter * FRAME;
  for (let i = 0; i < FRAME; i++) {
    const t = (offset + i) / SR;
    pcm[i] = 0.5 * Math.sin(2 * Math.PI * FREQ_HZ * t);
  }
  return pcm;
}

/**
 * RMS amplitude of a Float32Array. Used to distinguish "real audio
 * delivered" (≥ ~0.1 for a 0.5-amplitude sine through Opus) from
 * "silence padding / underrun" (≈ 0).
 *
 * @param {Float32Array} samples
 * @returns {number}
 */
export function pcmRms(samples) {
  if (samples.length === 0) return 0;
  let sumSq = 0;
  for (let i = 0; i < samples.length; i++) sumSq += samples[i] * samples[i];
  return Math.sqrt(sumSq / samples.length);
}

// ---------------------------------------------------------------------------
// GainNode test affordance
// ---------------------------------------------------------------------------

/**
 * No-op kept for backward compatibility with specs written before
 * voice.ffi.mjs grew its own SUNSET_TEST-gated `window.__voiceFfi`
 * handle. The module attaches `{ setPeerVolume, getPeerGain }` itself
 * when `window.SUNSET_TEST` is set before page load, so no init script
 * is needed here.
 *
 * Usage in a spec (callers that set `window.SUNSET_TEST = true` via
 * `addInitScript`, as the rest of these specs do):
 *
 *   const gain = await page.evaluate(hex => window.__voiceFfi.getPeerGain(hex), peerHex);
 *   expect(gain).toBe(0);  // muted-for-me
 *
 * @param {import("@playwright/test").Page} _page
 */
export async function installVoiceFfi(_page) {
  // Intentionally empty. See voice.ffi.mjs for the SUNSET_TEST handle.
}
