// voice_three_way.spec.js — Three-way voice through the real Gleam UI.
//
// Alice, Bob, and Carol all join the same voice channel. Each injector
// is verified to reach every other peer.

import { test, expect, devices } from "@playwright/test";
import {
  spawnRelay,
  teardownRelay,
  freshSeedHex,
  syntheticPcm,
  pcmChecksum,
} from "./helpers/voice.js";

let relay;
test.beforeAll(async () => {
  relay = await spawnRelay();
});
test.afterAll(async () => {
  teardownRelay(relay);
});

async function openPeer(browser, relayAddr) {
  const ctx = await browser.newContext({
    ...devices["Pixel 7"],
    permissions: ["microphone"],
  });
  await ctx.addInitScript(() => {
    window.SUNSET_TEST = true;
  });
  const page = await ctx.newPage();
  page.on("pageerror", (err) =>
    process.stderr.write(`[pageerror] ${err.stack || err}\n`),
  );
  page.on("console", (msg) => {
    if (msg.type() === "error" || msg.type() === "warn") {
      process.stderr.write(`[console.${msg.type()}] ${msg.text()}\n`);
    }
  });
  await ctx.addInitScript((seed) => {
    localStorage.setItem("sunset/identity-seed", seed);
  }, freshSeedHex());
  await page.goto(`/?relay=${encodeURIComponent(relayAddr)}#voice-test-room`);
  await page.waitForFunction(() => !!window.sunsetClient, null, {
    timeout: 15_000,
  });
  return { page, ctx };
}

async function getPubkeyBytes(page) {
  return page.evaluate(() => Array.from(new Uint8Array(window.sunsetClient.public_key)));
}

// Wait for a peer's recorder to accumulate ≥ minFrames from a given sender.
async function waitForFrames(receiverPage, senderBytes, minFrames, timeoutMs) {
  const handle = await receiverPage.waitForFunction(
    ([bytes, min]) => {
      try {
        const arr = window.sunsetClient.voice_recorded_frames(
          new Uint8Array(bytes),
        );
        return Array.isArray(arr) && arr.length >= min ? arr : null;
      } catch (_e) {
        return null;
      }
    },
    [senderBytes, minFrames],
    { timeout: timeoutMs },
  );
  return handle.jsonValue();
}

