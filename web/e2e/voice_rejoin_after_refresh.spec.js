// voice_rejoin_after_refresh.spec.js — Bug repro: when a peer refreshes
// their browser page (not just leaves/rejoins via the UI button) mid-call,
// the WebRTC connection fails to reestablish between them and the peer
// who stayed in the call.
//
// This is distinct from voice_rejoin_receives.spec.js. That spec exercises
// `voice_stop` followed by `voice_start` inside the same browser tab —
// the WASM heap, supervisor state, and identity all survive the gap. The
// user's repro here involves an actual `window.location.reload()`: the
// WASM heap dies, every in-memory intent / per-peer task / RTCPeerConnection
// on the refresher's side is gone, and only their identity seed in
// `localStorage` survives. The stay-er's side, meanwhile, still holds the
// old RTCPeerConnection (and possibly a supervisor intent if it was the
// dialer) until heartbeat / ICE timeouts force a cleanup.
//
// UX bound: a user that just clicked "refresh" (or hit F5) and then
// "join voice" expects audio with the peer they were just talking to
// within a couple of seconds. They will not tolerate a 30-s ICE-failure
// wait, much less a 45-s heartbeat timeout. We give 8 s of post-rejoin
// budget — generous compared to the 5 s voice_rejoin_receives uses,
// because the stay-er's side still has to flush its stale connection
// state before the new handshake can complete, but tight enough that
// the user can feel the difference between "fixed" and "broken".

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

