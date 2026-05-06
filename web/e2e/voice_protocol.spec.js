// Protocol regression: Opus voice frames flow Alice → Bob.
//
// Spawns a real sunset-relay, two Chromium pages each load
// /voice-e2e-test.html, both join the same room, both call
// startVoice, Alice injects a continuous 440 Hz sine via
// `injectPcm`, and asserts Bob's recorder eventually accumulates a
// frame attributed to Alice with non-trivial RMS (i.e. real decoded
// audio, not silence / underrun padding).
//
// Opus is lossy: pre-codec this spec asserted byte-equal SHA-256 of
// the f32 PCM Alice sent; post-codec we can't assert sample-level
// fidelity any more (Opus does not preserve individual sample
// values). The contract that matters to a real user is "frames get
// through and contain real audio energy", which is what this spec
// now checks.
//
// Auto-connect (VoiceRuntime auto_connect task) establishes the
// WebRTC P2P connection; no manual connectDirect call is needed.

import { test, expect } from "@playwright/test";
import { spawn } from "child_process";
import { mkdtempSync, rmSync } from "fs";
import { tmpdir } from "os";
import { join } from "path";

let relayProcess = null;
let relayAddress = null;
let relayDataDir = null;

test.beforeAll(async () => {
  relayDataDir = mkdtempSync(join(tmpdir(), "sunset-relay-voice-"));
  const configPath = join(relayDataDir, "relay.toml");
  const fs = await import("fs/promises");
  await fs.writeFile(
    configPath,
    [
      `listen_addr = "127.0.0.1:0"`,
      `data_dir = "${relayDataDir}"`,
      `interest_filter = "all"`,
      `identity_secret = "auto"`,
      `peers = []`,
      "",
    ].join("\n"),
  );

  relayProcess = spawn("sunset-relay", ["--config", configPath], {
    stdio: ["ignore", "pipe", "pipe"],
  });

  relayAddress = await new Promise((resolve, reject) => {
    const timer = setTimeout(
      () => reject(new Error("relay didn't print address banner within 15s")),
      15_000,
    );
    let buffer = "";
    relayProcess.stdout.on("data", (chunk) => {
      buffer += chunk.toString();
      const m = buffer.match(/address:\s+(ws:\/\/[^\s]+)/);
      if (m) {
        clearTimeout(timer);
        resolve(m[1]);
      }
    });
    relayProcess.stderr.on("data", (chunk) => {
      process.stderr.write(`[relay] ${chunk}`);
    });
    relayProcess.on("error", (e) => {
      clearTimeout(timer);
      reject(e);
    });
    relayProcess.on("exit", (code) => {
      if (code !== null && code !== 0) {
        clearTimeout(timer);
        reject(new Error(`relay exited prematurely with code ${code}`));
      }
    });
  });
});

test.afterAll(async () => {
  if (relayProcess && relayProcess.exitCode === null) {
    relayProcess.kill("SIGTERM");
  }
  if (relayDataDir) {
    rmSync(relayDataDir, { recursive: true, force: true });
  }
});

// Each test gets a fresh identity per peer. Reusing seeds across tests
// would mean reusing identities — but the relay persists CRDT entries
// keyed by identity (subscriptions, WebRTC SDP signaling), so test N+1
// would inherit stale signaling entries from test N and intermittently
// wedge the WebRTC handshake. Real production users have one identity
// per browser session; this matches that model.
function freshSeedHex() {
  // 64 hex chars = 32 bytes. Math.random is fine for test fixtures.
  let s = "";
  for (let i = 0; i < 64; i++) s += Math.floor(Math.random() * 16).toString(16);
  return s;
}

const ROOM = "voice-test-room";

// One 20 ms PCM frame of continuous 440 Hz sine at amplitude 0.5.
// `counter` advances the phase by exactly one frame so consecutive
// counters produce a continuous tone (which Opus is built to encode
// efficiently and reproduce faithfully). Matches the Rust
// `synth_pcm_with_counter`.
function syntheticPcm(counter) {
  const FREQ_HZ = 440;
  const SR = 48000;
  const FRAME = 960;
  const arr = new Float32Array(FRAME);
  const offset = counter * FRAME;
  for (let i = 0; i < FRAME; i++) {
    const t = (offset + i) / SR;
    arr[i] = 0.5 * Math.sin(2 * Math.PI * FREQ_HZ * t);
  }
  return arr;
}

