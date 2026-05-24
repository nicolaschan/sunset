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
//   2. The voice channel row reads "general" — the same default name
//      as the text channel, since the channels rail's kind separator
//      already distinguishes them. The label is what end users see in
//      the rail; renaming it without an e2e check would let it regress
//      on the next fixture refresh.
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
  getPubkeyHex,
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

test("voice channel roster: both peers visible, waveform tracks real audio", async ({
  browser,
}) => {
  const alice = await openPeer(browser, relay.addr);
  const bob = await openPeer(browser, relay.addr);

  // Voice channel row reads "general" — same default as the text
  // channel; the rail's kind separator already distinguishes them.
  // Scope to the voice-channel-row testid because the voice minibar
  // at the top of the chat panel also renders the channel name once
  // self_in_call flips true (so a bare `getByText` would match twice
  // after joining).
  await expect(
    alice.page.locator('[data-testid="voice-channel-row"]'),
  ).toContainText("general");
  await expect(
    bob.page.locator('[data-testid="voice-channel-row"]'),
  ).toContainText("general");

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

  // Once the WebRTC handshake completes, both peers should reach the
  // "connected" state (in_call=true). data-voice-connected="true"
  // means we have audio flow with that peer; this is the distinction
  // we'll pull on in the "in channel but not connected" test below.
  await expect(
    bob.page.locator(`[data-testid="voice-member"][data-peer-hex="${aliceHex}"]`),
  ).toHaveAttribute("data-voice-connected", "true", { timeout: 4_000 });
  await expect(
    alice.page.locator(`[data-testid="voice-member"][data-peer-hex="${bobHex}"]`),
  ).toHaveAttribute("data-voice-connected", "true", { timeout: 4_000 });

  // Self-level path: chromium's --use-fake-device-for-media-stream
  // pipes a steady 440 Hz tone into the capture worklet, so alice's
  // own mic level should rise above the speaking threshold within a
  // couple of seconds. Catches a regression in the
  // capture → updateSelfLevel → __voiceSelfLevelHandler dispatch that
  // would otherwise only show up on a real device.
  await alice.page.waitForFunction(
    () => (window.__voiceFfi.getSelfLevel() ?? 0) > 0.1,
    null,
    { timeout: 3_000 },
  );

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

test("voice channel roster: peer in channel but not connected renders dimmed with 'connecting…' affordance", async ({
  browser,
}) => {
  // The natural "WebRTC dial in flight" state is too fast to catch
  // deterministically in e2e (handshake completes in <500 ms in
  // tests). We exercise the same render path by dispatching a
  // synthetic VoicePeerStateChanged for a peer that's already in
  // the room: in_voice_channel=true but in_call=false. The Gleam
  // model treats this exactly like a pre-handshake state, and the
  // visual rendering is what the user is asking us to distinguish.
  //
  // Two peers joined via the normal flow, then we synthetically
  // flip alice's `in_voice_channel=true, in_call=false` on bob's
  // side to simulate "alice is in the channel but bob hasn't
  // connected to her yet". The other path — bob's natural view of
  // alice when she's actually connected — is covered by the
  // earlier roster test (data-voice-connected="true").
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

  const aliceHex = await getPubkeyHex(alice.page);
  const aliceRowOnBob = bob.page.locator(
    `[data-testid="voice-member"][data-peer-hex="${aliceHex}"]`,
  );

  // Wait until alice is in bob's roster (both connected initially).
  await expect(aliceRowOnBob).toHaveAttribute(
    "data-voice-connected",
    "true",
    { timeout: 4_000 },
  );

  // Force bob's view of alice into "in channel, not connected"
  // by re-dispatching the peer-state callback with
  // (in_call=false, in_voice_channel=true). The Gleam Msg flow
  // is the same path the runtime uses, just driven from the test
  // instead of from the bus subscriber.
  await bob.page.evaluate((hex) => {
    window.__voicePeerStateHandler(hex, false, false, false, true);
  }, aliceHex);

  // Within Lustre's next render the row should flip to
  // disconnected and show the 'connecting…' affordance instead of
  // the waveform meter. 1 s is generous for a virtual-DOM diff.
  await expect(aliceRowOnBob).toHaveAttribute(
    "data-voice-connected",
    "false",
    { timeout: 1_000 },
  );
  await expect(aliceRowOnBob).toBeVisible();
  await expect(
    aliceRowOnBob.locator('[data-testid="voice-member-not-connected"]'),
  ).toBeVisible();
  // Waveform meter should not render in the disconnected branch.
  await expect(
    aliceRowOnBob.locator('[data-testid="voice-waveform"]'),
  ).toHaveCount(0);

  await alice.ctx.close();
  await bob.ctx.close();
});

test("voice channel roster: peer in channel is visible to a user who hasn't joined yet, in observer (gray) mode", async ({
  browser,
}) => {
  // The user-facing UX: when alice joins a voice channel, bob should
  // see her in the rail *before* he clicks Join. Today's behaviour:
  // the voice runtime only spins up on JoinVoice, so durable
  // voice-presence entries never reach bob's combiner until he joins
  // — meaning the rail stays in the "idle" shape and bob has no way
  // to tell anyone is in the channel until he commits to joining.
  //
  // After the observe/activate split, bob's runtime starts observing
  // the moment the room handle is available (RoomOpened), so alice's
  // presence republish (every 2 s) reaches him via the relay-backed
  // sync layer regardless of whether he joins.
  //
  // Observer mode (bob hasn't joined): the block must read as
  // "this channel is live, you're not in it", NOT as "you are
  // connected" and NOT as "you are trying to connect". So the
  // voice-channel-row carries data-voice-self-joined="false", and
  // the connecting affordance is suppressed for every peer (we
  // aren't dialing anyone — bob hasn't asked to join).
  const alice = await openPeer(browser, relay.addr);
  const bob = await openPeer(browser, relay.addr);

  // Alice joins; bob does not.
  await alice.page
    .locator('[data-testid="voice-channel-row"]')
    .first()
    .click();
  await expect(alice.page.locator('[data-testid="voice-leave"]')).toBeVisible({
    timeout: 2_000,
  });

  const aliceHex = await getPubkeyHex(alice.page);
  const bobHex = await getPubkeyHex(bob.page);

  // Alice's row must show in bob's rail, even though bob hasn't
  // joined. Generous timeout: presence republish cadence is 2 s and
  // the entry has to traverse the relay both ways, so 8 s leaves
  // room for one missed republish without flaking.
  const aliceRowOnBob = bob.page.locator(
    `[data-testid="voice-member"][data-peer-hex="${aliceHex}"]`,
  );
  await expect(aliceRowOnBob).toBeVisible({ timeout: 8_000 });

  // The channel-row itself must mark observer mode — this is the
  // signal the styling keys off (neutral palette instead of the
  // magenta-accent in-call treatment).
  const channelRowOnBob = bob.page
    .locator('[data-testid="voice-channel-row"]')
    .first();
  await expect(channelRowOnBob).toHaveAttribute(
    "data-voice-self-joined",
    "false",
    { timeout: 2_000 },
  );

  // From bob's perspective there's no P2P leg to alice (he hasn't
  // joined the call). Crucially the UI must NOT show the
  // "connecting" affordance — bob isn't trying to connect to
  // anyone; he's an observer. Showing connecting would lie about
  // work the runtime isn't doing and add visual noise to the rail.
  await expect(aliceRowOnBob).toHaveAttribute(
    "data-voice-connected",
    "false",
    { timeout: 2_000 },
  );
  await expect(
    aliceRowOnBob.locator('[data-testid="voice-member-not-connected"]'),
  ).toHaveCount(0);
  // Waveform meter is also suppressed (no audio path, nothing to
  // visualise) — this is the same as the connected-but-not-in-call
  // branch in the in-call test; keeps the assertion symmetric.
  await expect(
    aliceRowOnBob.locator('[data-testid="voice-waveform"]'),
  ).toHaveCount(0);

  // Bob himself must not appear in bob's roster (he hasn't joined).
  // The runtime's voice-presence-membership task explicitly skips
  // self-published presence; the UI's `members_for_channels` map
  // reads self_in_call instead of `voice.peers[self]`, so a self
  // row would only appear after a real join.
  await expect(
    bob.page.locator(
      `[data-testid="voice-member"][data-peer-hex="${bobHex}"]`,
    ),
  ).toHaveCount(0);

  // The toggle must still read "Join general" — bob hasn't joined,
  // and clicking it should join (not leave). Pre-fix the rail would
  // have been idle anyway; this guards against a regression where
  // the roster appearing accidentally flipped the toggle to "Leave".
  await expect(channelRowOnBob).toHaveAttribute("aria-label", "Join general");

  // Tapping a peer row in observer mode must NOT open the per-peer
  // voice popover. The popover's controls (volume, mute-for-me,
  // send quality) only have an effect when there's an audio path
  // with that peer; in observer mode there isn't one, so making the
  // row appear interactive would lie about what tapping it does.
  // The row is rendered as a `disabled` button so the browser
  // suppresses the click at the platform level — assert that signal
  // first (deterministic, observable from the DOM) and then force-
  // click to confirm no popover surfaces even when the click is
  // pushed past Playwright's actionability check.
  await expect(aliceRowOnBob).toBeDisabled();
  await aliceRowOnBob.click({ force: true });
  await expect(
    bob.page.locator('[data-testid="voice-popover"]'),
  ).toHaveCount(0);

  // Once bob actually joins, the same row must flip to the
  // joined-mode treatment: the channel-row gains
  // data-voice-self-joined="true", and the connecting affordance
  // becomes legal again (it'll show transiently for any peer whose
  // P2P leg hasn't completed yet — there's no easy way to assert
  // that here without re-doing the synthetic-state dance the prior
  // test already covers, so we just check the joined-mode signal).
  await channelRowOnBob.click();
  await expect(bob.page.locator('[data-testid="voice-leave"]')).toBeVisible({
    timeout: 2_000,
  });
  await expect(channelRowOnBob).toHaveAttribute(
    "data-voice-self-joined",
    "true",
    { timeout: 2_000 },
  );

  await bob.page.locator('[data-testid="voice-leave"]').click();
  await expect(channelRowOnBob).toHaveAttribute(
    "data-voice-self-joined",
    "false",
    { timeout: 2_000 },
  );

  // Alice leaves. Bob's rail must return to the idle shape within
  // the presence-staleness budget. TTL is 6 s and stale-after is
  // ~8 s, so by 12 s the entry must have been swept and the live
  // block collapsed back to idle.
  await alice.page.locator('[data-testid="voice-leave"]').click();
  await expect(aliceRowOnBob).toHaveCount(0, { timeout: 12_000 });

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
