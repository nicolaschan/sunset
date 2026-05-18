// voice_rejoin_audio_quality_real_mic.spec.js — End-to-end audio
// quality check across leave + rejoin using the real-mic capture path.
//
// The existing rejoin specs (voice_rejoin_matrix.spec.js,
// voice_rejoin_receives.spec.js, voice_rejoin_after_refresh.spec.js)
// assert that frames _flow_ between peers after a rejoin. They don't
// assert anything about how those frames _sound_. User-reported bug:
// "after rejoining, A hears really distorted audio from B" — the bug
// is on the leaver/rejoiner's outbound path, where after rejoin the
// receiver hears amplitude-modulated tone instead of the clean tone
// they heard before the rejoin. Frame counts stay healthy; only the
// quality drops.
//
// What this spec catches:
//
//   * The "leaked capture worklet" class of bug. On rejoin a stale
//     AudioWorkletNode left over from the previous session keeps
//     calling `client.voice_input` from its `port.onmessage` handler
//     (the source's tracks ended, so it pumps silence; nothing
//     disconnected it). The fresh post-rejoin worklet also calls
//     `voice_input`. The encoder sees ~2× the frame rate (silence
//     interleaved with real audio), the receiver plays them in
//     sequence at the natural 50 fps audio clock, and the listener
//     hears 50 Hz amplitude modulation.
//
//   * Any future regression that breaks the quality of the
//     rejoiner→stay-er audio path while leaving frame flow intact.
//
// Why the real-mic path:
//
//   * `voice_inject_pcm` bypasses the capture worklet entirely, so
//     the leaked-worklet bug is invisible to specs that use it. The
//     bug only fires when audio actually flows through the worklet
//     pipeline — i.e. through `getUserMedia` + AudioWorkletNode.
//
//   * sweep.wav (built by the Nix flake) is a 5 s constant 440 Hz
//     sine at amplitude 0.99 — Chromium loops it indefinitely. That
//     gives both sides a known-clean reference signal, which is what
//     the `tone_purity_440` metric expects (signal energy at 440 Hz
//     vs total energy on the decoded PCM).
//
// Quality metric:
//
//   `frame.tone_purity_440 ∈ [0, 1]` — Computed in the Rust recorder
//   on every delivered frame. A clean Opus round-trip of the 440 Hz
//   sine at Maximum quality lands above 0.95 in practice; the
//   amplitude-modulation distortion the bug produces drops the
//   average below 0.6. The spec uses the average over a window of
//   post-rejoin frames so a single transient codec frame at the
//   rejoin boundary doesn't dominate the assertion.

import { test, expect, devices } from "@playwright/test";
import { spawnRelay, teardownRelay, freshSeedHex } from "./helpers/voice.js";

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
  await ctx.addInitScript((seed) => {
    localStorage.setItem("sunset/identity-seed", seed);
  }, freshSeedHex());
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

async function getPubkeyBytes(page) {
  return page.evaluate(() =>
    Array.from(new Uint8Array(window.sunsetClient.public_key)),
  );
}

async function joinVoice(page) {
  // The mobile sheet stays closed on desktop, so we go straight to the
  // voice row. Real-mic project runs Desktop Chrome — see
  // playwright.config.js.
  await page.locator('[data-testid="voice-channel-row"]').first().click();
  await expect(page.locator('[data-testid="voice-leave"]')).toBeVisible({
    timeout: 5_000,
  });
}

async function leaveVoice(page) {
  await page.locator('[data-testid="voice-leave"]').click();
  await expect(page.locator('[data-testid="voice-leave"]')).not.toBeVisible({
    timeout: 5_000,
  });
}

// Collect at least `min` recorded frames from `senderBytes` and return
// the array. Throws once the budget runs out.
async function collectFrames(receiverPage, senderBytes, min, budgetMs) {
  const handle = await receiverPage.waitForFunction(
    ([bytes, target]) => {
      try {
        const arr = window.sunsetClient.voice_recorded_frames(
          new Uint8Array(bytes),
        );
        return Array.isArray(arr) && arr.length >= target ? arr : null;
      } catch (_e) {
        return null;
      }
    },
    [senderBytes, min],
    { timeout: budgetMs },
  );
  return handle.jsonValue();
}

