// voice_rejoin_receives.spec.js — Bug repro: when a peer leaves a voice
// channel and re-joins (same browser session), it does not receive frames
// from peers who stayed in the channel.
//
// The existing voice_churn.spec.js "re-join: two epochs" test checks that
// the *stay-er* keeps receiving from the *rejoiner*. This spec checks the
// opposite direction — the *rejoiner* receives from the *stay-er* — which
// is the case the user actually reports being broken.
//
// Per the user's complaint:
//   "when I leave an audio channel with someone in it and come back
//    (same browser) I don't reconnect to them. I have to click the reset
//    all state button and refresh the page, then I can join the voice
//    call again and it quickly reconnects to them."
//
// UX bound: a rejoiner that just clicked "join voice" expects audio from
// peers within ≈ 3 s — the same end-to-end budget every other voice spec
// uses. A real user clicking "join" will not tolerate a 30-s wait while
// the supervisor's backoff expires.

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

// Rejoiner receives from stay-er — the user's reported failure mode.
//
// Sequence:
//   1. A and B join voice.
//   2. A injects 30 frames. B's recorder confirms it received ≥ 15
//      (proves the initial connection is working in the A → B direction).
//   3. B leaves voice.
//   4. (Optional) Wait through the membership-liveness stale window so
//      A actively forgets B (and any auto_connect state pinned to B
//      gets reset). The user's repro doesn't bound the gap; long-gap
//      is the harder case because both sides have torn down voice-level
//      state and need to rebuild without re-dialing the underlying
//      WebRTC connection.
//   5. B re-joins voice.
//   6. A injects 30 new frames with a fresh counter range so pre-rejoin
//      frames can't sneak across the assertion.
//   7. B's fresh recorder (installed after rejoin, so only post-rejoin
//      frames are counted) should accumulate ≥ 15 frames within 5 s.
//
// If the bug is live, step 7 times out — the rejoiner's new voice
// runtime never wires up a working frame path to the stay-er even
// though the stay-er kept publishing.
//
// Run the same scenario across two gap durations:
//   - quick: rejoin within 1 s (auto_connect & membership liveness on
//     the stay-er still hold the rejoiner as "in-channel/dialing");
//   - long: rejoin after 10 s (everything voice-level has staled on
//     the stay-er; the only persistent state is the supervisor's
//     direct-intent dedup and the engine's still-open peer connection).
//
// We do not control lex pubkey order — freshSeedHex is random — but
// the two scenarios together exercise both stale-and-fresh and
// pinned-and-fresh combinations.
const SCENARIOS = [
  { name: "quick rejoin", gapMs: 500 },
  { name: "long rejoin (past membership stale)", gapMs: 10_000 },
];

for (const { name, gapMs } of SCENARIOS) {
  test(`${name}: rejoiner receives frames from stay-er`, async ({
    browser,
  }) => {
    const a = await openPeer(browser, relay.addr);
    const b = await openPeer(browser, relay.addr);

    await joinVoice(a.page);
    await joinVoice(b.page);

    const aBytes = await getPubkeyBytes(a.page);

    // B installs recorder, A injects, B receives — proves the A→B path
    // worked at least once.
    await b.page.evaluate(() =>
      window.sunsetClient.voice_install_frame_recorder(),
    );
    await a.page.evaluate(() =>
      window.sunsetClient.voice_install_frame_recorder(),
    );
    await injectFrames(a.page, 100, 30);
    await waitForFrames(b.page, aBytes, 15, 5_000);

    // Snapshot B's supervisor intents pre-leave. Only the side with
    // the lexicographically smaller pubkey acts as the dialer
    // (glare avoidance in auto_connect.rs); the other side is the
    // acceptor and never registers a direct intent. freshSeedHex is
    // random, so this is ~50/50 per run. We only assert the
    // post-leave cleanup when B was the dialer — in that case the
    // WebDialer's Drop must drive direct-intents back to zero so
    // the post-rejoin dial doesn't dedupe against a stale intent.
    const directIntentsBefore = await b.page.evaluate(async () => {
      const arr = await window.sunsetClient.intents();
      return arr.filter((i) => (i.label || "").startsWith("webrtc://")).length;
    });

    // B leaves.
    await b.page.locator('[data-testid="voice-leave"]').click();
    await expect(
      b.page.locator('[data-testid="voice-minibar"]'),
    ).not.toBeVisible({ timeout: 2_000 });

    // If B was the dialer, verify the WebDialer's Drop ran a
    // spawn_local cleanup that cancel_directed every IntentId it
    // accumulated. The supervisor's command channel is FIFO, so any
    // immediately-following voice_start's `connect_direct` queues
    // after these `Remove`s, restoring "rejoin = fresh dial". If B
    // was the acceptor (directIntentsBefore = 0), no cleanup is
    // needed and this assertion is satisfied vacuously by either
    // side of the runtime tearing down.
    if (directIntentsBefore > 0) {
      await b.page.waitForFunction(
        async () => {
          const arr = await window.sunsetClient.intents();
          return (
            arr.filter((i) => (i.label || "").startsWith("webrtc://"))
              .length === 0
          );
        },
        null,
        { timeout: 3_000 },
      );
    }

    // Time gap between leave and rejoin. The user doesn't bound this;
    // we exercise both extremes (see SCENARIOS comment above).
    if (gapMs > 0) {
      await b.page.waitForTimeout(gapMs);
    }

    // B re-joins.
    await joinVoice(b.page);
    // Re-install the recorder — the previous runtime's recorder went
    // away with the runtime. The fresh recorder only catches frames
    // delivered after the re-join, which is exactly what we want to
    // assert: did A→B reconnect?
    await b.page.evaluate(() =>
      window.sunsetClient.voice_install_frame_recorder(),
    );

    // A continues to inject (it never left). Use a fresh counter range
    // so a stray pre-rejoin frame can't sneak in and clear the bar.
    await injectFrames(a.page, 5000, 30);

    // The rejoiner B should hear A within UX budget (≈ 3 s of
    // audio + 2 s slack = 5 s). 15 frames is half the injected
    // budget — the contract is "rejoin reconnects A→B promptly".
    const frames = await waitForFrames(b.page, aBytes, 15, 5_000);
    expect(
      frames.length,
      `expected ≥ 15 frames from A after B re-joined (gap=${gapMs}ms), got ${frames.length}`,
    ).toBeGreaterThanOrEqual(15);

    await a.ctx.close();
    await b.ctx.close();
  });
}
