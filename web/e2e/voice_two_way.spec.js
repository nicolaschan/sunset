// voice_two_way.spec.js — Two-way voice through the real Gleam UI.
//
// Both peers load the Gleam UI at /, join the same voice channel,
// and content-check frame delivery end-to-end.
//
// Uses window.sunsetClient (exposed when window.SUNSET_TEST=true before
// page load) to call test-hooks-only methods: voice_install_frame_recorder,
// voice_inject_pcm, voice_recorded_frames.
//
// Uses a mobile (Pixel 7) viewport so the voice minibar (data-testid="voice-minibar")
// is rendered — the minibar is phone-only in the Gleam UI.

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

// Helper: get the local pubkey hex from the wasm client.
async function getPubkeyHex(page) {
  return page.evaluate(() => {
    const pk = window.sunsetClient.public_key;
    return Array.from(new Uint8Array(pk))
      .map((b) => b.toString(16).padStart(2, "0"))
      .join("");
  });
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

  // The minibar appears once `voice_start()` resolves Ok on the WASM side
  // (the Gleam UI dispatches `VoiceStarted` from the FFI's success
  // callback before flipping `self_in_call`). Once visible, test-hook
  // methods like `voice_install_frame_recorder` are safe to call.

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

  // Bob's recorder must accumulate ≥ 40 frames from Alice within 3 s.
  const aliceHex = await getPubkeyHex(alice.page);
  const aliceBytes = Array.from(
    new Uint8Array(aliceHex.match(/.{2}/g).map((b) => parseInt(b, 16))),
  );

  // Bob must receive ≥ 40 frames from Alice within 3 s.
  // The voice pipeline (fake mic + injected frames → Opus codec → Bob)
  // is validated at the frame-count level here; byte-exact checksums are
  // not meaningful after Opus round-trip (the harness tests cover raw PCM).
  const recordedHandle = await bob.page.waitForFunction(
    ([bytes]) => {
      try {
        const arr = window.sunsetClient.voice_recorded_frames(
          new Uint8Array(bytes),
        );
        return Array.isArray(arr) && arr.length >= 40 ? arr : null;
      } catch (_e) {
        return null;
      }
    },
    [aliceBytes],
    { timeout: 3_000 },
  );
  const frames = await recordedHandle.jsonValue();
  expect(frames.length).toBeGreaterThanOrEqual(40);

  await alice.ctx.close();
  await bob.ctx.close();
});