// Average tone_purity_440 over frames whose RMS clears the silence
// floor. Silence frames have purity 0 by construction (no signal); a
// run of silence frames would pull the average toward 0 even when the
// non-silent frames are perfectly clean, so we filter to "real audio"
// before averaging. The same RMS floor (0.05) that the existing voice
// specs use for the silence-vs-audio test signal is fine here.
function avgPurityOnRealAudio(frames) {
  const real = frames.filter((f) => f.rms >= 0.05);
  const sum = real.reduce((acc, f) => acc + f.tone_purity_440, 0);
  return {
    avg: real.length === 0 ? 0 : sum / real.length,
    realCount: real.length,
    total: frames.length,
  };
}

// Quality assertion: a known clean 440 Hz tone fed through Opus and
// the playback path must land above ABS_FLOOR _and_ within REL_DELTA
// of the baseline measured on the same call before the rejoin. The
// absolute floor catches "rejoin completely broke the codec"; the
// relative delta catches "rejoin introduced perceptible distortion
// even though the absolute number still looks high enough."
const PURITY_ABS_FLOOR = 0.85;
const PURITY_REL_DELTA = 0.05;

async function withRecorder(page, fn) {
  await page.evaluate(() => window.sunsetClient.voice_install_frame_recorder());
  return fn();
}

async function setupCall(browser) {
  const alice = await openPeer(browser, relay.addr);
  const bob = await openPeer(browser, relay.addr);
  await joinVoice(alice.page);
  await joinVoice(bob.page);
  const aliceBytes = await getPubkeyBytes(alice.page);
  const bobBytes = await getPubkeyBytes(bob.page);
  return { alice, bob, aliceBytes, bobBytes };
}

async function teardown({ alice, bob }) {
  await alice.ctx.close();
  await bob.ctx.close();
}

test("B leaves + rejoins (real mic): A still hears clean 440 Hz tone from B", async ({
  browser,
}) => {
  const { alice, bob, aliceBytes: _aliceBytes, bobBytes } = await setupCall(browser);

  try {
    // Baseline: A records what B sends and we measure tone purity.
    // ≥ 50 frames is ~1 s of audio, plenty to average over.
    await withRecorder(alice.page, async () => {
      const baseline = await collectFrames(alice.page, bobBytes, 50, 10_000);
      const baselineStats = avgPurityOnRealAudio(baseline);
      expect(
        baselineStats.realCount,
        `baseline must have non-silence frames before testing rejoin quality`,
      ).toBeGreaterThanOrEqual(30);
      expect(
        baselineStats.avg,
        `baseline tone purity ${baselineStats.avg.toFixed(3)} below absolute floor — \
the real-mic capture pipeline is producing garbage even pre-rejoin, \
test fixture broken`,
      ).toBeGreaterThanOrEqual(PURITY_ABS_FLOOR);

      // B leaves and rejoins via the UI buttons. Real user action;
      // no localStorage workarounds.
      await leaveVoice(bob.page);
      await joinVoice(bob.page);

      // Re-install A's recorder so the post-rejoin sample window is
      // disjoint from the baseline. Otherwise the baseline frames
      // dominate the average for the same number-of-frames budget.
      await alice.page.evaluate(() =>
        window.sunsetClient.voice_install_frame_recorder(),
      );

      // 20 s budget for the post-rejoin sample: matches the
      // user-reported UX bound for "audio works again after rejoin"
      // (see voice_rejoin_matrix.spec.js header).
      const postRejoin = await collectFrames(alice.page, bobBytes, 50, 20_000);
      const postStats = avgPurityOnRealAudio(postRejoin);

      expect(
        postStats.realCount,
        `post-rejoin must have non-silence frames within budget`,
      ).toBeGreaterThanOrEqual(30);

      expect(
        postStats.avg,
        `post-rejoin tone purity ${postStats.avg.toFixed(3)} below absolute \
floor; baseline was ${baselineStats.avg.toFixed(3)}`,
      ).toBeGreaterThanOrEqual(PURITY_ABS_FLOOR);

      expect(
        baselineStats.avg - postStats.avg,
        `post-rejoin tone purity dropped from ${baselineStats.avg.toFixed(3)} \
to ${postStats.avg.toFixed(3)} (delta ${(baselineStats.avg - postStats.avg).toFixed(3)}). \
This is the classic "audio is distorted after rejoin" symptom — see \
voice_rejoin_audio_quality_real_mic.spec.js header for the leaked-worklet \
class of bug it's designed to catch.`,
      ).toBeLessThanOrEqual(PURITY_REL_DELTA);
    });
  } finally {
    await teardown({ alice, bob });
  }
});

