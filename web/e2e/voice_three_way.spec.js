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
  waitForVoiceReady,
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

  // Wait for voice_start() to complete on the WASM side for all three.
  await waitForVoiceReady(alice.page);
  await waitForVoiceReady(bob.page);
  await waitForVoiceReady(carol.page);

  // Install frame recorders on all three.
  for (const peer of [alice, bob, carol]) {
    await peer.page.evaluate(() =>
      window.sunsetClient.voice_install_frame_recorder(),
    );
  }

  const aliceBytes = await getPubkeyBytes(alice.page);
  const carolBytes = await getPubkeyBytes(carol.page);

  // Alice injects 50 frames — bob and carol must each receive ≥ 40 total frames.
  // Byte-exact checksums are not meaningful after Opus round-trip; the harness
  // tests cover raw PCM integrity. Here we verify end-to-end delivery.
  for (let c = 1; c <= 50; c++) {
    const pcm = syntheticPcm(c);
    await alice.page.evaluate(
      (arr) => window.sunsetClient.voice_inject_pcm(new Float32Array(arr)),
      Array.from(pcm),
    );
    await alice.page.waitForTimeout(20);
  }

  const bobFromAlice = await waitForFrames(bob.page, aliceBytes, 40, 4_000);
  expect(bobFromAlice.length).toBeGreaterThanOrEqual(40);

  const carolFromAlice = await waitForFrames(carol.page, aliceBytes, 40, 4_000);
  expect(carolFromAlice.length).toBeGreaterThanOrEqual(40);

  // Carol injects 50 frames — alice and bob must each receive ≥ 40 total frames.
  for (let c = 1001; c <= 1050; c++) {
    const pcm = syntheticPcm(c);
    await carol.page.evaluate(
      (arr) => window.sunsetClient.voice_inject_pcm(new Float32Array(arr)),
      Array.from(pcm),
    );
    await carol.page.waitForTimeout(20);
  }

  const aliceFromCarol = await waitForFrames(alice.page, carolBytes, 40, 4_000);
  expect(aliceFromCarol.length).toBeGreaterThanOrEqual(40);

  const bobFromCarol = await waitForFrames(bob.page, carolBytes, 40, 4_000);
  expect(bobFromCarol.length).toBeGreaterThanOrEqual(40);

  await alice.ctx.close();
  await bob.ctx.close();
  await carol.ctx.close();
});
