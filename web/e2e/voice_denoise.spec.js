// voice_denoise.spec.js — RNNoise receiver-side denoise toggle.
//
// Asserts two contracts:
//   1. The toggle UI is present once the user joins voice, defaults to
//      enabled (aria-pressed="false" because the warn highlight only
//      fires for the *non-default* off state), and round-trips through
//      Lustre + the WASM client without throwing.
//   2. Toggling off and back on doesn't crash the receive path: noise
//      frames keep arriving on the peer regardless of denoise state.
//      (Quality of denoising is covered by the Rust integration tests
//      in `crates/sunset-voice/tests/runtime_integration.rs`; the e2e
//      test here is wire-through only.)
//
// Pattern lifted from voice_mute_deafen.spec.js: spawn a relay, open
// two browser contexts, join voice, then drive the desktop self-control
// bar's denoise button.

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

// Desktop viewport so the self_control_bar (which holds the denoise
// button) renders. The phone minibar layout doesn't expose denoise
// controls — the user opens a voice sheet to access them — and that
// path is out of scope for this initial wire-through test.
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
  // voice-leave appears once self_in_call flips true, which only happens
  // after the WASM voice runtime has started. 2 s is the same budget as
  // voice_mute_deafen.spec.js — anything slower is a real UX bug.
  await expect(page.locator('[data-testid="voice-leave"]')).toBeVisible({
    timeout: 2_000,
  });
}

test("denoise toggle defaults on, flips state via aria-pressed, and survives noise traffic", async ({
  browser,
}, testInfo) => {
  // The denoise toggle lives in the desktop self_control_bar today;
  // the phone minibar doesn't expose it yet (the runtime still applies
  // denoising — it just isn't user-toggleable from the phone UI). Skip
  // on the mobile project so the UI assertions don't fail spuriously.
  test.skip(
    testInfo.project.name === "mobile-chrome",
    "denoise toggle UI is desktop-only for now",
  );

  const alice = await openPeer(browser, relay.addr);
  const bob = await openPeer(browser, relay.addr);

  await joinVoice(alice.page);
  await joinVoice(bob.page);

  const denoiseBtn = bob.page.locator('[data-testid="voice-denoise-toggle"]');
  await expect(denoiseBtn).toBeVisible({ timeout: 2_000 });
  // Default: denoise on. The button only highlights for the
  // non-default state, so aria-pressed reads "false" while denoise is
  // active. (See `self_control_bar` in channels.gleam for the
  // pass-`!denoise_on` rationale.)
  await expect(denoiseBtn).toHaveAttribute("aria-pressed", "false");
  await expect(denoiseBtn).toHaveAttribute(
    "title",
    "Disable noise reduction",
  );

  // Receiver-side wire-through: install bob's frame recorder so we
  // can confirm frames keep arriving as denoise is toggled.
  await bob.page.evaluate(() =>
    window.sunsetClient.voice_install_frame_recorder(),
  );

  const aliceBytes = await alice.page.evaluate(() =>
    Array.from(new Uint8Array(window.sunsetClient.public_key)),
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
  await expect(denoiseBtn).toHaveAttribute("aria-pressed", "true");
  await expect(denoiseBtn).toHaveAttribute(
    "title",
    "Enable noise reduction",
  );

  // Frames continue to flow with denoise off.
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
  await expect(denoiseBtn).toHaveAttribute("aria-pressed", "false");
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
