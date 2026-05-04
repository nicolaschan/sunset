// voice_real_mic.spec.js — Real-mic capture via Chromium fake-audio-capture.
//
// This spec runs only in the "chromium-real-mic" Playwright project, which
// passes --use-fake-device-for-media-stream and
// --use-file-for-fake-audio-capture=sweep.wav to Chromium. The WAV is a
// 5-second 440 Hz sine sweep generated in the Nix build.
//
// The test verifies that:
//   - ≥ 40 frames are received by bob within 5 s (real capture pipeline works)
//   - The talking-light for alice flips (bob's voice_active_peers shows
//     alice talking within 3 s)
//
// No content check (raw mic audio is not byte-deterministic across runs).

import { test, expect, devices } from "@playwright/test";
import { spawnRelay, teardownRelay, freshSeedHex, waitForVoiceReady } from "./helpers/voice.js";

// This spec is only matched by the chromium-real-mic project (see playwright.config.js).

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

async function getPubkeyBytes(page) {
  return page.evaluate(() =>
    Array.from(new Uint8Array(window.sunsetClient.public_key)),
  );
}

test("real-mic capture: bob receives ≥ 40 frames from alice within 5 s", async ({
  browser,
}) => {
  const alice = await openPeer(browser, relay.addr);
  const bob = await openPeer(browser, relay.addr);

  // On Desktop the self_control_bar appears instead of the voice-minibar.
  // The self_control_bar appears when self_in_call && active_voice_channel.
  // We assert via the data-testid="voice-leave" button which is in both.
  const aliceVoiceRow = alice.page.locator('[data-testid="voice-channel-row"]').first();
  await aliceVoiceRow.click();
  // Wait for the leave button to confirm we're in call (Desktop shows self_control_bar).
  await expect(alice.page.locator('[data-testid="voice-leave"]')).toBeVisible({
    timeout: 2_000,
  });

  const bobVoiceRow = bob.page.locator('[data-testid="voice-channel-row"]').first();
  await bobVoiceRow.click();
  await expect(bob.page.locator('[data-testid="voice-leave"]')).toBeVisible({
    timeout: 2_000,
  });

  // Wait for voice_start() to complete on the WASM side for both peers.
  await waitForVoiceReady(alice.page);
  await waitForVoiceReady(bob.page);

  // Install frame recorder on bob to capture alice's transmitted frames.
  await bob.page.evaluate(() =>
    window.sunsetClient.voice_install_frame_recorder(),
  );

  const aliceBytes = await getPubkeyBytes(alice.page);

  // Bob receives ≥ 40 frames from alice within 5 s (real capture from WAV).
  const handle = await bob.page.waitForFunction(
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
    { timeout: 5_000 },
  );
  const frames = await handle.jsonValue();
  expect(frames.length).toBeGreaterThanOrEqual(40);

  // Bob's voice_active_peers should show alice talking within 3 s.
  // voice_active_peers() returns [{ peer_id: Uint8Array, in_call, talking, is_muted }]
  await bob.page.waitForFunction(
    ([bytes]) => {
      try {
        const peers = window.sunsetClient.voice_active_peers();
        if (!Array.isArray(peers)) return false;
        return peers.some((p) => {
          const id = new Uint8Array(p.peer_id);
          if (id.length !== bytes.length) return false;
          for (let i = 0; i < bytes.length; i++) {
            if (id[i] !== bytes[i]) return false;
          }
          return p.talking;
        });
      } catch (_e) {
        return false;
      }
    },
    [aliceBytes],
    { timeout: 3_000 },
  );

  await alice.ctx.close();
  await bob.ctx.close();
});