test("A leaves + rejoins (real mic): B still hears clean 440 Hz tone from A", async ({
  browser,
}) => {
  const { alice, bob, aliceBytes, bobBytes: _bobBytes } = await setupCall(browser);

  try {
    await withRecorder(bob.page, async () => {
      const baseline = await collectFrames(bob.page, aliceBytes, 50, 10_000);
      const baselineStats = avgPurityOnRealAudio(baseline);
      expect(baselineStats.realCount).toBeGreaterThanOrEqual(30);
      expect(baselineStats.avg).toBeGreaterThanOrEqual(PURITY_ABS_FLOOR);

      await leaveVoice(alice.page);
      await joinVoice(alice.page);

      await bob.page.evaluate(() =>
        window.sunsetClient.voice_install_frame_recorder(),
      );

      const postRejoin = await collectFrames(bob.page, aliceBytes, 50, 20_000);
      const postStats = avgPurityOnRealAudio(postRejoin);

      expect(postStats.realCount).toBeGreaterThanOrEqual(30);
      expect(postStats.avg).toBeGreaterThanOrEqual(PURITY_ABS_FLOOR);
      expect(baselineStats.avg - postStats.avg).toBeLessThanOrEqual(
        PURITY_REL_DELTA,
      );
    });
  } finally {
    await teardown({ alice, bob });
  }
});

test("B leaves + rejoins twice (real mic): A's received audio quality holds", async ({
  browser,
}) => {
  // State-accumulation case: if the leaked-worklet teardown is fixed
  // for the first rejoin but not idempotent for the second, this
  // catches it. Same shape as the single-rejoin test, just looped.
  const { alice, bob, bobBytes } = await setupCall(browser);

  try {
    await withRecorder(alice.page, async () => {
      const baseline = await collectFrames(alice.page, bobBytes, 50, 10_000);
      const baselineStats = avgPurityOnRealAudio(baseline);
      expect(baselineStats.realCount).toBeGreaterThanOrEqual(30);
      expect(baselineStats.avg).toBeGreaterThanOrEqual(PURITY_ABS_FLOOR);

      for (let cycle = 1; cycle <= 2; cycle++) {
        await leaveVoice(bob.page);
        await joinVoice(bob.page);

        await alice.page.evaluate(() =>
          window.sunsetClient.voice_install_frame_recorder(),
        );

        const postRejoin = await collectFrames(
          alice.page,
          bobBytes,
          50,
          20_000,
        );
        const postStats = avgPurityOnRealAudio(postRejoin);

        expect(
          postStats.realCount,
          `cycle ${cycle}: must have non-silence frames within budget`,
        ).toBeGreaterThanOrEqual(30);
        expect(
          postStats.avg,
          `cycle ${cycle}: tone purity ${postStats.avg.toFixed(3)} below floor`,
        ).toBeGreaterThanOrEqual(PURITY_ABS_FLOOR);
        expect(
          baselineStats.avg - postStats.avg,
          `cycle ${cycle}: tone purity dropped from ${baselineStats.avg.toFixed(3)} \
to ${postStats.avg.toFixed(3)}`,
        ).toBeLessThanOrEqual(PURITY_REL_DELTA);
      }
    });
  } finally {
    await teardown({ alice, bob });
  }
});
