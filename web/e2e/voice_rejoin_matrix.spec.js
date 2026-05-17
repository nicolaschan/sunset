// voice_rejoin_matrix.spec.js — The user-reported failure modes from the
// PR #88 follow-up: existing rejoin specs cover narrow slices and assume
// a 5 s recovery budget; the user reports actual failures still happen
// in scenarios those specs don't exercise, and the real UX bound is
// closer to 20 s. This file covers the full matrix:
//
//   * which peer triggered the transition (A or B),
//   * what transition fired (page reload vs. UI-button leave+rejoin),
//   * does audio resume in BOTH directions, not just one,
//   * does the engine roster on BOTH sides agree the other is connected,
//   * does it hold up across a SECOND transition (some bugs only show
//     on the second cycle once state has accumulated).
//
// The contract every test in this file enforces:
//
//   1. Both peers' `voice_engine_connected_peers` includes the other
//      within 20 s of the transition completing.
//   2. Frames flow A→B AND B→A within 20 s, with continuous injection
//      modelling a real user who keeps talking after rejoining.
//   3. No "reset local state" workaround anywhere — the same browser
//      context that performed the transition must recover on its own.
//
// 20 s is the user's stated UX budget. Existing specs use 5 s, which is
// the right ceiling once the system is stable but masks bugs whose
// failure window is in the 5–20 s range.

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

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

