// voice_echo_cancellation.spec.js — Independent echo-cancellation toggle.
//
// Asserts:
//   1. The toggle appears on the self-row voice popover with a sensible
//      initial state (no prior preference → derived from the default
//      quality preset, which is `"maximum"` → echo cancellation off).
//   2. Clicking the toggle persists the new value to
//      `localStorage["sunset/echo-cancellation"]` ("on" / "off") and
//      flips `aria-pressed` accordingly.
//   3. The toggle is independent of the quality preset: changing the
//      preset does not overwrite an explicit echo-cancellation choice,
//      and toggling EC does not change the preset.
//   4. The toggle is only shown for the self row (m.you === true). A
//      peer's popover renders the denoise toggle instead.
//
// What this does NOT verify:
//   - Whether the browser actually honors `echoCancellation: true` in
//     the `getUserMedia` constraints (Chromium under fake-audio doesn't
//     run real EC). That's a browser concern; the contract here is
//     that the constraint is plumbed through end-to-end.

import { test, expect, devices } from "@playwright/test";
import {
  spawnRelay,
  teardownRelay,
  freshSeedHex,
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

function uint8ToHex(arr) {
  return Array.from(arr)
    .map((b) => b.toString(16).padStart(2, "0"))
    .join("");
}

async function joinVoiceAndOpenSelfPopover(page) {
  await page.locator('[data-testid="voice-channel-row"]').first().click();
  await expect(page.locator('[data-testid="voice-leave"]')).toBeVisible({
    timeout: 5_000,
  });
  const selfBytes = await page.evaluate(() =>
    Array.from(new Uint8Array(window.sunsetClient.public_key)),
  );
  const selfHex = uint8ToHex(selfBytes);
  const selfRow = page.locator(
    `[data-testid="voice-member"][data-peer-hex="${selfHex}"]`,
  );
  await expect(selfRow).toBeVisible({ timeout: 5_000 });
  await selfRow.click();
  await expect(page.locator('[data-testid="voice-popover"]')).toBeVisible({
    timeout: 2_000,
  });
  return { selfHex };
}

test("echo cancellation defaults off (matches default `maximum` preset) and toggle is rendered on self row", async ({
  browser,
}, testInfo) => {
  test.skip(
    testInfo.project.name === "mobile-chrome",
    "wire-through covered by Desktop run",
  );

  const peer = await openPeer(browser, relay.addr);
  await joinVoiceAndOpenSelfPopover(peer.page);

  const ecBtn = peer.page.locator(
    '[data-testid="voice-popover-echo-cancellation"]',
  );
  await expect(ecBtn).toBeVisible({ timeout: 2_000 });
  // Default preset is `"maximum"`, so the derived EC default is off.
  await expect(ecBtn).toHaveAttribute("aria-pressed", "false");

  // The persisted FFI-side default agrees.
  const initial = await peer.page.evaluate(
    () => window.localStorage.getItem("sunset/echo-cancellation"),
  );
  expect(initial).toBeNull();

  await peer.ctx.close();
});

test("toggling echo cancellation persists to localStorage and flips aria-pressed", async ({
  browser,
}, testInfo) => {
  test.skip(
    testInfo.project.name === "mobile-chrome",
    "wire-through covered by Desktop run",
  );

  const peer = await openPeer(browser, relay.addr);
  await joinVoiceAndOpenSelfPopover(peer.page);

  const ecBtn = peer.page.locator(
    '[data-testid="voice-popover-echo-cancellation"]',
  );
  await expect(ecBtn).toHaveAttribute("aria-pressed", "false");

  // Flip on.
  await ecBtn.click();
  await expect(ecBtn).toHaveAttribute("aria-pressed", "true");
  await expect
    .poll(async () =>
      peer.page.evaluate(() =>
        window.localStorage.getItem("sunset/echo-cancellation"),
      ),
    )
    .toBe("on");

  // Flip off.
  await ecBtn.click();
  await expect(ecBtn).toHaveAttribute("aria-pressed", "false");
  await expect
    .poll(async () =>
      peer.page.evaluate(() =>
        window.localStorage.getItem("sunset/echo-cancellation"),
      ),
    )
    .toBe("off");

  await peer.ctx.close();
});

test("echo cancellation toggle is independent of the quality preset", async ({
  browser,
}, testInfo) => {
  test.skip(
    testInfo.project.name === "mobile-chrome",
    "wire-through covered by Desktop run",
  );

  const peer = await openPeer(browser, relay.addr);
  await joinVoiceAndOpenSelfPopover(peer.page);

  const ecBtn = peer.page.locator(
    '[data-testid="voice-popover-echo-cancellation"]',
  );

  // 1. Explicitly set EC on.
  await ecBtn.click();
  await expect(ecBtn).toHaveAttribute("aria-pressed", "true");
  await expect
    .poll(async () =>
      peer.page.evaluate(() =>
        window.localStorage.getItem("sunset/echo-cancellation"),
      ),
    )
    .toBe("on");

  // 2. Change the quality preset to `voice` via the radio button.
  //    EC should remain on (the explicit choice wins) and the preset
  //    key should be updated independently.
  await peer.page.locator('[data-testid="voice-popover-quality-voice"]').click();
  await expect
    .poll(async () =>
      peer.page.evaluate(() =>
        window.localStorage.getItem("sunset/voice-quality"),
      ),
    )
    .toBe("voice");
  await expect(ecBtn).toHaveAttribute("aria-pressed", "true");
  expect(
    await peer.page.evaluate(() =>
      window.localStorage.getItem("sunset/echo-cancellation"),
    ),
  ).toBe("on");

  // 3. Change preset to `maximum`. EC still stays on (independent).
  await peer.page
    .locator('[data-testid="voice-popover-quality-maximum"]')
    .click();
  await expect
    .poll(async () =>
      peer.page.evaluate(() =>
        window.localStorage.getItem("sunset/voice-quality"),
      ),
    )
    .toBe("maximum");
  await expect(ecBtn).toHaveAttribute("aria-pressed", "true");
  expect(
    await peer.page.evaluate(() =>
      window.localStorage.getItem("sunset/echo-cancellation"),
    ),
  ).toBe("on");

  // 4. Toggling EC off does not change the preset.
  await ecBtn.click();
  await expect(ecBtn).toHaveAttribute("aria-pressed", "false");
  expect(
    await peer.page.evaluate(() =>
      window.localStorage.getItem("sunset/voice-quality"),
    ),
  ).toBe("maximum");

  await peer.ctx.close();
});

test("echo cancellation toggle is not rendered on a remote peer's popover", async ({
  browser,
}, testInfo) => {
  test.skip(
    testInfo.project.name === "mobile-chrome",
    "wire-through covered by Desktop run",
  );

  const alice = await openPeer(browser, relay.addr);
  const bob = await openPeer(browser, relay.addr);

  await alice.page.locator('[data-testid="voice-channel-row"]').first().click();
  await bob.page.locator('[data-testid="voice-channel-row"]').first().click();
  await expect(alice.page.locator('[data-testid="voice-leave"]')).toBeVisible({
    timeout: 5_000,
  });
  await expect(bob.page.locator('[data-testid="voice-leave"]')).toBeVisible({
    timeout: 5_000,
  });

  const aliceBytes = await alice.page.evaluate(() =>
    Array.from(new Uint8Array(window.sunsetClient.public_key)),
  );
  const aliceHex = uint8ToHex(aliceBytes);

  // Open Alice's row from Bob's popover view — i.e. the peer (non-self) row.
  const aliceRow = bob.page.locator(
    `[data-testid="voice-member"][data-peer-hex="${aliceHex}"]`,
  );
  await expect(aliceRow).toBeVisible({ timeout: 5_000 });
  await aliceRow.click();
  await expect(bob.page.locator('[data-testid="voice-popover"]')).toBeVisible({
    timeout: 2_000,
  });

  // Peer row shows denoise but not echo cancellation.
  await expect(bob.page.locator('[data-testid="voice-popover-denoise"]')).toBeVisible();
  await expect(
    bob.page.locator('[data-testid="voice-popover-echo-cancellation"]'),
  ).toHaveCount(0);

  await alice.ctx.close();
  await bob.ctx.close();
});
