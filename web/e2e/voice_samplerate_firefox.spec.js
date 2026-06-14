// voice_samplerate_firefox.spec.js — Firefox-only regression guard for:
//
//   "Microphone access required to join voice. (AudioContext.create-
//    MediaStreamSource: Connecting AudioNodes from AudioContexts with
//    different sample-rate is currently not supported.)"
//
// Firefox delivers the microphone at the audio *device* rate (44.1 kHz in
// this headless env) and throws from createMediaStreamSource when that
// differs from the AudioContext's rate. The old code forced the capture
// context to 48 kHz, so joining voice on any non-48 kHz device threw —
// surfaced to the user as the (misleading) mic-permission toast. The fix
// lets the capture context adopt the device rate (so the mic always
// connects) and resamples to 48 kHz in the capture worklet; the resampler
// itself is covered by web/audio/resampler.test.mjs.
//
// This runs only on Firefox: Chromium's AudioContext defaults to 48 kHz
// and createMediaStreamSource resamples transparently, so the bug never
// reproduces there. The Firefox project supplies a fake mic via
// firefoxUserPrefs (see playwright.config.js).

import { test, expect } from "@playwright/test";

test("voice joins on Firefox when the audio device rate is not 48 kHz", async ({
  page,
}) => {
  page.on("pageerror", (err) =>
    process.stderr.write(`[pageerror] ${err.stack || err}\n`),
  );

  // Expose the test client so we can wait for the wasm Client + room handle
  // to land before clicking — otherwise the join can fall through to the
  // "Voice not ready" path instead of exercising mic capture.
  await page.addInitScript(() => {
    window.SUNSET_TEST = true;
  });
  await page.goto("/#voice-test-room");
  await page.waitForFunction(() => !!window.sunsetClient, null, {
    timeout: 20_000,
  });
  await page.waitForFunction(() => !!window.sunsetRoom, null, {
    timeout: 20_000,
  });

  // Open the channels rail if it's behind a toggle, then join the voice
  // channel.
  const toggle = page.locator('[data-testid="phone-rooms-toggle"]');
  if (await toggle.isVisible()) await toggle.click();
  await page.locator('[data-testid="voice-channel-row"]').first().click();

  // The fix is proven by the join succeeding: the leave button appears only
  // once self_in_call is true, which requires the capture worklet (with its
  // device-rate -> 48 kHz resampler) to have loaded and createMediaStream-
  // Source to have connected. On the unfixed code this times out — the join
  // is rolled back to the mic-permission toast.
  await expect(page.locator('[data-testid="voice-leave"]')).toBeVisible({
    timeout: 8_000,
  });

  // And the sample-rate failure must not have surfaced as the mic toast.
  await expect(
    page.locator('[data-testid="voice-error-toast"]'),
  ).not.toBeVisible();
});
