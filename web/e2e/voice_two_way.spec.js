// voice_two_way.spec.js — Two-way voice through the real Gleam UI.
//
// Both peers load the Gleam UI at /, join the same voice channel,
// and content-check frame delivery end-to-end.
//
// Uses window.sunsetClient (exposed when window.SUNSET_TEST=true before
// page load) to call test-hooks-only methods: voice_install_frame_recorder,
// voice_inject_pcm, voice_recorded_frames.
//
// Uses a mobile (Pixel 7) viewport because the channels rail lives
// inside a drawer there — driving the test through a real drawer
// open + tap exercises the phone-specific layout. The minibar itself
// is identical on desktop (see voice_bar_placement.spec.js for the
// dedicated desktop placement test).

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

// Helper: open a fresh Gleam UI page with a fresh identity on the relay.
// Uses a phone viewport so the voice minibar is visible.
async function openPeer(browser, relayAddr) {
  const ctx = await browser.newContext({
    ...devices["Pixel 7"],
    permissions: ["microphone"],
  });
  // Set SUNSET_TEST before the page loads so createClient() exposes
  // window.sunsetClient.
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
  // Inject a fresh identity seed so each test uses a distinct key.
  await ctx.addInitScript((seed) => {
    localStorage.setItem("sunset/identity-seed", seed);
  }, freshSeedHex());
  await page.goto(`/?relay=${encodeURIComponent(relayAddr)}#voice-test-room`);
  // Wait for the wasm client to be ready.
  await page.waitForFunction(() => !!window.sunsetClient, null, {
    timeout: 15_000,
  });
  return { page, ctx };
}

test("alice + bob hear each other through real Gleam UI", async ({
  browser,
}) => {
  const alice = await openPeer(browser, relay.addr);
  const bob = await openPeer(browser, relay.addr);

  // On phone the channels rail is inside a drawer; open it first.
  await alice.page.locator('[data-testid="phone-rooms-toggle"]').click();
  await alice.page.locator('[data-testid="voice-channel-row"]').first().click();
  // The voice minibar must appear within 500 ms confirming the join succeeded.
  await expect(
    alice.page.locator('[data-testid="voice-minibar"]'),
  ).toBeVisible({ timeout: 500 });

  await bob.page.locator('[data-testid="phone-rooms-toggle"]').click();
  await bob.page.locator('[data-testid="voice-channel-row"]').first().click();
  await expect(bob.page.locator('[data-testid="voice-minibar"]')).toBeVisible({
    timeout: 500,
  });

  const [aliceHex] = await waitForVoiceConnected([alice, bob]);

  // Detach the fake mic from the capture worklet on the injecting
  // side so only `voice_inject_pcm` frames flow into
  // `runtime.send_pcm`. Otherwise Chromium's
  // --use-fake-device-for-media-stream feeds a continuous 440 Hz tone
  // alongside our injection. With Opus that wouldn't break the
  // count-based assertion, but it pollutes the per-frame RMS check
  // (mic-derived frames also have real audio energy and would mask a
  // bug where injected frames were silently being dropped). Capture
  // path coverage lives in voice_real_mic.spec.js.
  await alice.page.evaluate(() => window.__voiceFfi.stopCaptureSource());

  // Install frame recorders on both so delivered frames are captured.
  await alice.page.evaluate(() =>
    window.sunsetClient.voice_install_frame_recorder(),
  );
  await bob.page.evaluate(() =>
    window.sunsetClient.voice_install_frame_recorder(),
  );

  // Alice injects 50 frames at 20 ms cadence (≈ 1 s of audio).
  for (let c = 1; c <= 50; c++) {
    const pcm = syntheticPcm(c);
    await alice.page.evaluate(
      (arr) =>
        window.sunsetClient.voice_inject_pcm(new Float32Array(arr)),
      Array.from(pcm),
    );
    await alice.page.waitForTimeout(20);
  }

  // Bob must receive ≥ 40 Opus-decoded frames from Alice within
  // 3 s. The recorder's `rms` field tells real frames (≥ 0.05 for an
  // Opus-decoded 0.5-amplitude sine) from silence underrun padding
  // (≈ 0). Per-frame attribution to Alice is enforced by the
  // recorder keying its ring buffer by PeerId.
  const recordedHandle = await bob.page.waitForFunction(
    ([hex]) => {
      try {
        const bytes = new Uint8Array(
          hex.match(/.{2}/g).map((b) => parseInt(b, 16)),
        );
        const arr = window.sunsetClient.voice_recorded_frames(bytes);
        return Array.isArray(arr) && arr.filter((f) => f.rms >= 0.05).length >= 40
          ? arr
          : null;
      } catch (_e) {
        return null;
      }
    },
    [aliceHex],
    { timeout: 3_000 },
  );
  const frames = await recordedHandle.jsonValue();

  // Spec section 3: the content checks that catch "looks fine but
  // is silent / stuck / wrong-peer". With a lossy codec we can't tie
  // each frame back to an injected counter, but we can still catch:
  //   - silence/underrun masquerading as delivery (RMS threshold);
  //   - jitter pump stuttering on the same frame (no run of identical
  //     decoded checksums longer than 5 — Opus's stateful predictor
  //     ensures distinct inputs produce distinct outputs).
  assertOpusFramesDelivered(frames, 40, "alice → bob");

  await alice.ctx.close();
  await bob.ctx.close();
});
