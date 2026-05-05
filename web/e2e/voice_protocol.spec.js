// Protocol regression: byte-equal voice frame round-trip.
//
// Spawns a real sunset-relay, two Chromium pages each load
// /voice-e2e-test.html, both join the same room, both call
// startVoice, Alice injects a known synthetic PCM frame via
// injectPcm, and asserts Bob's recorded frame checksum matches
// the expected SHA-256 of the same PCM bytes.
//
// Auto-connect (VoiceRuntime auto_connect task) establishes the
// WebRTC P2P connection; no manual connectDirect call is needed.

import { test, expect } from "@playwright/test";
import { spawn } from "child_process";
import { mkdtempSync, rmSync } from "fs";
import { tmpdir } from "os";
import { join } from "path";
import { createHash } from "crypto";

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

// Build a synthetic PCM frame matching synth_pcm_with_counter(counter) in Rust.
// pcm[0] = counter / 1_000_000.0, remaining samples follow a deterministic pattern.
function syntheticPcm(counter) {
  const arr = new Float32Array(960);
  arr[0] = counter / 1_000_000.0;
  for (let i = 1; i < 960; i++) {
    arr[i] = Math.sin((counter + i) / 1_000_000.0);
  }
  return arr;
}

// Compute SHA-256 of PCM as little-endian f32 bytes (matches Rust recorder).
function pcmChecksum(samples) {
  const buf = Buffer.from(samples.buffer, samples.byteOffset, samples.byteLength);
  return createHash("sha256").update(buf).digest("hex");
}

test("alice injectPcm arrives at bob byte-equal (checksum match)", async ({ browser }) => {
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

  // Compute the expected synthetic PCM and its checksum in Node.js.
  const counter = 0x4200; // arbitrary non-zero counter
  const pcm = syntheticPcm(counter);
  const expectedChecksum = pcmChecksum(pcm);
  const sample = Array.from(pcm);

  // Send frames repeatedly from alice so auto-connect + jitter buffer
  // have time to establish P2P and deliver at least one frame.
  // 3 s at 50 ms intervals = 60 frames.
  for (let i = 0; i < 60; i++) {
    await alice.evaluate((s) => window.__voice.injectPcm(s), sample);
    await alice.waitForTimeout(50);
  }

  // Poll bob for a recorded frame from alice within 5 s.
  const deadline = Date.now() + 5_000;
  let receivedFrames = null;
  while (Date.now() < deadline) {
    const frames = await bob.evaluate(
      (k) => window.__voice.recordedFor(k),
      alicePk,
    );
    if (frames && frames.length > 0) {
      receivedFrames = frames;
      break;
    }
    await bob.waitForTimeout(100);
  }
  expect(receivedFrames, "bob should receive at least one recorded frame from alice").not.toBeNull();
  expect(receivedFrames.length).toBeGreaterThan(0);

  // Verify the frame's wire-format codec, byte length, and checksum.
  // `voice_inject_pcm` takes the test-only `pcm-f32-le` path, which
  // wraps each 960-sample frame as 3840 bytes of little-endian f32 and
  // forwards them through the runtime opaquely. Bob's recorder hashes
  // the raw payload bytes, so SHA-256(payload) == SHA-256(pcm-f32-bytes)
  // == `expectedChecksum`. Real-mic capture uses WebCodecs Opus and a
  // smaller / lossy payload — exercised by voice_real_mic.spec.js.
  const frame = receivedFrames[0];
  expect(frame.codec_id).toBe("pcm-f32-le");
  expect(frame.len).toBe(3840);
  expect(frame.checksum).toBe(expectedChecksum);

  await aliceCtx.close();
  await bobCtx.close();
});