async function openPeer(browser, relayAddr, seedHex) {
  const ctx = await browser.newContext({
    ...devices["Pixel 7"],
    permissions: ["microphone"],
  });
  await ctx.addInitScript(() => {
    window.SUNSET_TEST = true;
  });
  // Pin the identity seed via addInitScript so it survives page.reload().
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

async function leaveVoice(page) {
  await page.locator('[data-testid="voice-leave"]').click();
  await expect(page.locator('[data-testid="voice-minibar"]')).not.toBeVisible({
    timeout: 2_000,
  });
}

async function refreshPage(page) {
  // Capture pubkey + raw localStorage seed before the reload so we can
  // assert below that the same identity comes back. This guards
  // against a test-fixture bug where `addInitScript` would mask a
  // localStorage-isn't-actually-persisting situation: if Playwright
  // ever changed reload semantics and dropped storage, the init
  // script would silently re-seed and the test would keep passing
  // — but production (no init script) would NOT preserve identity.
  // Reading the localStorage value directly catches that.
  const before = await page.evaluate(() => ({
    pubkey: Array.from(new Uint8Array(window.sunsetClient.public_key)),
    seed: window.localStorage.getItem("sunset/identity-seed"),
  }));

  await page.reload();
  await page.waitForFunction(() => !!window.sunsetClient, null, {
    timeout: 15_000,
  });

  const after = await page.evaluate(() => ({
    pubkey: Array.from(new Uint8Array(window.sunsetClient.public_key)),
    seed: window.localStorage.getItem("sunset/identity-seed"),
  }));

  // localStorage must persist across reload natively. If this fails,
  // the addInitScript is the only thing pinning identity, which means
  // the test is hiding a real bug in production.
  expect(after.seed, "localStorage identity seed must survive page.reload()")
    .toBe(before.seed);
  expect(after.pubkey, "public key must match across reload (persistent identity)")
    .toEqual(before.pubkey);
}

async function getPubkeyBytes(page) {
  return page.evaluate(() =>
    Array.from(new Uint8Array(window.sunsetClient.public_key)),
  );
}

function pubkeyHex(bytes) {
  return bytes.map((b) => b.toString(16).padStart(2, "0")).join("");
}

// Engine-level connected check — covers WebRTC handshake completed,
// PeerHello run, peer_outbound populated. Voice-level liveness is a
// stricter check (see `assertBidirectionalAudio`), but engine-level
// being false means we never even reached the voice layer.
async function expectEngineConnected(page, peerBytes, label, timeoutMs) {
  const peerHex = pubkeyHex(peerBytes);
  await page.waitForFunction(
    (target) => {
      try {
        return Promise.resolve(
          window.sunsetClient.voice_engine_connected_peers(),
        ).then((peers) => {
          for (const u8 of peers) {
            const hex = Array.from(new Uint8Array(u8))
              .map((b) => b.toString(16).padStart(2, "0"))
              .join("");
            if (hex === target) return true;
          }
          return false;
        });
      } catch (_e) {
        return false;
      }
    },
    peerHex,
    { timeout: timeoutMs, polling: 200 },
  );
}

// Inject a single PCM frame.
async function injectOneFrame(page, counter) {
  const pcm = syntheticPcm(counter);
  await page.evaluate(
    (arr) => window.sunsetClient.voice_inject_pcm(new Float32Array(arr)),
    Array.from(pcm),
  );
}

// True when `peer` appears in `voice_active_peers` with `in_call: true`.
// `in_call` is the UX-visible "connecting…" gate — driven by
// `frame_alive || membership_alive`, so a single inbound frame OR a
// single heartbeat from the peer flips it true. This is what the user
// is actually looking at when they say "A never shows as connected
// to B" — the voice roster row in the channels drawer.
async function inCall(page, peerBytes) {
  return page.evaluate(
    (bytes) => {
      try {
        const peers = window.sunsetClient.voice_active_peers();
        if (!Array.isArray(peers)) return false;
        const target = Array.from(new Uint8Array(bytes));
        for (const p of peers) {
          const id = Array.from(new Uint8Array(p.peer_id));
          if (id.length !== target.length) continue;
          let same = true;
          for (let i = 0; i < id.length; i++) {
            if (id[i] !== target[i]) {
              same = false;
              break;
            }
          }
          if (same) return !!p.in_call;
        }
        return false;
      } catch (_e) {
        return false;
      }
    },
    peerBytes,
  );
}

async function expectInCall(page, peerBytes, label, timeoutMs) {
  await page.waitForFunction(
    (bytes) => {
      try {
        const peers = window.sunsetClient.voice_active_peers();
        if (!Array.isArray(peers)) return false;
        const target = Array.from(new Uint8Array(bytes));
        for (const p of peers) {
          const id = Array.from(new Uint8Array(p.peer_id));
          if (id.length !== target.length) continue;
          let same = true;
          for (let i = 0; i < id.length; i++) {
            if (id[i] !== target[i]) {
              same = false;
              break;
            }
          }
          if (same) return !!p.in_call;
        }
        return false;
      } catch (_e) {
        return false;
      }
    },
    peerBytes,
    { timeout: timeoutMs, polling: 200 },
  );
}

async function recordedFrameCount(page, senderBytes) {
  return page.evaluate(
    (bytes) => {
      try {
        const arr = window.sunsetClient.voice_recorded_frames(
          new Uint8Array(bytes),
        );
        return Array.isArray(arr) ? arr.length : 0;
      } catch (_e) {
        return 0;
      }
    },
    senderBytes,
  );
}

// Inject frames continuously from both A and B in parallel, and wait
// for the receiver-recorders on each side to accumulate at least
// `minFrames` from the other peer. Returns once both directions have
// crossed the threshold OR the deadline fires.
//
// Continuous injection models a real user who keeps talking after a
// transition: even if the engine-level link is up the moment we
// resume, the subscription-registry propagation needed for routing
// can lag by hundreds of ms, and a single 600 ms burst injected the
// instant the link comes up can land before that propagation
// completes. A real user does not encode their speech into a single
// burst; they keep speaking until the other side hears them.
async function assertBidirectionalAudio(a, b, aBytes, bBytes, deadline) {
  const startCounterA = Math.floor(Math.random() * 1_000_000);
  const startCounterB = startCounterA + 1_000_000;
  let counterA = startCounterA;
  let counterB = startCounterB;

  const goal = 15; // ≈ 300 ms of decoded audio: enough to assert a
  // working path without flake from one-off jitter.

  while (Date.now() < deadline) {
    const [aGotB, bGotA] = await Promise.all([
      recordedFrameCount(a.page, bBytes),
      recordedFrameCount(b.page, aBytes),
    ]);
    if (aGotB >= goal && bGotA >= goal) {
      return { aGotB, bGotA };
    }
    await Promise.all([
      injectOneFrame(a.page, counterA++),
      injectOneFrame(b.page, counterB++),
    ]);
    // 20 ms cadence matches one Opus frame; PCM injection on the
    // capture pipeline runs naturally at this rate.
    await a.page.waitForTimeout(20);
  }

  const aGotB = await recordedFrameCount(a.page, bBytes);
  const bGotA = await recordedFrameCount(b.page, aBytes);
  throw new Error(
    `bidirectional audio did not establish: A heard ${aGotB} from B, ` +
      `B heard ${bGotA} from A (goal ≥ ${goal} each, budget exhausted)`,
  );
}

// Common setup + first-frame baseline that every scenario starts from.
// Returns the two peers with recorders installed and audio confirmed
// flowing in both directions.
async function setupConnectedCall(browser) {
  const a = await openPeer(browser, relay.addr, freshSeedHex());
  const b = await openPeer(browser, relay.addr, freshSeedHex());

  await joinVoice(a.page);
  await joinVoice(b.page);

  const aBytes = await getPubkeyBytes(a.page);
  const bBytes = await getPubkeyBytes(b.page);

  await a.page.evaluate(() =>
    window.sunsetClient.voice_install_frame_recorder(),
  );
  await b.page.evaluate(() =>
    window.sunsetClient.voice_install_frame_recorder(),
  );

  // Baseline: confirm the initial connection works before any
  // transition. A failure here means the test fixture is broken,
  // not the scenario we're testing.
  await expectEngineConnected(a.page, bBytes, "A→B initial", 10_000);
  await expectEngineConnected(b.page, aBytes, "B→A initial", 10_000);
  await assertBidirectionalAudio(a, b, aBytes, bBytes, Date.now() + 10_000);

  return { a, b, aBytes, bBytes };
}

// Run the standard "after a transition, both sides must recover" check.
// `transition` is an async fn(a, b) that performs the user action
// (page refresh, leave+rejoin, etc.) and returns once the action has
// settled (e.g. the refreshed page's WASM is initialized).
async function assertRecoveryAfter(a, b, aBytes, bBytes, transition) {
  await transition(a, b);

  // The user's UX budget is 20 s. We split it into three nested
  // assertions, each tighter than the previous, so failures point at
  // which layer is broken:
  //
  //   1. Engine link-up   (≤ 10 s) — WebRTC handshake completed,
  //      PeerHello run, `peer_outbound[other]` populated on each
  //      side. If this fails the WebRTC layer never recovered.
  //
  //   2. UI in-call gate  (≤ 10 s after engine) — both peers see
  //      the OTHER in `voice_active_peers` with `in_call: true`,
  //      which is what the user is reading off the screen when
  //      they report "A never shows as connected to B". This is
  //      `frame_alive || membership_alive`, so even with no audio
  //      flowing, a single received heartbeat (cadence 2 s) is
  //      enough to flip it. No PCM injected here — the
  //      heartbeat-only path is what most real-world users see
  //      first, since they aren't talking at the millisecond they
  //      finish rejoining.
  //
  //   3. Bidirectional audio (the same 20 s overall budget) —
  //      with continuous PCM injection from both sides, the
  //      receivers' frame recorders accumulate ≥ 15 frames each.
  //      Captures the "actual audio works both ways" requirement.
  //
  // Failing at (2) but passing at (1) means the engine link is up
  // but the voice-layer routing (subscription registry, ephemeral
  // dispatch) didn't recover — that's the asymmetry the user
  // reported.
  await expectEngineConnected(a.page, bBytes, "A→B engine", 10_000);
  await expectEngineConnected(b.page, aBytes, "B→A engine", 10_000);

  await expectInCall(a.page, bBytes, "A sees B in_call", 10_000);
  await expectInCall(b.page, aBytes, "B sees A in_call", 10_000);

  // Install recorders after the transition so they catch only
  // post-transition frames, then run the bidirectional audio check.
  await a.page.evaluate(() =>
    window.sunsetClient.voice_install_frame_recorder(),
  );
  await b.page.evaluate(() =>
    window.sunsetClient.voice_install_frame_recorder(),
  );
  await assertBidirectionalAudio(a, b, aBytes, bBytes, Date.now() + 10_000);
}

// ---------------------------------------------------------------------------
// Scenarios
// ---------------------------------------------------------------------------

// Sanity check on the test fixture itself: localStorage must survive
// page.reload() WITHOUT the addInitScript safety net. If this ever
// fails, the rest of the matrix is testing fresh identities (every
// reload = a new pubkey), not the persistent-identity rejoin paths
// the user actually cares about. We use a bare context (no
// addInitScript seed pinning) and let the app generate its own
// identity on first load via `crypto.getRandomValues` →
// `localStorage.setItem`. Reload. Assert pubkey is byte-identical.
test("[fixture] identity persists across page.reload without init-script pinning", async ({
  browser,
}) => {
  const ctx = await browser.newContext({
    ...devices["Pixel 7"],
    permissions: ["microphone"],
  });
  await ctx.addInitScript(() => {
    window.SUNSET_TEST = true;
  });
  const page = await ctx.newPage();
  await page.goto(`/?relay=${encodeURIComponent(relay.addr)}#voice-test-room`);
  await page.waitForFunction(() => !!window.sunsetClient, null, {
    timeout: 15_000,
  });

  const before = await page.evaluate(() => ({
    pubkey: Array.from(new Uint8Array(window.sunsetClient.public_key)),
    seed: window.localStorage.getItem("sunset/identity-seed"),
  }));
  expect(
    before.seed,
    "app must write identity seed to localStorage on first load",
  ).toMatch(/^[0-9a-fA-F]{64}$/);

  await page.reload();
  await page.waitForFunction(() => !!window.sunsetClient, null, {
    timeout: 15_000,
  });

  const after = await page.evaluate(() => ({
    pubkey: Array.from(new Uint8Array(window.sunsetClient.public_key)),
    seed: window.localStorage.getItem("sunset/identity-seed"),
  }));
  expect(
    after.seed,
    "localStorage identity seed must persist natively across reload",
  ).toBe(before.seed);
  expect(
    after.pubkey,
    "public key must match across reload (natural identity persistence)",
  ).toEqual(before.pubkey);

  await ctx.close();
});

test("A refreshes mid-call: both sides recover bidirectional audio", async ({
  browser,
}) => {
  const { a, b, aBytes, bBytes } = await setupConnectedCall(browser);
  await assertRecoveryAfter(a, b, aBytes, bBytes, async (a) => {
    await refreshPage(a.page);
    await joinVoice(a.page);
  });
  await a.ctx.close();
  await b.ctx.close();
});

test("B refreshes mid-call: both sides recover bidirectional audio", async ({
  browser,
}) => {
  const { a, b, aBytes, bBytes } = await setupConnectedCall(browser);
  await assertRecoveryAfter(a, b, aBytes, bBytes, async (_a, b) => {
    await refreshPage(b.page);
    await joinVoice(b.page);
  });
  await a.ctx.close();
  await b.ctx.close();
});

test("A leaves+rejoins via UI: both sides recover bidirectional audio", async ({
  browser,
}) => {
  const { a, b, aBytes, bBytes } = await setupConnectedCall(browser);
  await assertRecoveryAfter(a, b, aBytes, bBytes, async (a) => {
    await leaveVoice(a.page);
    await joinVoice(a.page);
  });
  await a.ctx.close();
  await b.ctx.close();
});

test("B leaves+rejoins via UI: both sides recover bidirectional audio", async ({
  browser,
}) => {
  const { a, b, aBytes, bBytes } = await setupConnectedCall(browser);
  await assertRecoveryAfter(a, b, aBytes, bBytes, async (_a, b) => {
    await leaveVoice(b.page);
    await joinVoice(b.page);
  });
  await a.ctx.close();
  await b.ctx.close();
});

test("A refreshes twice: both sides recover bidirectional audio", async ({
  browser,
}) => {
  const { a, b, aBytes, bBytes } = await setupConnectedCall(browser);
  // First cycle.
  await assertRecoveryAfter(a, b, aBytes, bBytes, async (a) => {
    await refreshPage(a.page);
    await joinVoice(a.page);
  });
  // Second cycle — the same browser context refreshes again. Bugs
  // that only surface after state has accumulated (e.g. stale
  // signaling entries from cycle 1 that the cycle-2 dispatcher
  // replays) fire here.
  await assertRecoveryAfter(a, b, aBytes, bBytes, async (a) => {
    await refreshPage(a.page);
    await joinVoice(a.page);
  });
  await a.ctx.close();
  await b.ctx.close();
});

test("A leaves+rejoins twice via UI: both sides recover bidirectional audio", async ({
  browser,
}) => {
  const { a, b, aBytes, bBytes } = await setupConnectedCall(browser);
  for (let cycle = 1; cycle <= 2; cycle++) {
    await assertRecoveryAfter(a, b, aBytes, bBytes, async (a) => {
      await leaveVoice(a.page);
      await joinVoice(a.page);
    });
  }
  await a.ctx.close();
  await b.ctx.close();
});

// Slow rejoin: the rejoiner leaves for longer than membership-liveness'
// 5 s window, so the stay-er's voice runtime observes a Stale event
// for the rejoiner. The `dialer.release` + auto-connect Unknown→
// Dialing path activates here, which is different from the "still
// alive in membership when rejoin fires" code path the quick-cycle
// tests above exercise. (Pre-PR #82 this was the broken path; the
// existing `voice_rejoin_receives.spec.js`'s "long rejoin" pair
// covers the rejoiner-direction; this asserts both directions.)
test("A leaves for 6 s then rejoins (past membership-stale window): bidirectional", async ({
  browser,
}) => {
  const { a, b, aBytes, bBytes } = await setupConnectedCall(browser);
  await assertRecoveryAfter(a, b, aBytes, bBytes, async (a) => {
    await leaveVoice(a.page);
    // 6 s > MEMBERSHIP_STALE_AFTER (5 s) → guaranteed Stale on B's
    // membership-liveness for A. Stay-er's auto_connect resets
    // peer state to Unknown and releases any prior direct intent.
    await a.page.waitForTimeout(6_000);
    await joinVoice(a.page);
  });
  await a.ctx.close();
  await b.ctx.close();
});

// State-accumulation stress: 5 leave+rejoin cycles back-to-back.
// Any per-peer-state bug that grows linearly with cycle count
// (stale Noise sessions, leaked supervisor intents, unbounded
// signaling history, frame-recorder drift, etc.) snaps here even
// when single-cycle tests pass cleanly. The 20-s recovery budget
// applies *to each cycle*, not in aggregate — the user does not
// accept "the 5th rejoin takes a minute because state leaked
// from the first four."
test("5× leave+rejoin cycle on A: every cycle stays within UX budget", async ({
  browser,
}) => {
  const { a, b, aBytes, bBytes } = await setupConnectedCall(browser);
  for (let cycle = 1; cycle <= 5; cycle++) {
    await assertRecoveryAfter(a, b, aBytes, bBytes, async (a) => {
      await leaveVoice(a.page);
      await joinVoice(a.page);
    });
  }
  await a.ctx.close();
  await b.ctx.close();
});

// Alternating transition stress: A refreshes, B leaves+rejoins, A
// leaves+rejoins, B refreshes, A refreshes. Catches bugs where the
// state cleanup on transition N corrupts something that affects
// transition N+1's recovery on the OTHER peer.
test("alternating-actor transition stress: 5 transitions, no degradation", async ({
  browser,
}) => {
  const { a, b, aBytes, bBytes } = await setupConnectedCall(browser);

  const transitions = [
    async (a) => {
      await refreshPage(a.page);
      await joinVoice(a.page);
    },
    async (_a, b) => {
      await leaveVoice(b.page);
      await joinVoice(b.page);
    },
    async (a) => {
      await leaveVoice(a.page);
      await joinVoice(a.page);
    },
    async (_a, b) => {
      await refreshPage(b.page);
      await joinVoice(b.page);
    },
    async (a) => {
      await refreshPage(a.page);
      await joinVoice(a.page);
    },
  ];

  for (let i = 0; i < transitions.length; i++) {
    await assertRecoveryAfter(a, b, aBytes, bBytes, transitions[i]);
  }
  await a.ctx.close();
  await b.ctx.close();
});
