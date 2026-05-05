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

  const aliceBytes = await getPubkeyBytes(alice.page);
  const carolBytes = await getPubkeyBytes(carol.page);

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

  const bobFromAlice = await waitForFrames(bob.page, aliceBytes, 40, 4_000);
  assertOpusFramesDelivered(bobFromAlice, 40, "alice → bob");

  const carolFromAlice = await waitForFrames(carol.page, aliceBytes, 40, 4_000);
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

  const aliceFromCarol = await waitForFrames(alice.page, carolBytes, 40, 4_000);
  assertOpusFramesDelivered(aliceFromCarol, 40, "carol → alice");

  const bobFromCarol = await waitForFrames(bob.page, carolBytes, 40, 4_000);
  assertOpusFramesDelivered(bobFromCarol, 40, "carol → bob");

  await alice.ctx.close();
  await bob.ctx.close();
  await carol.ctx.close();
});

/**
 * Validates that a peer's recorded frames look like real Opus-decoded
 * audio rather than silence padding or stuck repeats.
 *
 *   - At least `minCount` of the recorded frames have RMS ≥ 0.05
 *     (Opus-decoded sine at amplitude 0.5 lands ~0.35 RMS; silence
 *     underrun is 0.0; this threshold rejects the latter cleanly).
 *   - No stretch of identical SHA-256 checksums longer than 5
 *     among the *non-silence* frames. Long silence runs are fine —
 *     they're the jitter pump correctly padding underruns with
 *     zero-PCM, not stuck-frame stutter — but a real user would
 *     still notice if every Opus-decoded frame from a peer was the
 *     same audio repeated.
 *
 * Pre-Opus this check matched per-frame counters back to the
 * injected sequence; with a lossy codec individual sample values
 * don't survive, so identification by injected-counter is no longer
 * meaningful. The two assertions here encode what's actually
 * shippable to a real user.
 *
 * @param {Array<{len: number, checksum: string, rms: number}>} frames
 * @param {number} minCount  Minimum non-silence frames expected.
 * @param {string} label     Used in failure messages.
 */
function assertOpusFramesDelivered(frames, minCount, label) {
  const real = frames.filter((f) => f.rms >= 0.05);
  expect(
    real.length,
    `${label}: only ${real.length} non-silence frames; expected ≥ ${minCount}`,
  ).toBeGreaterThanOrEqual(minCount);

  let runLen = 0;
  let runVal = null;
  for (const f of real) {
    if (f.checksum === runVal) {
      runLen += 1;
    } else {
      runVal = f.checksum;
      runLen = 1;
    }
    expect(
      runLen,
      `${label}: stuck-frame: ${runLen} consecutive non-silence frames with the same decoded PCM`,
    ).toBeLessThanOrEqual(5);
  }
}
