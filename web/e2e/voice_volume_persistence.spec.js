// voice_volume_persistence.spec.js — the per-peer volume the user dials
// in is remembered across a page reload and re-applied to the peer's
// GainNode the moment their audio comes back.
//
// Two halves, both driven through the real popover slider (no poking at
// FFI setters):
//   1. Persistence + apply: alice sets bob to 300% (= 4× gain), reloads
//      her page (WASM heap + audio graph gone, only localStorage
//      survives — exactly like a real refresh), rejoins, and bob keeps
//      talking. Bob's GainNode on alice's side is recreated on his first
//      post-reload frame and must open at the remembered 4×, NOT the
//      hardcoded unity default — alice never touches the slider this
//      session. The popover slider also rehydrates to 300%.
//   2. Scope: the local "Output volume" (self monitoring) row drives the
//      same slider, but the local user isn't a peer, so dialing it must
//      not write the peer-volume cache.
//
// UX bound: a user who set a peer's volume and then refreshed expects
// that peer to come back at the level they chose, within the same
// rejoin budget the refresh-rejoin spec uses (~6 s link-up + a few
// seconds of audio). Beyond that the feature is broken, not slow.

import { test, expect, devices } from "@playwright/test";
import {
  spawnRelay,
  teardownRelay,
  freshSeedHex,
  syntheticPcm,
  getPubkeyHex,
} from "./helpers/voice.js";

const PEER_VOLUMES_KEY = "sunset/peer-volumes";

let relay;
test.beforeAll(async () => {
  relay = await spawnRelay();
});
test.afterAll(async () => {
  teardownRelay(relay);
});

async function openPeer(browser, relayAddr, seedHex) {
  const ctx = await browser.newContext({
    ...devices["Desktop Chrome"],
    permissions: ["microphone"],
  });
  await ctx.addInitScript(() => {
    window.SUNSET_TEST = true;
  });
  // Pin the identity seed so it survives page.reload() — the init script
  // re-runs on every navigation, so the post-refresh load comes back as
  // the same peer, just as a real user's localStorage-rooted identity
  // does.
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
  return { page, ctx };
}

async function joinVoice(page) {
  await page.locator('[data-testid="voice-channel-row"]').first().click();
  await expect(page.locator('[data-testid="voice-leave"]')).toBeVisible({
    timeout: 4_000,
  });
}

async function getPubkeyBytes(page) {
  return page.evaluate(() =>
    Array.from(new Uint8Array(window.sunsetClient.public_key)),
  );
}

// Drive the slider exactly as a pointer drag does: set DOM `value` then
// dispatch `input` so Lustre runs its decoder + dispatches
// `SetMemberVolume`. `.fill()` fires `change` only on blur, skipping the
// in-flight path the popover relies on.
async function setSliderPercent(page, percent) {
  await page.evaluate((p) => {
    const el = document.querySelector('[data-testid="voice-popover-volume"]');
    if (!el) throw new Error("voice-popover-volume slider not found");
    el.value = String(p);
    el.dispatchEvent(new Event("input", { bubbles: true }));
  }, percent);
}

async function readPeerGain(page, peerHex) {
  return page.evaluate((hex) => window.__voiceFfi.getPeerGain(hex), peerHex);
}

// Poll until the GainNode lands within `tol` of `expected`. The slider →
// Lustre → effect → FFI hop is fast but not synchronous.
async function expectPeerGainCloseTo(page, peerHex, expected, tol = 0.01) {
  await expect
    .poll(async () => readPeerGain(page, peerHex), {
      timeout: 4_000,
      message: `gain to converge near ${expected}`,
    })
    .toBeGreaterThanOrEqual(expected - tol);
  const observed = await readPeerGain(page, peerHex);
  expect(observed).toBeGreaterThanOrEqual(expected - tol);
  expect(observed).toBeLessThanOrEqual(expected + tol);
}

async function readVolumeCache(page) {
  return page.evaluate((key) => {
    const raw = localStorage.getItem(key);
    return raw ? JSON.parse(raw) : null;
  }, PEER_VOLUMES_KEY);
}

// Wait until `peerBytes` shows up in the engine-connected peer set — the
// WebRTC handshake completed and the DataChannel is open, so injected
// frames will land rather than drop on the floor.
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

async function injectFrame(page, counter) {
  const pcm = syntheticPcm(counter);
  await page.evaluate(
    (arr) => window.sunsetClient.voice_inject_pcm(new Float32Array(arr)),
    Array.from(pcm),
  );
}

// Inject frames from `senderPage` at the real 20 ms cadence until
// `predicate()` holds or `deadlineMs` elapses. A real peer talks
// continuously; the predicate is the condition wait, the 20 ms is the
// genuine inter-frame interval (not a settle-sleep).
async function injectUntil(senderPage, baseCounter, predicate, deadlineMs) {
  const deadline = Date.now() + deadlineMs;
  let c = baseCounter;
  while (Date.now() < deadline) {
    if (await predicate()) return true;
    await injectFrame(senderPage, c++);
    await senderPage.waitForTimeout(20);
  }
  return predicate();
}

