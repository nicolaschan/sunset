// voice_denoise.spec.js — RNNoise receiver-side per-peer denoise toggle.
//
// Asserts two contracts:
//   1. The toggle UI is present on a *remote* peer's voice popover
//      (denoising your own outgoing audio isn't a thing — denoise
//      runs locally on the receiving side, so the self row doesn't
//      offer it), defaults to enabled (aria-pressed="true"), and
//      round-trips through Lustre + the WASM client without throwing.
//   2. Toggling off and back on doesn't crash the receive path: noise
//      frames keep arriving on that peer regardless of denoise state.
//      (Quality of denoising is covered by the Rust integration tests
//      in `crates/sunset-voice/tests/runtime_integration.rs`; the e2e
//      test here is wire-through only.)
//
// Pattern: spawn a relay, open two browser contexts, both join voice,
// then on bob's page open alice's voice-member row (which renders her
// peer popover) and drive the denoise toggle there.

import { test, expect, devices } from "@playwright/test";
import {
  spawnRelay,
  teardownRelay,
  freshSeedHex,
  syntheticPcm,
  installVoiceFfi,
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
    ...devices["Desktop Chrome"],
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
  await installVoiceFfi(page);
  await page.goto(`/?relay=${encodeURIComponent(relayAddr)}#voice-test-room`);
  await page.waitForFunction(() => !!window.sunsetClient, null, {
    timeout: 15_000,
  });
  return { page, ctx };
}

async function joinVoice(page) {
  await page.locator('[data-testid="voice-channel-row"]').first().click();
  await expect(page.locator('[data-testid="voice-leave"]')).toBeVisible({
    timeout: 2_000,
  });
}

function uint8ToHex(arr) {
  return Array.from(arr)
    .map((b) => b.toString(16).padStart(2, "0"))
    .join("");
}

test("per-peer denoise toggle defaults on, flips state, and survives noise traffic", async ({
  browser,
}, testInfo) => {
  // Phone and desktop share the same minibar + popover for voice
  // controls. Run the wire-through against Desktop to keep startup
  // fast (no mobile emulation overhead).
  test.skip(
    testInfo.project.name === "mobile-chrome",
    "wire-through covered by Desktop run; mobile-chrome would just duplicate",
  );

  const alice = await openPeer(browser, relay.addr);
  const bob = await openPeer(browser, relay.addr);

  await joinVoice(alice.page);
  await joinVoice(bob.page);

  const aliceBytes = await alice.page.evaluate(() =>
    Array.from(new Uint8Array(window.sunsetClient.public_key)),
  );
  const aliceHex = uint8ToHex(aliceBytes);

  // Wait for alice's row to appear in bob's roster, then open her
  // popover. The denoise toggle lives on remote peers only.
  const aliceRow = bob.page.locator(
    `[data-testid="voice-member"][data-peer-hex="${aliceHex}"]`,
  );
  await expect(aliceRow).toBeVisible({ timeout: 5_000 });
  await aliceRow.click();

  const denoiseBtn = bob.page.locator(
    '[data-testid="voice-popover-denoise"]',
  );
  await expect(denoiseBtn).toBeVisible({ timeout: 2_000 });
  // Default: denoise on for every peer. The popover switch mirrors the
  // per-peer state, so aria-pressed reads "true" until toggled off.
  await expect(denoiseBtn).toHaveAttribute("aria-pressed", "true");

  // Receiver-side wire-through: install bob's frame recorder so we
  // can confirm frames keep arriving as denoise is toggled.
  await bob.page.evaluate(() =>
    window.sunsetClient.voice_install_frame_recorder(),
  );

  async function injectNoiseFrames(page, startCounter, count) {
    for (let c = startCounter; c < startCounter + count; c++) {
      // Reuse the synthetic 440 Hz sine — what matters for this test
      // is that frames flow, not their spectral content.
      const pcm = syntheticPcm(c);
      await page.evaluate(
        (arr) => window.sunsetClient.voice_inject_pcm(new Float32Array(arr)),
        Array.from(pcm),
      );
      await page.waitForTimeout(20);
    }
  }

  // Frames flow with denoise on (default).
  await injectNoiseFrames(alice.page, 0, 30);
  await bob.page.waitForFunction(
    ([bytes]) => {
      try {
        const arr = window.sunsetClient.voice_recorded_frames(
          new Uint8Array(bytes),
        );
        return Array.isArray(arr) && arr.length >= 10;
      } catch (_e) {
        return false;
      }
    },
    [aliceBytes],
    { timeout: 3_000 },
  );

  // Toggle denoise off via the actual UI button.
  await denoiseBtn.click();
  await expect(denoiseBtn).toHaveAttribute("aria-pressed", "false");

  // Frames continue to flow with denoise off for this peer.
  const beforeOff = await bob.page.evaluate(
    ([bytes]) =>
      window.sunsetClient.voice_recorded_frames(new Uint8Array(bytes)).length,
    [aliceBytes],
  );
  await injectNoiseFrames(alice.page, 30, 30);
  await bob.page.waitForFunction(
    ([bytes, prior]) => {
      try {
        const arr = window.sunsetClient.voice_recorded_frames(
          new Uint8Array(bytes),
        );
        return Array.isArray(arr) && arr.length > prior + 10;
      } catch (_e) {
        return false;
      }
    },
    [aliceBytes, beforeOff],
    { timeout: 3_000 },
  );

  // Toggle back on and verify frames still flow.
  await denoiseBtn.click();
  await expect(denoiseBtn).toHaveAttribute("aria-pressed", "true");
  const beforeOn = await bob.page.evaluate(
    ([bytes]) =>
      window.sunsetClient.voice_recorded_frames(new Uint8Array(bytes)).length,
    [aliceBytes],
  );
  await injectNoiseFrames(alice.page, 60, 30);
  await bob.page.waitForFunction(
    ([bytes, prior]) => {
      try {
        const arr = window.sunsetClient.voice_recorded_frames(
          new Uint8Array(bytes),
        );
        return Array.isArray(arr) && arr.length > prior + 10;
      } catch (_e) {
        return false;
      }
    },
    [aliceBytes, beforeOn],
    { timeout: 3_000 },
  );

  await alice.ctx.close();
  await bob.ctx.close();
});