test("alice's opus frames arrive at bob with real audio energy", async ({ browser }) => {
  const aliceCtx = await browser.newContext();
  const bobCtx = await browser.newContext();
  const alice = await aliceCtx.newPage();
  const bob = await bobCtx.newPage();
  for (const [name, page] of [["A", alice], ["B", bob]]) {
    page.on("pageerror", (err) =>
      process.stderr.write(`[${name} pageerror] ${err.stack || err}\n`),
    );
    page.on("console", (msg) => {
      if (msg.type() === "error" || msg.type() === "warn") {
        process.stderr.write(`[${name} console.${msg.type()}] ${msg.text()}\n`);
      }
    });
  }
  await alice.goto("/voice-e2e-test.html");
  await bob.goto("/voice-e2e-test.html");

  const aliceInfo = await alice.evaluate(
    async ({ seed, room, relay }) => window.__voice.start({ seed, room, relay }),
    { seed: freshSeedHex(), room: ROOM, relay: relayAddress },
  );
  await bob.evaluate(
    async ({ seed, room, relay }) => window.__voice.start({ seed, room, relay }),
    { seed: freshSeedHex(), room: ROOM, relay: relayAddress },
  );

  const alicePk = aliceInfo.publicKey;

  // Start presence on both so the voice runtime can discover peers and
  // the auto-connect task can dial WebRTC P2P.
  await alice.evaluate(async () => await window.__voice.startPresence());
  await bob.evaluate(async () => await window.__voice.startPresence());

  // startVoice on both. The auto_connect task inside VoiceRuntime
  // establishes WebRTC P2P; no manual connectDirect call is needed.
  await alice.evaluate(() => window.__voice.startVoice());
  await bob.evaluate(() => window.__voice.startVoice());

  // Stream frames from alice continuously so auto-connect + jitter
  // buffer have time to establish P2P and Opus has a few frames of
  // priming before a "real" frame is delivered to Bob's recorder.
  // 5 s at 50 ms intervals = 100 frames.
  for (let counter = 0; counter < 100; counter++) {
    await alice.evaluate(
      (s) => window.__voice.injectPcm(s),
      Array.from(syntheticPcm(counter)),
    );
    await alice.waitForTimeout(50);
  }

  // Poll bob for a recorded frame from alice with real audio energy
  // within 5 s.
  //
  // Why an RMS threshold rather than just "any frame" — the jitter
  // pump pads underruns with the previous PCM, then with silence,
  // which would let the test pass even if Opus delivered nothing.
  // A threshold of 0.05 catches Opus-decoded sine (≈ 0.35 RMS for a
  // 0.5-amplitude input) while comfortably rejecting silence.
  const deadline = Date.now() + 5_000;
  let goodFrames = null;
  while (Date.now() < deadline) {
    const frames = await bob.evaluate(
      (k) => window.__voice.recordedFor(k),
      alicePk,
    );
    if (frames && frames.length > 0) {
      const real = frames.filter((f) => f.rms >= 0.05);
      if (real.length > 0) {
        goodFrames = real;
        break;
      }
    }
    await bob.waitForTimeout(100);
  }
  expect(goodFrames, "bob should receive at least one Opus-decoded frame from alice with non-trivial RMS").not.toBeNull();
  expect(goodFrames.length).toBeGreaterThan(0);

  // Verify the frame shape matches the contract: 960 samples (20 ms
  // at 48 kHz mono). RMS is the energy check; checksum is just
  // exposed so distinctness can be eyeballed across consecutive
  // frames.
  const frame = goodFrames[0];
  expect(frame.len).toBe(960);
  expect(typeof frame.checksum).toBe("string");
  expect(frame.checksum).toHaveLength(64);

  await aliceCtx.close();
  await bobCtx.close();
});
