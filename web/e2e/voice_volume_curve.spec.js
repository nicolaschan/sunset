// voice_volume_curve.spec.js — Per-peer volume slider drives the
// GainNode through the linear-then-exponential curve.
//
// The popover slider's value is the *user-facing percent* (0..500
// for non-self peers). The audio engine wants a linear `GainNode`
// multiplier. The curve in `voice_volume.gleam` keeps 0..100% linear
// (so 50% slider = 0.5 gain) and turns 100..500% into an exponential
// boost (each +100% doubles gain → 500% = 16× gain).
//
// This spec drives the actual `<input type="range">` the user
// touches — it doesn't poke at FFI methods. The contract is: when
// the user moves the slider to N%, the `GainNode.gain.value` should
// match `percent_to_gain(N)` within float-rounding tolerance.

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

async function getPubkeyHex(page) {
  return page.evaluate(() =>
    Array.from(new Uint8Array(window.sunsetClient.public_key))
      .map((b) => b.toString(16).padStart(2, "0"))
      .join(""),
  );
}

async function injectFrames(page, startCounter, count) {
  for (let c = startCounter; c < startCounter + count; c++) {
    const pcm = syntheticPcm(c);
    await page.evaluate(
      (arr) => window.sunsetClient.voice_inject_pcm(new Float32Array(arr)),
      Array.from(pcm),
    );
    await page.waitForTimeout(20);
  }
}

// Drive the slider the same way the browser does on a user pointer
// drag: set the DOM `value` then dispatch the `input` event so Lustre
// runs its decoder + dispatches `SetPeerVolume`. A plain `.fill(...)`
// fires `change` only after blur, which would skip the in-flight
// update path the popover relies on.
async function setSliderPercent(page, percent) {
  await page.evaluate((p) => {
    const el = document.querySelector('[data-testid="voice-popover-volume"]');
    if (!el) throw new Error("voice-popover-volume slider not found");
    el.value = String(p);
    el.dispatchEvent(new Event("input", { bubbles: true }));
  }, percent);
}

async function readPeerGain(page, peerHex) {
  return page.evaluate(
    (hex) => window.__voiceFfi.getPeerGain(hex),
    peerHex,
  );
}

// Wait until the GainNode value lands within `tol` of `expected`.
// The slider → Lustre → effect → FFI hop is fast but not synchronous,
// so polling matches what a real test of "I moved the slider and
// then I expect the audio to feel different" would do.
async function expectPeerGainCloseTo(page, peerHex, expected, tol = 0.01) {
  await expect
    .poll(
      async () => readPeerGain(page, peerHex),
      { timeout: 2_000, message: `gain to converge near ${expected}` },
    )
    .toBeGreaterThanOrEqual(expected - tol);
  const observed = await readPeerGain(page, peerHex);
  expect(observed).toBeGreaterThanOrEqual(expected - tol);
  expect(observed).toBeLessThanOrEqual(expected + tol);
}

test("popover slider drives GainNode through linear-then-exponential curve", async ({
  browser,
}) => {
  const alice = await openPeer(browser, relay.addr);
  const bob = await openPeer(browser, relay.addr);

  await joinVoice(alice.page);
  await joinVoice(bob.page);

  const bobHex = await getPubkeyHex(bob.page);

  // Bob's GainNode on alice's side is allocated on the first delivered
  // frame from bob. Drive bob to inject some frames so the per-peer
  // slot exists, otherwise `setPeerVolume` is a silent no-op.
  await injectFrames(bob.page, 1, 5);
  await alice.page.waitForFunction(
    (hex) => window.__voiceFfi.getPeerGain(hex) !== null,
    bobHex,
    { timeout: 3_000 },
  );

  // Open bob's voice popover from alice's roster — same path the user
  // takes (click the member row).
  const bobRow = alice.page.locator(
    `[data-testid="voice-member"][data-peer-hex="${bobHex}"]`,
  );
  await expect(bobRow).toBeVisible({ timeout: 4_000 });
  await bobRow.click();
  await expect(
    alice.page.locator('[data-testid="voice-popover-volume"]'),
  ).toBeVisible({ timeout: 2_000 });

  // The slider's `max` should be 500 (other peers), not 200.
  const sliderMax = await alice.page.evaluate(
    () =>
      document
        .querySelector('[data-testid="voice-popover-volume"]')
        .getAttribute("max"),
  );
  expect(sliderMax).toBe("500");

  // Linear segment: 50% slider ⇒ 0.5 gain.
  await setSliderPercent(alice.page, 50);
  await expectPeerGainCloseTo(alice.page, bobHex, 0.5);

  // Boundary: 100% slider ⇒ unity gain. Continuous across the linear
  // / exponential split.
  await setSliderPercent(alice.page, 100);
  await expectPeerGainCloseTo(alice.page, bobHex, 1.0);

  // Exponential segment anchors at 200% ⇒ 2× — preserves the boundary
  // behaviour of the original linear-only slider so users who used to
  // dial to 200% still hear the same boost.
  await setSliderPercent(alice.page, 200);
  await expectPeerGainCloseTo(alice.page, bobHex, 2.0);

  // 300% ⇒ 4×, 500% ⇒ 16× (the slider's new max). Confirms the curve
  // doubles per +100% and reaches the published 16× ceiling.
  await setSliderPercent(alice.page, 300);
  await expectPeerGainCloseTo(alice.page, bobHex, 4.0);

  await setSliderPercent(alice.page, 500);
  await expectPeerGainCloseTo(alice.page, bobHex, 16.0);

  await alice.ctx.close();
  await bob.ctx.close();
});
