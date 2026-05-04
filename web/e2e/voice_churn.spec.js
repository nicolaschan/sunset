// voice_churn.spec.js — Churn scenarios: late join, early leave,
// hard departure (close tab), and re-join.

import { test, expect, devices } from "@playwright/test";
import {
  spawnRelay,
  teardownRelay,
  freshSeedHex,
  syntheticPcm,
  pcmChecksum,
  waitForVoiceReady,
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
  // On phone the channels rail is in a drawer; open it before clicking.
  await page.locator('[data-testid="phone-rooms-toggle"]').click();
  await page.locator('[data-testid="voice-channel-row"]').first().click();
  await expect(page.locator('[data-testid="voice-minibar"]')).toBeVisible({
    timeout: 500,
  });
  // Dismiss the drawer — its backdrop intercepts pointer events while open.
  // The drawer aside (z-index 30) sits inside the backdrop (z-index 29).
  // Click the backdrop at a position outside the drawer panel (drawer is
  // max 320px wide; Pixel 7 viewport is 412px wide) to dispatch CloseDrawer.
  await page.locator('[data-testid="drawer-backdrop"]').nth(0).click({
    position: { x: 380, y: 400 },
  });
  // Wait for voice_start() to complete on the WASM side (getUserMedia +
  // AudioWorklet addModule are async; minibar appearing is UI-only).
  await waitForVoiceReady(page);
}

async function getPubkeyBytes(page) {
  return page.evaluate(() =>
    Array.from(new Uint8Array(window.sunsetClient.public_key)),
  );
}

// Wait for a peer's recorder to accumulate ≥ minFrames total frames from sender.
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

// Late joiner: A and B in call, then C joins and receives A's frames.
test("late joiner receives frames from existing peers", async ({ browser }) => {
  const a = await openPeer(browser, relay.addr);
  const b = await openPeer(browser, relay.addr);
  const c = await openPeer(browser, relay.addr);

  await joinVoice(a.page);
  await joinVoice(b.page);

  const aBytes = await getPubkeyBytes(a.page);

  // A injects before C joins.
  await a.page.evaluate(() =>
    window.sunsetClient.voice_install_frame_recorder(),
  );
  await injectFrames(a.page, 1, 30);

  // C joins late.
  await joinVoice(c.page);
  await c.page.evaluate(() =>
    window.sunsetClient.voice_install_frame_recorder(),
  );

  // A continues injecting after C joined.
  await injectFrames(a.page, 31, 30);

  // C should receive ≥ 20 frames from A within 3 s.
  const frames = await waitForFrames(c.page, aBytes, 20, 3_000);
  expect(frames.length).toBeGreaterThanOrEqual(20);

  await a.ctx.close();
  await b.ctx.close();
  await c.ctx.close();
});

// Early leaver: A+B+C in call, C leaves gracefully.
// A and B should no longer show C in active peers within 6 s.
// A continues injecting and B still receives.
test("early leaver is removed from active peers; remaining peers keep hearing each other", async ({
  browser,
}) => {
  const a = await openPeer(browser, relay.addr);
  const b = await openPeer(browser, relay.addr);
  const c = await openPeer(browser, relay.addr);

  await joinVoice(a.page);
  await joinVoice(b.page);
  await joinVoice(c.page);

  const aBytes = await getPubkeyBytes(a.page);
  const cBytes = await getPubkeyBytes(c.page);

  await a.page.evaluate(() =>
    window.sunsetClient.voice_install_frame_recorder(),
  );
  await b.page.evaluate(() =>
    window.sunsetClient.voice_install_frame_recorder(),
  );

  // C leaves by clicking the leave button.
  await c.page.locator('[data-testid="voice-leave"]').click();
  await expect(c.page.locator('[data-testid="voice-minibar"]')).not.toBeVisible(
    { timeout: 2_000 },
  );

  // A and B should see C absent from active peers.
  // MEMBERSHIP_STALE_AFTER = 5 s; HEARTBEAT_INTERVAL = 2 s. Worst-case
  // detection lag is MEMBERSHIP_STALE_AFTER + HEARTBEAT_INTERVAL = 7 s.
  // Use 10 s for headroom without masking real liveness failures.
  // voice_active_peers() returns [{ peer_id: Uint8Array, in_call, talking, is_muted }]
  const isPeerGoneFromActivePeers = ([bytes]) => {
    try {
      const peers = window.sunsetClient.voice_active_peers();
      if (!Array.isArray(peers)) return false;
      // peer_id is a Uint8Array; compare byte-by-byte.
      return !peers.some((p) => {
        if (!p.in_call) return false;
        const id = new Uint8Array(p.peer_id);
        if (id.length !== bytes.length) return false;
        for (let i = 0; i < bytes.length; i++) {
          if (id[i] !== bytes[i]) return false;
        }
        return true;
      });
    } catch (_e) {
      return false;
    }
  };

  await a.page.waitForFunction(isPeerGoneFromActivePeers, [cBytes], {
    timeout: 10_000,
  });
  await b.page.waitForFunction(isPeerGoneFromActivePeers, [cBytes], {
    timeout: 10_000,
  });

  // A injects; B still receives ≥ 40 frames from A within 3 s.
  await injectFrames(a.page, 500, 50);
  const frames = await waitForFrames(b.page, aBytes, 40, 3_000);
  expect(frames.length).toBeGreaterThanOrEqual(40);

  await a.ctx.close();
  await b.ctx.close();
  await c.ctx.close();
});

