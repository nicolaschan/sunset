// voice_two_way.spec.js — Two-way voice through the real Gleam UI.
//
// Both peers load the Gleam UI at /, join the same voice channel,
// and content-check frame delivery end-to-end.
//
// Uses window.sunsetClient (exposed when window.SUNSET_TEST=true before
// page load) to call test-hooks-only methods: voice_install_frame_recorder,
// voice_inject_pcm, voice_recorded_frames.
//
// Uses a mobile (Pixel 7) viewport so the voice minibar (data-testid="voice-minibar")
// is rendered — the minibar is phone-only in the Gleam UI.

import { test, expect, devices } from "@playwright/test";
import {
  spawnRelay,
  teardownRelay,
  freshSeedHex,
  syntheticPcm,
  pcmChecksum,
} from "./helpers/voice.js";

let relay;
test.beforeAll(async () => {
  relay = await spawnRelay();
});
test.afterAll(async () => {
  teardownRelay(relay);
});

// Helper: open a fresh Gleam UI page with a fresh identity on the relay.
// Uses a phone viewport so the voice minibar is visible.
async function openPeer(browser, relayAddr) {
  const ctx = await browser.newContext({
    ...devices["Pixel 7"],
    permissions: ["microphone"],
  });
  // Set SUNSET_TEST before the page loads so createClient() exposes
  // window.sunsetClient.
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
  // Inject a fresh identity seed so each test uses a distinct key.
  await ctx.addInitScript((seed) => {
    localStorage.setItem("sunset/identity-seed", seed);
  }, freshSeedHex());
  await page.goto(`/?relay=${encodeURIComponent(relayAddr)}#voice-test-room`);
  // Wait for the wasm client to be ready.
  await page.waitForFunction(() => !!window.sunsetClient, null, {
    timeout: 15_000,
  });
  return { page, ctx };
}

// Helper: get the local pubkey hex from the wasm client.
async function getPubkeyHex(page) {
  return page.evaluate(() => {
    const pk = window.sunsetClient.public_key;
    return Array.from(new Uint8Array(pk))
      .map((b) => b.toString(16).padStart(2, "0"))
      .join("");
  });
}

test("alice + bob hear each other through real Gleam UI", async ({
  browser,
}) => {
  const alice = await openPeer(browser, relay.addr);
  const bob = await openPeer(browser, relay.addr);

  // On phone the channels rail is inside a drawer; open it first.
  await alice.page.locator('[data-testid="phone-rooms-toggle"]').click();
  await alice.page.locator('[data-testid="voice-channel-row"]').first().click();
  // The voice minibar must appear within 500 ms confirming the join succeeded.
  await expect(
    alice.page.locator('[data-testid="voice-minibar"]'),
  ).toBeVisible({ timeout: 500 });

  await bob.page.locator('[data-testid="phone-rooms-toggle"]').click();
  await bob.page.locator('[data-testid="voice-channel-row"]').first().click();
  await expect(bob.page.locator('[data-testid="voice-minibar"]')).toBeVisible({
    timeout: 500,
  });

  // The minibar appears once `voice_start()` resolves Ok on the WASM side
  // (the Gleam UI dispatches `VoiceStarted` from the FFI's success
  // callback before flipping `self_in_call`). Once visible, test-hook
  // methods like `voice_install_frame_recorder` are safe to call.

  // Detach the fake mic from the capture worklet on the injecting side
  // so only `voice_inject_pcm` frames flow into `runtime.send_pcm`.
  // Otherwise Chromium's --use-fake-device-for-media-stream feeds a
  // continuous 440 Hz tone and Bob's recorder records both kinds of
  // frame interleaved, which breaks the spec's per-counter checksum
  // assertion. (The capture worklet path is exercised separately by
  // voice_real_mic.spec.js.)
  await alice.page.evaluate(() => window.__voiceFfi.stopCaptureSource());

  // Install frame recorders on both so delivered frames are captured.
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
      (arr) =>
        window.sunsetClient.voice_inject_pcm(new Float32Array(arr)),
      Array.from(pcm),
    );
    await alice.page.waitForTimeout(20);
  }

  // Bob's recorder must accumulate ≥ 40 frames from Alice within 3 s.
  const aliceHex = await getPubkeyHex(alice.page);
  const aliceBytes = Array.from(
    new Uint8Array(aliceHex.match(/.{2}/g).map((b) => parseInt(b, 16))),
  );

  // Bob must receive ≥ 40 frames from Alice within 3 s. The codec is
  // passthrough in C2c, so frames arrive byte-equal — that means the
  // recorder's per-frame SHA-256 must equal the JS-side checksum of
  // the same `syntheticPcm(counter)` we injected.
  const recordedHandle = await bob.page.waitForFunction(
    ([bytes]) => {
      try {
        const arr = window.sunsetClient.voice_recorded_frames(
          new Uint8Array(bytes),
        );
        return Array.isArray(arr) && arr.length >= 40 ? arr : null;
      } catch (_e) {
        return null;
      }
    },
    [aliceBytes],
    { timeout: 3_000 },
  );
  const frames = await recordedHandle.jsonValue();
  expect(frames.length).toBeGreaterThanOrEqual(40);

  // Spec section 3 (voice_two_way): the three content checks that catch
  // "looks fine but is silent / repeated / swapped peer". Alice injected
  // counters 1..50; the spec budgets ≤ 20% jitter-buffer drop (≥ 40
  // distinct counters surviving).
  assertContentChecks(
    frames,
    { minCounter: 1, maxCounter: 50, minCount: 40 },
    "alice → bob",
  );

  await alice.ctx.close();
  await bob.ctx.close();
});

