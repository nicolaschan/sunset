// voice_relay_fallback.spec.js — Audio crosses the relay when there is no
// direct WebRTC link, and the received frames are tagged `via: "relay"`.
//
// Both peers load with `?relay-only=1`, which makes the client never attempt
// a direct WebRTC link (Dialer::ensure_direct is a no-op). The only path for
// Bob to hear Alice is the relay's ephemeral re-forward (the relay-audio-
// fallback feature). Each delivered frame carries its inbound transport
// provenance; with no direct link every real frame must report `via: "relay"`.
// This is the browser-level half of the provenance observability — the engine
// derives `Relay`/`Direct` from the inbound session kind (covered by
// sunset-sync tests), the voice runtime threads it to the sink (covered by a
// sunset-voice test), and here we assert it survives the wasm→JS boundary in a
// real relayed call.
//
// Mirrors voice_two_way.spec.js (phone viewport, real Gleam UI, frame
// recorder) — the only differences are the `relay-only` URL flag and the
// per-frame `via` assertion.

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

// Open a fresh Gleam UI page with a fresh identity, in relay-only mode so no
// direct WebRTC link is ever attempted.
async function openRelayOnlyPeer(browser, relayAddr) {
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
  await page.goto(
    `/?relay=${encodeURIComponent(relayAddr)}&relay-only=1#voice-test-room`,
  );
  await page.waitForFunction(() => !!window.sunsetClient, null, {
    timeout: 15_000,
  });
  return { page, ctx };
}

test("relayed audio is delivered and tagged via=relay with no direct link", async ({
  browser,
}) => {
  const alice = await openRelayOnlyPeer(browser, relay.addr);
  const bob = await openRelayOnlyPeer(browser, relay.addr);

  // Join the voice channel on both (phone: open the drawer first).
  await alice.page.locator('[data-testid="phone-rooms-toggle"]').click();
  await alice.page.locator('[data-testid="voice-channel-row"]').first().click();
  await expect(
    alice.page.locator('[data-testid="voice-minibar"]'),
  ).toBeVisible({ timeout: 500 });

  await bob.page.locator('[data-testid="phone-rooms-toggle"]').click();
  await bob.page.locator('[data-testid="voice-channel-row"]').first().click();
  await expect(bob.page.locator('[data-testid="voice-minibar"]')).toBeVisible({
    timeout: 500,
  });

  // Connectivity here is established purely over the relay (membership
  // heartbeats are ephemeral and re-forwarded), since no direct link forms.
  const [aliceHex] = await waitForVoiceConnected([alice, bob]);

  // Detach the fake mic so only injected frames flow (see voice_two_way).
  await alice.page.evaluate(() => window.__voiceFfi.stopCaptureSource());
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
      (arr) => window.sunsetClient.voice_inject_pcm(new Float32Array(arr)),
      Array.from(pcm),
    );
    await alice.page.waitForTimeout(20);
  }

  // Bob must receive ≥ 40 non-silence frames from Alice within 3 s — which is
  // only possible if the relay re-forwarded them.
  const recordedHandle = await bob.page.waitForFunction(
    ([hex]) => {
      try {
        const bytes = new Uint8Array(
          hex.match(/.{2}/g).map((b) => parseInt(b, 16)),
        );
        const arr = window.sunsetClient.voice_recorded_frames(bytes);
        return Array.isArray(arr) &&
          arr.filter((f) => f.rms >= 0.05).length >= 40
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

  // Content checks (silence / stuck-frame), same as voice_two_way.
  assertOpusFramesDelivered(frames, 40, "alice → bob (relay)");

  // The provenance assertion: with no direct link, every real frame Bob
  // received crossed the relay, so each must be tagged `via: "relay"`. A
  // regression that dropped the provenance (or pinned it to "local"/"direct")
  // would surface here.
  const real = frames.filter((f) => f.rms >= 0.05);
  for (const f of real) {
    expect(
      f.via,
      `relay-only frame must be tagged "relay", got "${f.via}"`,
    ).toBe("relay");
  }

  await alice.ctx.close();
  await bob.ctx.close();
});