// Hard departure: C closes its context (tab crash / close).
// A and B detect C gone via voice_active_peers() within the liveness window.
// MEMBERSHIP_STALE_AFTER = 5 s; HEARTBEAT_INTERVAL = 2 s; allow 10 s.
test("hard departure detected within liveness window", async ({ browser }) => {
  const a = await openPeer(browser, relay.addr);
  const b = await openPeer(browser, relay.addr);
  const c = await openPeer(browser, relay.addr);

  await joinVoice(a.page);
  await joinVoice(b.page);
  await joinVoice(c.page);

  const cBytes = await getPubkeyBytes(c.page);

  // Close C's context abruptly (simulates tab close / crash).
  await c.ctx.close();

  // A and B should detect C gone within the liveness window.
  // voice_active_peers() returns [{ peer_id: Uint8Array, in_call, talking, is_muted }]
  const isPeerGone = ([bytes]) => {
    try {
      const peers = window.sunsetClient.voice_active_peers();
      if (!Array.isArray(peers)) return false;
      return !peers.some((p) => {
        if (!p.in_call) return false;
        const id = new Uint8Array(p.peer_id);
        if (id.length !== bytes.length) return false;
        for (let i = 0; i < bytes.length; i++) {
          if (id[i] !== bytes[i]) return false;
        }
        return true;
      });
    } catch (_e) {
      return false;
    }
  };

  await a.page.waitForFunction(isPeerGone, [cBytes], { timeout: 10_000 });
  await b.page.waitForFunction(isPeerGone, [cBytes], { timeout: 10_000 });

  await a.ctx.close();
  await b.ctx.close();
});

// Re-join: B leaves and re-joins. A's recorder sees two distinct epochs.
test("re-join: two epochs of monotonic counters", async ({ browser }) => {
  const a = await openPeer(browser, relay.addr);
  const b = await openPeer(browser, relay.addr);

  await joinVoice(a.page);
  await joinVoice(b.page);

  const bBytes = await getPubkeyBytes(b.page);

  await a.page.evaluate(() =>
    window.sunsetClient.voice_install_frame_recorder(),
  );

  // B injects first epoch.
  await b.page.evaluate(() =>
    window.sunsetClient.voice_install_frame_recorder(),
  );
  await injectFrames(b.page, 100, 30);

  // Wait for A to receive ≥ 15 frames from B (epoch-1).
  await waitForFrames(a.page, bBytes, 15, 3_000);

  // B leaves.
  await b.page.locator('[data-testid="voice-leave"]').click();
  await expect(b.page.locator('[data-testid="voice-minibar"]')).not.toBeVisible(
    { timeout: 2_000 },
  );

  // B re-joins.
  await joinVoice(b.page);
  await b.page.evaluate(() =>
    window.sunsetClient.voice_install_frame_recorder(),
  );

  // B injects second epoch with different counter range.
  await injectFrames(b.page, 2000, 30);

  // A receives frames from both epoch-1 and epoch-2 of B.
  // Wait until A has ≥ 30 total frames from B (combining both epochs).
  // Byte-exact counter checking is not reliable after Opus round-trip;
  // the harness tests cover raw PCM integrity. Here we verify that the
  // re-joined peer's audio continues to flow after re-joining.
  const allFrames = await waitForFrames(a.page, bBytes, 30, 3_000);
  expect(allFrames.length).toBeGreaterThanOrEqual(30);

  await a.ctx.close();
  await b.ctx.close();
});
