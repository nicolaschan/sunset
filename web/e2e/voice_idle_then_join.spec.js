// voice_idle_then_join.spec.js — Protocol-level guard for the
// observer-idle → late-join path that the user's one-way-audio report exercises.
//
// User report:
//   "after being connected to peers but without joining the voice chat for a
//    while, then when I join the voice chat I can hear them but they can't
//    hear me."
//
// The ROOT CAUSE of that report is a transport-layer defect: voice rides the
// unreliable datagram channel, and a datagram path that silently dies after
// idle (a NAT idle-expiring the UDP mapping) drops outbound voice with no
// fallback and no liveness signal. That defect CANNOT be reproduced on
// localhost — loopback never drops datagrams in transit — so it is covered by
// the deterministic native test
// `sunset-sync/src/peer.rs::dead_datagram_path_delivers_voice_over_reliable`
// (+ `datagram_path_death_is_detected_and_voice_recovers`), which uses a
// silent-drop transport fixture.
//
// THIS spec instead guards the *protocol* path the report describes — a peer
// connected at the data layer but out of the voice channel for a while, then
// joining late — and asserts both directions deliver. It passes on master
// (the relay routing is robust by design) and would catch a regression that
// broke late-join audio symmetry at the application/routing layer.
//
// Sequence:
//   1. A and B both load the room. Both are connected to the relay and in
//      *observer* mode for voice (the roster is visible, but neither has
//      joined the call).
//   2. B joins the voice channel and starts transmitting.
//   3. A stays in observer mode (connected, NOT in the call) for `gapMs`.
//      This is the "without joining for a while" precondition.
//   4. A joins the voice channel.
//   5. "I can hear them": A must receive B's frames.
//   6. "They can hear me": B must receive A's frames.
//
// UX bound: a user who just clicked "join voice" expects the peers in the
// call to hear them within ≈ 3 s of audio + slack. We use the same 5 s
// end-to-end receive budget every other voice spec uses.

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

async function joinVoice(page) {
  await page.locator('[data-testid="phone-rooms-toggle"]').click();
  await page.locator('[data-testid="voice-channel-row"]').first().click();
  await expect(page.locator('[data-testid="voice-minibar"]')).toBeVisible({
    timeout: 2_000,
  });
  await page.locator('[data-testid="drawer-backdrop"]').nth(0).click({
    position: { x: 380, y: 400 },
  });
}

async function getPubkeyBytes(page) {
  return page.evaluate(() =>
    Array.from(new Uint8Array(window.sunsetClient.public_key)),
  );
}

async function installRecorder(page) {
  await page.evaluate(() =>
    window.sunsetClient.voice_install_frame_recorder(),
  );
}

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

// Exercise a short late-join (no real idle — the pure "join into an existing
// call" case), a medium idle, and a long idle that crosses the protocol
// staleness windows (presence TTL 6s, store_data 5s, membership 8s,
// relay_broad refresh 15s, anti-entropy 30s).
const SCENARIOS = [
  { name: "short late-join", gapMs: 2_000 },
  { name: "medium idle", gapMs: 12_000 },
  { name: "long idle (past anti-entropy)", gapMs: 35_000 },
];

for (const { name, gapMs } of SCENARIOS) {
  test(`${name}: late joiner is heard by the staying peer`, async ({
    browser,
  }) => {
    test.setTimeout(90_000);
    const a = await openPeer(browser, relay.addr);
    const b = await openPeer(browser, relay.addr);

    const aBytes = await getPubkeyBytes(a.page);
    const bBytes = await getPubkeyBytes(b.page);

    // B joins the voice channel; A stays in observer mode (connected at the
    // data layer, not in the call).
    await joinVoice(b.page);

    // The "without joining for a while" idle. A is connected the whole time.
    if (gapMs > 0) {
      await a.page.waitForTimeout(gapMs);
    }

    // A joins the voice channel.
    await joinVoice(a.page);

    await installRecorder(a.page);
    await installRecorder(b.page);

    // CONTROL — "I can hear them": A receives B's audio. B has been
    // transmitting since it joined; inject a fresh burst so there is
    // guaranteed non-silence to deliver.
    await injectFrames(b.page, 200, 30);
    const bToA = await waitForFrames(a.page, bBytes, 15, 5_000);
    expect(
      bToA.length,
      `[${name}] control failed: A should hear B (got ${bToA.length} frames)`,
    ).toBeGreaterThanOrEqual(15);

    // FAILING DIRECTION — "they can't hear me": B receives A's audio.
    await injectFrames(a.page, 5_000, 30);
    const aToB = await waitForFrames(b.page, aBytes, 15, 5_000);
    expect(
      aToB.length,
      `[${name}] late joiner A is not heard by B (got ${aToB.length} frames)`,
    ).toBeGreaterThanOrEqual(15);

    await a.ctx.close();
    await b.ctx.close();
  });
}
