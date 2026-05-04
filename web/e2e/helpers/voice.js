// Shared voice test fixtures for Phase 5 Playwright specs (Tasks 28-33).
//
// Exports:
//   spawnRelay()          — spawn a sunset-relay subprocess and capture its
//                           listen address from the banner; returns { proc, dir, addr }
//   teardownRelay(state)  — SIGTERM the relay and delete the temp data dir
//   freshSeedHex()        — 64-char hex seed from Math.random (one per peer/test)
//   syntheticPcm(counter) — Float32Array(960) matching Rust synth_pcm_with_counter
//   decodeCounter(val)    — inverse of syntheticPcm: recover counter from pcm[0]
//   pcmChecksum(samples)  — SHA-256 of f32 LE bytes (matches Rust recorder)
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
import { createHash } from "crypto";

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
 * Build a synthetic PCM frame that matches Rust `synth_pcm_with_counter(counter)`:
 *   pcm[0] = counter / 1_000_000.0
 *   pcm[i] = sin((counter + i) / 1_000_000.0)  for i >= 1
 *
 * @param {number} counter  Non-negative integer counter value.
 * @returns {Float32Array}  960-sample frame.
 */
export function syntheticPcm(counter) {
  const pcm = new Float32Array(960);
  pcm[0] = counter / 1_000_000;
  for (let i = 1; i < 960; i++) {
    pcm[i] = Math.sin((counter + i) / 1_000_000);
  }
  return pcm;
}

/**
 * Recover the counter value embedded in the first sample of a synthetic frame.
 * Inverse of `syntheticPcm`.
 *
 * @param {number} firstSampleVal  `pcm[0]` from a received frame.
 * @returns {number}  The original counter integer.
 */
export function decodeCounter(firstSampleVal) {
  return Math.round(firstSampleVal * 1_000_000);
}

/**
 * Compute SHA-256 of PCM samples encoded as little-endian f32 bytes.
 * Matches the Rust recorder checksum so tests can assert byte-equal delivery.
 *
 * @param {Float32Array} samples
 * @returns {string}  Lowercase hex digest.
 */
export function pcmChecksum(samples) {
  const buf = Buffer.from(samples.buffer, samples.byteOffset, samples.byteLength);
  return createHash("sha256").update(buf).digest("hex");
}

// ---------------------------------------------------------------------------
// Voice readiness helper
// ---------------------------------------------------------------------------

/**
 * Wait until the WASM voice session is fully started so test-hook methods
 * like `voice_install_frame_recorder` and `voice_inject_pcm` can be called
 * without throwing "voice not started".
 *
 * The WASM `voice_start()` is invoked asynchronously (getUserMedia → worklet
 * addModule → voice_start) after the UI dispatches JoinVoice. The minibar
 * appearing only means the UI has dispatched the intent; `voice_start()` on
 * the WASM side may still be in flight. This helper polls `voice_active_peers`
 * (which throws "voice not started" until voice is ready) and returns once
 * it succeeds.
 *
 * @param {import("@playwright/test").Page} page
 * @param {number} [timeoutMs=5000]
 */
export async function waitForVoiceReady(page, timeoutMs = 5000) {
  await page.waitForFunction(
    () => {
      try {
        // voice_active_peers() throws "voice not started" until voice_start
        // completes on the WASM side; once it returns (even an empty array),
        // the voice session is up and test-hook methods are safe to call.
        const peers = window.sunsetClient.voice_active_peers();
        return Array.isArray(peers);
      } catch (_) {
        return false;
      }
    },
    null,
    { timeout: timeoutMs },
  );
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