test("peer volume persists across reload and re-applies to the recreated GainNode", async ({
  browser,
}) => {
  const alice = await openPeer(browser, relay.addr, freshSeedHex());
  const bob = await openPeer(browser, relay.addr, freshSeedHex());

  await joinVoice(alice.page);
  await joinVoice(bob.page);

  const bobHex = await getPubkeyHex(bob.page);
  const bobBytes = await getPubkeyBytes(bob.page);
  const aliceBytes = await getPubkeyBytes(alice.page);

  // Bob's GainNode on alice's side is allocated on his first delivered
  // frame. Gate on the link, then have bob talk until the slot exists.
  await waitForEnginePeer(alice.page, bobBytes, 6_000);
  await waitForEnginePeer(bob.page, aliceBytes, 6_000);
  expect(
    await injectUntil(
      bob.page,
      1,
      async () => (await readPeerGain(alice.page, bobHex)) !== null,
      8_000,
    ),
    "bob's GainNode should exist on alice before the link drops",
  ).toBeTruthy();

  // Alice opens bob's popover from her roster (real user path: click the
  // member row) and drags the volume slider to 300% ⇒ 4× gain.
  const bobRow = alice.page.locator(
    `[data-testid="voice-member"][data-peer-hex="${bobHex}"]`,
  );
  await expect(bobRow).toBeVisible({ timeout: 4_000 });
  await bobRow.click();
  await expect(
    alice.page.locator('[data-testid="voice-popover-volume"]'),
  ).toBeVisible({ timeout: 2_000 });
  await setSliderPercent(alice.page, 300);
  await expectPeerGainCloseTo(alice.page, bobHex, 4.0);

  // The choice is persisted as a single FIFO entry for bob (percent).
  await expect
    .poll(() => readVolumeCache(alice.page), {
      timeout: 2_000,
      message: "volume cache should persist bob@300",
    })
    .toEqual([[bobHex, 300]]);

  // --- alice refreshes; bob stays in the call ---
  await alice.page.reload();
  await alice.page.waitForFunction(() => !!window.sunsetClient, null, {
    timeout: 15_000,
  });
  await joinVoice(alice.page);
  await waitForEnginePeer(alice.page, bobBytes, 6_000);
  await waitForEnginePeer(bob.page, aliceBytes, 6_000);

  // Bob keeps talking. Alice recreates his GainNode on his first
  // post-reload frame, and it must open at the remembered 4× — alice has
  // NOT touched the slider this session.
  expect(
    await injectUntil(
      bob.page,
      5000,
      async () => (await readPeerGain(alice.page, bobHex)) !== null,
      8_000,
    ),
    "bob's GainNode should be recreated on alice after reload",
  ).toBeTruthy();
  await expectPeerGainCloseTo(alice.page, bobHex, 4.0);

  // And the popover slider rehydrates to the remembered 300%.
  const bobRow2 = alice.page.locator(
    `[data-testid="voice-member"][data-peer-hex="${bobHex}"]`,
  );
  await expect(bobRow2).toBeVisible({ timeout: 4_000 });
  await bobRow2.click();
  await expect(
    alice.page.locator('[data-testid="voice-popover-volume"]'),
  ).toHaveValue("300", { timeout: 2_000 });

  // Reset bob via the popover's reset control: it routes through the same
  // write path, so it must restore unity gain, snap the slider to 100,
  // AND rewrite the remembered 300 to 100 in the persisted cache.
  await alice.page.locator('[data-testid="voice-popover-reset"]').click();
  await expectPeerGainCloseTo(alice.page, bobHex, 1.0);
  await expect(
    alice.page.locator('[data-testid="voice-popover-volume"]'),
  ).toHaveValue("100", { timeout: 2_000 });
  await expect
    .poll(() => readVolumeCache(alice.page), {
      timeout: 2_000,
      message: "reset should rewrite the cache to bob@100",
    })
    .toEqual([[bobHex, 100]]);

  await alice.ctx.close();
  await bob.ctx.close();
});

test("self output volume is not written to the peer cache", async ({
  browser,
}) => {
  const alice = await openPeer(browser, relay.addr, freshSeedHex());
  await joinVoice(alice.page);
  const aliceHex = await getPubkeyHex(alice.page);

  // Open alice's OWN roster row → the "Output volume" (local monitoring)
  // slider, which shares the same control as the per-peer one.
  const selfRow = alice.page.locator(
    `[data-testid="voice-member"][data-peer-hex="${aliceHex}"]`,
  );
  await expect(selfRow).toBeVisible({ timeout: 4_000 });
  await selfRow.click();
  const slider = alice.page.locator('[data-testid="voice-popover-volume"]');
  await expect(slider).toBeVisible({ timeout: 2_000 });
  // Self caps at 100% (monitoring only) — confirms this really is the
  // self row, not a peer's.
  await expect(slider).toHaveAttribute("max", "100");

  await setSliderPercent(alice.page, 50);
  // The change applies locally (model + slider reflect it)...
  await expect(slider).toHaveValue("50", { timeout: 2_000 });
  // ...but the local user isn't a peer, so nothing is persisted. The
  // poll gives any (incorrect) persist effect a window to fire.
  await expect
    .poll(() => readVolumeCache(alice.page), {
      timeout: 2_000,
      message: "self output volume must not enter the peer cache",
    })
    .toBeNull();

  await alice.ctx.close();
});