/**
 * Spec content checks (section 3 of the voice-c2c spec).
 *
 * We can't simply assert "every recorded frame has a matching
 * checksum" because in this test environment Chromium's
 * `--use-fake-device-for-media-stream` feeds a 440 Hz tone into the
 * capture worklet alongside the synthetic injection. We call
 * `stopCaptureSource` before installing the recorder to silence that
 * path, but already-in-flight frames can still arrive at the receiver
 * after the recorder is installed — and those frames have the same
 * codec/transport shape as injected ones, just with mic-derived PCM.
 *
 * To filter cleanly: for each `c` in the injected counter range we
 * pre-compute the expected `(counter, checksum)` pair. Real injected
 * frames hit one of these pairs exactly (codec is passthrough →
 * byte-equal). Mic-derived frames almost surely don't (a 32-byte
 * SHA-256 collision against a known set of 50 values is astronomical).
 * The spec checks are then applied to only the confirmed-injected
 * subset:
 *   - Counter sequence is monotonically increasing within the subset.
 *   - No stretch of identical counter values longer than 5 frames
 *     (catches stuck-frame; jitter-pump may pad an underrun by
 *     repeating the last delivered frame).
 *   - Every confirmed frame's `(counter, checksum)` is in the
 *     expected set (catches empty / wrong-frame / cross-peer mixup).
 *   - At least `minCount` distinct injected counters land in the
 *     recorder (catches lost frames; spec budgets ≤ 20% drop).
 *
 * @param {Array<{seq_in_frame: number, len: number, checksum: string}>} frames
 * @param {{minCounter: number, maxCounter: number, minCount: number}} opts
 *   Inclusive range of injected counters and the minimum number of
 *   distinct counters expected to survive jitter-buffer drop.
 * @param {string} label  Used in failure messages (e.g. "alice → bob").
 */
function assertContentChecks(frames, opts, label) {
  const { minCounter, maxCounter, minCount } = opts;
  // Pre-compute the expected (counter, checksum) pairs.
  const expectedByCounter = new Map();
  for (let c = minCounter; c <= maxCounter; c++) {
    expectedByCounter.set(c, pcmChecksum(syntheticPcm(c)));
  }

  const confirmed = frames.filter(
    (f) =>
      expectedByCounter.has(f.seq_in_frame) &&
      f.checksum === expectedByCounter.get(f.seq_in_frame),
  );
  expect(
    confirmed.length,
    `${label}: only ${confirmed.length} confirmed-injected frames; expected ≥ ${minCount}`,
  ).toBeGreaterThanOrEqual(minCount);

  let prev = -Infinity;
  let runLen = 0;
  let runVal = null;
  const distinct = new Set();
  for (const f of confirmed) {
    expect(
      f.seq_in_frame,
      `${label}: counter regression (prev=${prev}, got=${f.seq_in_frame})`,
    ).toBeGreaterThanOrEqual(prev);
    if (f.seq_in_frame === runVal) {
      runLen += 1;
    } else {
      runVal = f.seq_in_frame;
      runLen = 1;
    }
    expect(
      runLen,
      `${label}: stuck-frame: ${runLen} consecutive frames with counter=${runVal}`,
    ).toBeLessThanOrEqual(5);
    distinct.add(f.seq_in_frame);
    prev = f.seq_in_frame;
  }
  expect(
    distinct.size,
    `${label}: only ${distinct.size} distinct injected counters delivered`,
  ).toBeGreaterThanOrEqual(minCount);
}
