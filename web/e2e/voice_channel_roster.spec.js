// voice_channel_roster.spec.js — Voice channel roster + waveform.
//
// Two peers (alice, bob) join the same voice channel through the real
// Gleam UI. The test exercises three things the previous "fixture-only"
// rail couldn't:
//
//   1. Both peers appear in the channel's voice-member roster — pre-fix
//      the rail derived in_call from a short_pubkey lookup against a
//      full-hex peer dict, so non-self peers were silently filtered out
//      and only the local user ever showed.
//
//   2. The channel header reads "Voice Channel" (not "Lounge"). The
//      fixture-driven name is what end users see in the rail; renaming
//      it without an e2e check would let it regress on the next
//      fixture refresh.
//
//   3. The waveform next to a peer's name reflects real audio energy.
//      Each row exposes its smoothed level via `data-voice-level`
//      (also surfaced via `window.__voiceFfi.getPeerLevel`); after
//      alice injects 40 frames of 0.5-amplitude sine, bob's row for
//      alice must report a non-trivial level. When alice goes silent,
//      bob's row for alice must drop back below the speaking
//      threshold within a couple of seconds (so we know the meter
//      actually tracks audio, not "any audio ever delivered").
//
// Uses Desktop Chrome viewport so the channels rail is rendered
// directly (mobile would stash it in a drawer, requiring an extra
// click that doesn't add coverage to the things we're verifying).

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

async function getPubkeyHex(page) {
  return page.evaluate(() => {
    const pk = window.sunsetClient.public_key;
    return Array.from(new Uint8Array(pk))
      .map((b) => b.toString(16).padStart(2, "0"))
      .join("");
  });
}