async function openPeer(browser, relayAddr, seedHex) {
  const ctx = await browser.newContext({
    ...devices["Pixel 7"],
    permissions: ["microphone"],
  });
  await ctx.addInitScript(() => {
    window.SUNSET_TEST = true;
  });
  // Pin the identity seed via addInitScript so it survives page.reload().
  // The init script re-runs on every navigation in the context, so the
  // post-refresh load sees the same seed in localStorage that the
  // pre-refresh load did. This is exactly what a real user gets: their
  // identity is rooted in localStorage and persists across refreshes.
  await ctx.addInitScript((seed) => {
    localStorage.setItem("sunset/identity-seed", seed);
  }, seedHex);
  const page = await ctx.newPage();
  page.on("pageerror", (err) =>
    process.stderr.write(`[pageerror] ${err.stack || err}\n`),
  );
  page.on("console", (msg) => {
    if (msg.type() === "error" || msg.type() === "warn") {
      process.stderr.write(`[console.${msg.type()}] ${msg.text()}\n`);
    }
  });
  await page.goto(`/?relay=${encodeURIComponent(relayAddr)}#voice-test-room`);
  await page.waitForFunction(() => !!window.sunsetClient, null, {
    timeout: 15_000,
  });
  return { page, ctx, seedHex };
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

// Wait for `peer` to appear in the page's engine-connected peer set —
// i.e. the WebRTC handshake completed and PeerHello has run. Frames
// injected before this point are dropped because the peer's DataChannel
// isn't open yet; injecting a single 600 ms burst (30 frames × 20 ms)
// and then waiting silently is fine when the link is already up, but
// after a refresh the link has to be re-handshaked and the burst will
// land on the floor. Gate on engine-connected so the burst lands on a
// live channel.
async function waitForEnginePeer(page, peerBytes, timeoutMs) {
  await page.waitForFunction(
    (target) => {
      try {
        return Promise.resolve(
          window.sunsetClient.voice_engine_connected_peers(),
        ).then((peers) => {
          const targetHex = target
            .map((b) => b.toString(16).padStart(2, "0"))
            .join("");
          for (const u8 of peers) {
            const hex = Array.from(new Uint8Array(u8))
              .map((b) => b.toString(16).padStart(2, "0"))
              .join("");
            if (hex === targetHex) return true;
          }
          return false;
        });
      } catch (_e) {
        return false;
      }
    },
    peerBytes,
    { timeout: timeoutMs },
  );
}

// The user's reported failure mode: User A refreshes their browser page
// while User B stays in the voice channel. After A's page reloads and A
// rejoins the call (same identity, persisted via localStorage), audio
// fails to flow between them.
//
// We assert audio flows in **both directions** post-refresh:
//   - A→B is the direction that voice_rejoin_receives's
//     "voice_stop then voice_start" path already covers — but with a
//     full WASM-heap teardown on A's side, A's new supervisor has no
//     intent for B at all, so A is forced to redial from scratch. This
//     is the case where the rejoiner-as-dialer redial succeeds.
//   - B→A is the case the user actually reports. B never tore down
//     anything (B kept the call open). B's per-peer task / supervisor
//     intent / RTCPeerConnection for A are all in some stale state.
//     For audio to resume, B's side must observe A's old connection is
//     dead and dial fresh (or accept a fresh inbound dial) within UX
//     budget.
//
// We run a single scenario rather than the quick/long pair the existing
// rejoin spec uses, because page.reload() takes a few hundred ms by
// itself and queuing a 10-s sleep on top would push the total test past
// 30s for no diagnostic benefit — the bug fires (or doesn't) at the
// shortest gap a real user would produce.

test("refresh + rejoin while peer stays in call: audio resumes both ways", async ({
  browser,
}) => {
  const a = await openPeer(browser, relay.addr, freshSeedHex());
  const b = await openPeer(browser, relay.addr, freshSeedHex());

  await joinVoice(a.page);
  await joinVoice(b.page);

  const aBytes = await getPubkeyBytes(a.page);
  const bBytes = await getPubkeyBytes(b.page);

  // Baseline: prove the initial A↔B path works before we touch it.
  // Install recorders on both sides, send a few frames each way.
  await a.page.evaluate(() =>
    window.sunsetClient.voice_install_frame_recorder(),
  );
  await b.page.evaluate(() =>
    window.sunsetClient.voice_install_frame_recorder(),
  );
  await injectFrames(a.page, 100, 30);
  await waitForFrames(b.page, aBytes, 15, 5_000);
  await injectFrames(b.page, 200, 30);
  await waitForFrames(a.page, bBytes, 15, 5_000);

  // Refresh A's page. B stays put. The addInitScript above re-sets the
  // same identity seed in localStorage on every navigation so A's
  // pubkey is unchanged across the reload — this matches real-world
  // behaviour where the seed is the only thing persisting A's
  // identity across a refresh.
  await a.page.reload();
  await a.page.waitForFunction(() => !!window.sunsetClient, null, {
    timeout: 15_000,
  });

  // Sanity: A's pubkey is still the same one B has been talking to.
  const aBytesAfter = await getPubkeyBytes(a.page);
  expect(aBytesAfter, "identity seed must persist across reload").toEqual(
    aBytes,
  );

  // A rejoins the voice channel.
  await joinVoice(a.page);

  // Wait for the WebRTC handshake to complete (engine-level PeerHello
  // run) from both sides before we start injecting frames. Without
  // this, the 600 ms inject burst can land before the new
  // RTCPeerConnection has its DataChannel open — frames silently
  // dropped, recorder never accumulates, test times out — even though
  // the connection comes up moments later. A real user wouldn't be
  // injecting PCM at the millisecond they click "rejoin"; they'd
  // be talking continuously, so we model that by gating on link-up.
  // 6 s is the UX budget for "rejoin → audio works" — beyond that
  // it's a real bug, not a measurement artefact.
  await waitForEnginePeer(a.page, bBytes, 6_000);
  await waitForEnginePeer(b.page, aBytes, 6_000);

  // Install fresh recorders. The pre-refresh recorder on A is gone
  // (heap teardown); B's still exists but we want to assert on
  // post-rejoin frames only, so re-install on both sides with fresh
  // counters below.
  await a.page.evaluate(() =>
    window.sunsetClient.voice_install_frame_recorder(),
  );
  await b.page.evaluate(() =>
    window.sunsetClient.voice_install_frame_recorder(),
  );

  // Inject continuously on both sides for the rest of the test. The
  // engine-level PeerHello is done (we gated on it above), but the
  // ephemeral-frame routing depends on each side having received the
  // OTHER side's subscription filter through sync — which propagates
  // a beat after PeerHello via the relay. A real user talks
  // continuously after rejoining, so model that: keep frames flowing
  // until the receiver has accumulated enough or the deadline fires.
  // Run both directions in parallel — direction 1 (A→B) shouldn't
  // wait for direction 2 (B→A) to even start injecting, because the
  // failure mode we care about ("audio doesn't resume after refresh")
  // is symmetric and we want to assert both within the same UX
  // budget.
  async function injectUntil(page, baseCounter, predicate, deadline) {
    let c = baseCounter;
    while (Date.now() < deadline) {
      if (await predicate()) return;
      const pcm = syntheticPcm(c++);
      await page.evaluate(
        (arr) => window.sunsetClient.voice_inject_pcm(new Float32Array(arr)),
        Array.from(pcm),
      );
      await page.waitForTimeout(20);
    }
  }
  async function hasFrames(page, senderBytes, min) {
    return page.evaluate(
      ([bytes, m]) => {
        try {
          const arr = window.sunsetClient.voice_recorded_frames(
            new Uint8Array(bytes),
          );
          return Array.isArray(arr) && arr.length >= m;
        } catch (_e) {
          return false;
        }
      },
      [senderBytes, min],
    );
  }

  // 8 s end-to-end budget after the engine handshake completed: gives
  // post-handshake sync (subscription propagation) ample headroom
  // while staying well below the UX threshold where a user gives up
  // and reloads.
  const deadline = Date.now() + 8_000;
  await Promise.all([
    injectUntil(
      a.page,
      5000,
      () => hasFrames(b.page, aBytes, 15),
      deadline,
    ),
    injectUntil(
      b.page,
      6000,
      () => hasFrames(a.page, bBytes, 15),
      deadline,
    ),
  ]);

  // Final assertions: each side must have ≥ 15 frames from the other.
  // The injectUntil loop above returns on the first observation of
  // ≥ 15 frames; if it didn't, the count is < 15 and these fail with
  // a clear message naming the broken direction.
  const aToB = await a.page.evaluate(async () => {
    /* keep symmetry */ return 0;
  }); // (no-op, kept for parallel structure)
  void aToB;
  const aToBFrames = await b.page.evaluate(
    (bytes) =>
      window.sunsetClient.voice_recorded_frames(new Uint8Array(bytes)) || [],
    aBytes,
  );
  const bToAFrames = await a.page.evaluate(
    (bytes) =>
      window.sunsetClient.voice_recorded_frames(new Uint8Array(bytes)) || [],
    bBytes,
  );
  expect(
    aToBFrames.length,
    `A→B after refresh: expected ≥15 frames, got ${aToBFrames.length}`,
  ).toBeGreaterThanOrEqual(15);
  expect(
    bToAFrames.length,
    `B→A after refresh: expected ≥15 frames, got ${bToAFrames.length}`,
  ).toBeGreaterThanOrEqual(15);

  await a.ctx.close();
  await b.ctx.close();
});