test("three-way voice: all peers hear each other", async ({ browser }) => {
  const alice = await openPeer(browser, relay.addr);
  const bob = await openPeer(browser, relay.addr);
  const carol = await openPeer(browser, relay.addr);

  // All three join the voice channel.
  // On phone the channels rail is in a drawer; open it before clicking.
  await alice.page.locator('[data-testid="phone-rooms-toggle"]').click();
  await alice.page.locator('[data-testid="voice-channel-row"]').first().click();
  await expect(alice.page.locator('[data-testid="voice-minibar"]')).toBeVisible({
    timeout: 500,
  });

  await bob.page.locator('[data-testid="phone-rooms-toggle"]').click();
  await bob.page.locator('[data-testid="voice-channel-row"]').first().click();
  await expect(bob.page.locator('[data-testid="voice-minibar"]')).toBeVisible({
    timeout: 500,
  });

  await carol.page.locator('[data-testid="phone-rooms-toggle"]').click();
  await carol.page.locator('[data-testid="voice-channel-row"]').first().click();
  await expect(carol.page.locator('[data-testid="voice-minibar"]')).toBeVisible({
    timeout: 500,
  });

  // The minibar appears once `voice_start()` resolves Ok on the WASM side
  // (Gleam UI dispatches `VoiceStarted` from the FFI's success callback
  // before flipping `self_in_call`). Once visible, test-hook methods are
  // safe to call.

  // Detach the fake mic from the capture worklet on every peer so only
  // `voice_inject_pcm` frames flow into `runtime.send_pcm`. Without this
  // Chromium's fake-device tone interleaves with the synthetic injected
  // frames at receivers and breaks the per-counter checksum assertion.
  // (Capture-path coverage lives in voice_real_mic.spec.js.)
  for (const peer of [alice, bob, carol]) {
    await peer.page.evaluate(() => window.__voiceFfi.stopCaptureSource());
  }

  // Install frame recorders on all three.
  for (const peer of [alice, bob, carol]) {
    await peer.page.evaluate(() =>
      window.sunsetClient.voice_install_frame_recorder(),
    );
  }

  const aliceBytes = await getPubkeyBytes(alice.page);
  const carolBytes = await getPubkeyBytes(carol.page);

  // Alice injects 50 frames — bob and carol must each receive ≥ 40 total
  // frames. The codec is passthrough, so we also verify the spec's three
  // content checks (monotonic counters, no stuck-frame run > 5, per-counter
  // checksum match) for both receivers.
  for (let c = 1; c <= 50; c++) {
    const pcm = syntheticPcm(c);
    await alice.page.evaluate(
      (arr) => window.sunsetClient.voice_inject_pcm(new Float32Array(arr)),
      Array.from(pcm),
    );
    await alice.page.waitForTimeout(20);
  }

  const aliceRange = { minCounter: 1, maxCounter: 50, minCount: 40 };

  const bobFromAlice = await waitForFrames(bob.page, aliceBytes, 40, 4_000);
  expect(bobFromAlice.length).toBeGreaterThanOrEqual(40);
  assertContentChecks(bobFromAlice, aliceRange, "alice → bob");

  const carolFromAlice = await waitForFrames(carol.page, aliceBytes, 40, 4_000);
  expect(carolFromAlice.length).toBeGreaterThanOrEqual(40);
  assertContentChecks(carolFromAlice, aliceRange, "alice → carol");

  // Carol injects 50 frames — alice and bob must each receive ≥ 40 total
  // frames, and pass the same content checks (catches the bug where audio
  // is delivered to one peer but not another).
  for (let c = 1001; c <= 1050; c++) {
    const pcm = syntheticPcm(c);
    await carol.page.evaluate(
      (arr) => window.sunsetClient.voice_inject_pcm(new Float32Array(arr)),
      Array.from(pcm),
    );
    await carol.page.waitForTimeout(20);
  }

  const carolRange = { minCounter: 1001, maxCounter: 1050, minCount: 40 };

  const aliceFromCarol = await waitForFrames(alice.page, carolBytes, 40, 4_000);
  expect(aliceFromCarol.length).toBeGreaterThanOrEqual(40);
  assertContentChecks(aliceFromCarol, carolRange, "carol → alice");

  const bobFromCarol = await waitForFrames(bob.page, carolBytes, 40, 4_000);
  expect(bobFromCarol.length).toBeGreaterThanOrEqual(40);
  assertContentChecks(bobFromCarol, carolRange, "carol → bob");

  await alice.ctx.close();
  await bob.ctx.close();
  await carol.ctx.close();
});

/**
 * Spec content checks (section 3 of the voice-c2c spec). Same
 * semantics as voice_two_way's helper — see there for the rationale
 * on why we filter to confirmed-injected frames (Chromium's fake
 * mic interleaves with synthetic injection in this test environment).
 *
 * @param {Array<{seq_in_frame: number, len: number, checksum: string}>} frames
 * @param {{minCounter: number, maxCounter: number, minCount: number}} opts
 * @param {string} label
 */
function assertContentChecks(frames, opts, label) {
  const { minCounter, maxCounter, minCount } = opts;
  const expectedByCounter = new Map();
  for (let c = minCounter; c <= maxCounter; c++) {
    expectedByCounter.set(c, pcmChecksum(syntheticPcm(c)));
  }

  const confirmed = frames.filter(
    (f) =>
      expectedByCounter.has(f.seq_in_frame) &&
      f.checksum === expectedByCounter.get(f.seq_in_frame),
  );
  expect(
    confirmed.length,
    `${label}: only ${confirmed.length} confirmed-injected frames; expected ≥ ${minCount}`,
  ).toBeGreaterThanOrEqual(minCount);

  let prev = -Infinity;
  let runLen = 0;
  let runVal = null;
  const distinct = new Set();
  for (const f of confirmed) {
    expect(
      f.seq_in_frame,
      `${label}: counter regression (prev=${prev}, got=${f.seq_in_frame})`,
    ).toBeGreaterThanOrEqual(prev);
    if (f.seq_in_frame === runVal) {
      runLen += 1;
    } else {
      runVal = f.seq_in_frame;
      runLen = 1;
    }
    expect(
      runLen,
      `${label}: stuck-frame: ${runLen} consecutive frames with counter=${runVal}`,
    ).toBeLessThanOrEqual(5);
    distinct.add(f.seq_in_frame);
    prev = f.seq_in_frame;
  }
  expect(
    distinct.size,
    `${label}: only ${distinct.size} distinct injected counters delivered`,
  ).toBeGreaterThanOrEqual(minCount);
}
