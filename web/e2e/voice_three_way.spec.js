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
  waitForVoiceConnected,
  assertOpusFramesDelivered,
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

// Wait for a peer's recorder to accumulate ≥ minFrames from a given sender.
async function waitForFrames(receiverPage, senderHex, minFrames, timeoutMs) {
  const handle = await receiverPage.waitForFunction(
    ([hex, min]) => {
      try {
        const bytes = new Uint8Array(
          hex.match(/.{2}/g).map((b) => parseInt(b, 16)),
        );
        const arr = window.sunsetClient.voice_recorded_frames(bytes);
        return Array.isArray(arr) && arr.length >= min ? arr : null;
      } catch (_e) {
        return null;
      }
    },
    [senderHex, minFrames],
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

  const [aliceHex, , carolHex] = await waitForVoiceConnected([
    alice,
    bob,
    carol,
  ]);

  // Detach the fake mic from the capture worklet on every peer so
  // only `voice_inject_pcm` frames flow into `runtime.send_pcm`.
  // Without this Chromium's fake-device 440 Hz tone interleaves with
  // our injected sine in the recorder, which doesn't break the
  // count-based assertions but pollutes the RMS-based "real audio"
  // signal. (Capture-path coverage lives in voice_real_mic.spec.js.)
  for (const peer of [alice, bob, carol]) {
    await peer.page.evaluate(() => window.__voiceFfi.stopCaptureSource());
  }

  // Install frame recorders on all three.
  for (const peer of [alice, bob, carol]) {
    await peer.page.evaluate(() =>
      window.sunsetClient.voice_install_frame_recorder(),
    );
  }

  // Alice injects 50 frames — bob and carol must each receive ≥ 40
  // total frames with non-trivial RMS (real Opus-decoded audio, not
  // silence underrun padding).
  for (let c = 1; c <= 50; c++) {
    const pcm = syntheticPcm(c);
    await alice.page.evaluate(
      (arr) => window.sunsetClient.voice_inject_pcm(new Float32Array(arr)),
      Array.from(pcm),
    );
    await alice.page.waitForTimeout(20);
  }

  const bobFromAlice = await waitForFrames(bob.page, aliceHex, 40, 4_000);
  assertOpusFramesDelivered(bobFromAlice, 40, "alice → bob");

  const carolFromAlice = await waitForFrames(carol.page, aliceHex, 40, 4_000);
  assertOpusFramesDelivered(carolFromAlice, 40, "alice → carol");

  // Carol injects 50 frames — alice and bob must each receive ≥ 40
  // total frames, same shape (catches the bug where audio is
  // delivered to one peer but not another).
  for (let c = 1001; c <= 1050; c++) {
    const pcm = syntheticPcm(c);
    await carol.page.evaluate(
      (arr) => window.sunsetClient.voice_inject_pcm(new Float32Array(arr)),
      Array.from(pcm),
    );
    await carol.page.waitForTimeout(20);
  }

  const aliceFromCarol = await waitForFrames(alice.page, carolHex, 40, 4_000);
  assertOpusFramesDelivered(aliceFromCarol, 40, "carol → alice");

  const bobFromCarol = await waitForFrames(bob.page, carolHex, 40, 4_000);
  assertOpusFramesDelivered(bobFromCarol, 40, "carol → bob");

  await alice.ctx.close();
  await bob.ctx.close();
  await carol.ctx.close();
});