test("voice channel roster: both peers visible, waveform tracks real audio", async ({
  browser,
}) => {
  const alice = await openPeer(browser, relay.addr);
  const bob = await openPeer(browser, relay.addr);

  // Channel header reads "Voice Channel" — the rename is part of the
  // user-visible deliverable. Asserting on text catches a future
  // fixture refresh that drops it back to "Lounge".
  await expect(alice.page.getByText("Voice Channel")).toBeVisible();
  await expect(bob.page.getByText("Voice Channel")).toBeVisible();

  // Both peers join voice. On Desktop the voice-leave button appears
  // when voice_start() resolves Ok — same gating signal the rest of
  // the voice suite uses for "wasm runtime is ready".
  await alice.page
    .locator('[data-testid="voice-channel-row"]')
    .first()
    .click();
  await expect(alice.page.locator('[data-testid="voice-leave"]')).toBeVisible({
    timeout: 2_000,
  });
  await bob.page
    .locator('[data-testid="voice-channel-row"]')
    .first()
    .click();
  await expect(bob.page.locator('[data-testid="voice-leave"]')).toBeVisible({
    timeout: 2_000,
  });

  const aliceHex = await getPubkeyHex(alice.page);
  const bobHex = await getPubkeyHex(bob.page);

  // Both peers must show in *both* channel rosters within 4 s. The
  // selector keys off `data-peer-hex` (the full pubkey hex), which is
  // the same identifier the voice subsystem uses everywhere else
  // (FFI peer table, voice.peers state, popover identity).
  await expect(
    alice.page.locator(`[data-testid="voice-member"][data-peer-hex="${aliceHex}"]`),
  ).toBeVisible({ timeout: 4_000 });
  await expect(
    alice.page.locator(`[data-testid="voice-member"][data-peer-hex="${bobHex}"]`),
  ).toBeVisible({ timeout: 4_000 });
  await expect(
    bob.page.locator(`[data-testid="voice-member"][data-peer-hex="${aliceHex}"]`),
  ).toBeVisible({ timeout: 4_000 });
  await expect(
    bob.page.locator(`[data-testid="voice-member"][data-peer-hex="${bobHex}"]`),
  ).toBeVisible({ timeout: 4_000 });

  // Detach the fake-mic from alice's capture worklet so only the
  // injected sine reaches bob's playback path. This isolates the
  // "is the waveform driven by real audio" check from chromium's
  // 440 Hz fake-device tone.
  await alice.page.evaluate(() => window.__voiceFfi.stopCaptureSource());

  // Alice injects ~1 s of 0.5-amplitude 440 Hz sine. The receiver
  // computes RMS per frame and feeds it into the per-peer EMA, which
  // is what the waveform reads from.
  for (let c = 1; c <= 50; c++) {
    const pcm = syntheticPcm(c);
    await alice.page.evaluate(
      (arr) =>
        window.sunsetClient.voice_inject_pcm(new Float32Array(arr)),
      Array.from(pcm),
    );
    await alice.page.waitForTimeout(20);
  }

  // Bob's per-peer level for alice must rise above the 0.05
  // "speaking" threshold within 3 s. We read the FFI directly (the
  // smoother is the source of truth) and also assert the rendered
  // attribute matches, so a regression in *either* layer is caught.
  await bob.page.waitForFunction(
    (hex) => (window.__voiceFfi.getPeerLevel(hex) ?? 0) > 0.1,
    aliceHex,
    { timeout: 3_000 },
  );

  const aliceRowOnBob = bob.page.locator(
    `[data-testid="voice-member"][data-peer-hex="${aliceHex}"]`,
  );
  // The rendered waveform's peer row should also flip to speaking.
  await expect(aliceRowOnBob).toHaveAttribute(
    "data-voice-speaking",
    "true",
    { timeout: 2_000 },
  );

  // Stop injecting and let alice fall silent. The level should decay
  // back below the speaking threshold — this is the difference
  // between a meter and a "did this peer ever talk" sticky flag.
  await bob.page.waitForFunction(
    (hex) => (window.__voiceFfi.getPeerLevel(hex) ?? 0) < 0.05,
    aliceHex,
    { timeout: 3_000 },
  );
  await expect(aliceRowOnBob).toHaveAttribute(
    "data-voice-speaking",
    "false",
    { timeout: 2_000 },
  );

  // While alice's row decays toward zero on bob's UI, ensure the row
  // is still in the roster — silence shouldn't unlist a peer.
  await expect(aliceRowOnBob).toBeVisible();

  await alice.ctx.close();
  await bob.ctx.close();
});

test("voice channel popover: keyed by full pubkey hex, opens for any peer", async ({
  browser,
}) => {
  const alice = await openPeer(browser, relay.addr);
  const bob = await openPeer(browser, relay.addr);

  await alice.page
    .locator('[data-testid="voice-channel-row"]')
    .first()
    .click();
  await expect(alice.page.locator('[data-testid="voice-leave"]')).toBeVisible({
    timeout: 2_000,
  });
  await bob.page
    .locator('[data-testid="voice-channel-row"]')
    .first()
    .click();
  await expect(bob.page.locator('[data-testid="voice-leave"]')).toBeVisible({
    timeout: 2_000,
  });

  const bobHex = await getPubkeyHex(bob.page);

  // Wait for bob to appear in alice's roster.
  const bobRow = alice.page.locator(
    `[data-testid="voice-member"][data-peer-hex="${bobHex}"]`,
  );
  await expect(bobRow).toBeVisible({ timeout: 4_000 });

  // Click bob's row — the per-peer popover should open. Pre-fix the
  // popover's lookup compared `m.id == MemberId(short_hex)` but the
  // click dispatched the full hex, so the popover never resolved a
  // member and rendered as an empty fragment.
  await bobRow.click();
  await expect(alice.page.locator('[data-testid="voice-popover"]')).toBeVisible(
    { timeout: 1_000 },
  );

  // Volume slider lives inside the popover; it should be reachable
  // (exercises the "popover resolved a member" path end-to-end).
  await expect(
    alice.page.locator('[data-testid="voice-popover-volume"]'),
  ).toBeVisible();

  await alice.ctx.close();
  await bob.ctx.close();
});
