// Two-browser e2e for C2b voice over the network.
//
// Spawns a real sunset-relay, two chromium pages each load
// /voice-e2e-test.html, both join the same room, both call
// voice_start, alice calls voice_input with a known synthetic PCM
// frame, asserts bob's on_frame fires within 5 s with byte-equal
// PCM and asserts on_voice_peer_state transitions.

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

const ALICE_SEED = "a1".repeat(32);
const BOB_SEED = "b2".repeat(32);
const ROOM = "voice-test-room";

function syntheticPcm(seedByte) {
  const arr = new Float32Array(960);
  for (let i = 0; i < 960; i++) {
    arr[i] = ((seedByte + i) & 0xff) / 128.0 - 1.0;
  }
  return arr;
}

// Voice frames flow over the unreliable channel of
// SyncMessage::EphemeralDelivery (sunset-sync/src/peer.rs:371). The browser
// WebSocket transport doesn't support an unreliable channel, so two-browser
// voice requires a direct WebRTC P2P connection via Client::connect_direct.
//
// For connect_direct to succeed, both peers must call publish_room_subscription
// after add_relay so the relay forwards <fp>/webrtc/ signaling entries. The
// harness does this in its start() shim.
test("alice voice_input arrives at bob byte-equal", async ({ browser }) => {
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
    { seed: ALICE_SEED, room: ROOM, relay: relayAddress },
  );
  const bobInfo = await bob.evaluate(
    async ({ seed, room, relay }) => window.__voice.start({ seed, room, relay }),
    { seed: BOB_SEED, room: ROOM, relay: relayAddress },
  );

  const alicePk = aliceInfo.publicKey;
  const bobPk = bobInfo.publicKey;

  // Start presence on both first (so connection_mode tracking + member
  // membership work). Then alice triggers WebRTC P2P to bob.
  // Voice frames need WebRTC P2P because the WS-to-relay channel can't
  // carry unreliable EphemeralDelivery messages.
  // voice_start runs AFTER connect_direct, matching presence.spec.js's
  // working order.
  await alice.evaluate(async () => await window.__voice.startPresence());
  await bob.evaluate(async () => await window.__voice.startPresence());

  await alice.evaluate(async (pk) => await window.__voice.connectDirect(pk), bobPk);

  const directDeadline = Date.now() + 15_000;
  let aliceDirect = false;
  while (Date.now() < directDeadline) {
    const mode = await alice.evaluate((pk) => window.__voice.peerMode(pk), bobPk);
    if (mode === "direct") {
      aliceDirect = true;
      break;
    }
    await alice.waitForTimeout(200);
  }
  expect(aliceDirect, "alice→bob WebRTC P2P should be direct").toBe(true);

  await alice.evaluate(() => window.__voice.startVoice());
  await bob.evaluate(() => window.__voice.startVoice());

  // Send one frame from alice every 50 ms for 3 s.
  const sample = Array.from(syntheticPcm(0x42));
  for (let i = 0; i < 60; i++) {
    await alice.evaluate((s) => window.__voice.sendFrame(s), sample);
    await alice.waitForTimeout(50);
  }

  // Poll bob for a frame from alice.
  const deadline = Date.now() + 5_000;
  let received = null;
  while (Date.now() < deadline) {
    const frames = await bob.evaluate(
      (k) => window.__voice.framesFor(k).map((a) => Array.from(a)),
      alicePk,
    );
    if (frames.length > 0) {
      received = frames[0];
      break;
    }
    await bob.waitForTimeout(100);
  }
  expect(received).not.toBeNull();
  expect(received.length).toBe(960);

  // Passthrough codec: alice sent === bob received, byte-for-byte.
  for (let i = 0; i < 960; i++) {
    expect(received[i]).toBeCloseTo(sample[i], 6);
  }

  await aliceCtx.close();
  await bobCtx.close();
});

test("voice peer state transitions in_call -> talking -> silent -> out", async ({
  browser,
}) => {
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
    { seed: ALICE_SEED, room: ROOM, relay: relayAddress },
  );
  const bobInfo = await bob.evaluate(
    async ({ seed, room, relay }) => window.__voice.start({ seed, room, relay }),
    { seed: BOB_SEED, room: ROOM, relay: relayAddress },
  );

  const alicePk = aliceInfo.publicKey;
  const bobPk = bobInfo.publicKey;

  // Same setup as the byte-equal test: presence + connect_direct + voice_start.
  await alice.evaluate(async () => await window.__voice.startPresence());
  await bob.evaluate(async () => await window.__voice.startPresence());
  await alice.evaluate(async (pk) => await window.__voice.connectDirect(pk), bobPk);
  const directDeadline = Date.now() + 15_000;
  let aliceDirect = false;
  while (Date.now() < directDeadline) {
    const mode = await alice.evaluate((pk) => window.__voice.peerMode(pk), bobPk);
    if (mode === "direct") {
      aliceDirect = true;
      break;
    }
    await alice.waitForTimeout(200);
  }
  expect(aliceDirect, "alice→bob WebRTC P2P should be direct").toBe(true);
  await alice.evaluate(() => window.__voice.startVoice());
  await bob.evaluate(() => window.__voice.startVoice());

  // Wait for bob to see alice as in_call (one heartbeat interval ≈ 2 s).
  const inCallDeadline = Date.now() + 4_000;
  let sawInCall = false;
  while (Date.now() < inCallDeadline) {
    const st = await bob.evaluate((k) => window.__voice.voiceStateFor(k), alicePk);
    if (st && st.in_call) {
      sawInCall = true;
      break;
    }
    await bob.waitForTimeout(100);
  }
  expect(sawInCall).toBe(true);

  // Alice sends a frame. Bob should see talking=true.
  const sample = Array.from(syntheticPcm(0x77));
  await alice.evaluate((s) => window.__voice.sendFrame(s), sample);
  const talkingDeadline = Date.now() + 1_000;
  let sawTalking = false;
  while (Date.now() < talkingDeadline) {
    const st = await bob.evaluate((k) => window.__voice.voiceStateFor(k), alicePk);
    if (st && st.talking) {
      sawTalking = true;
      break;
    }
    await bob.waitForTimeout(50);
  }
  expect(sawTalking).toBe(true);

  // Stop sending frames, wait > 1 s. talking should drop to false.
  const silentDeadline = Date.now() + 2_500;
  let sawSilent = false;
  while (Date.now() < silentDeadline) {
    const st = await bob.evaluate((k) => window.__voice.voiceStateFor(k), alicePk);
    if (st && !st.talking) {
      sawSilent = true;
      break;
    }
    await bob.waitForTimeout(100);
  }
  expect(sawSilent).toBe(true);

  // Alice voice_stop, wait > 5 s for membership to expire.
  await alice.evaluate(() => window.__voice.stop());
  const outDeadline = Date.now() + 7_000;
  let sawOut = false;
  while (Date.now() < outDeadline) {
    const st = await bob.evaluate((k) => window.__voice.voiceStateFor(k), alicePk);
    if (st && !st.in_call) {
      sawOut = true;
      break;
    }
    await bob.waitForTimeout(200);
  }
  expect(sawOut).toBe(true);

  await aliceCtx.close();
  await bobCtx.close();
});
