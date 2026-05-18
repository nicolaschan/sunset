// voice_bar_placement.spec.js — the in-call voice bar lives at the
// top of the chat panel on both desktop and phone.
//
// Pre-refactor the desktop UI rendered a separate "self-controls" bar
// pinned to the bottom of the channels column, while the phone used a
// minibar across the top of the chat panel. This test pins the new
// unified behaviour: on desktop, once the user joins voice, the
// voice-minibar must appear inside the main panel, between the channel
// header and the messages list.

import { test, expect, devices } from "@playwright/test";
import {
  spawnRelay,
  teardownRelay,
  freshSeedHex,
  installVoiceFfi,
} from "./helpers/voice.js";

let relay;
test.beforeAll(async () => {
  relay = await spawnRelay();
});
test.afterAll(async () => {
  teardownRelay(relay);
});

async function openDesktopPeer(browser, relayAddr) {
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
  await ctx.addInitScript((seed) => {
    localStorage.setItem("sunset/identity-seed", seed);
  }, freshSeedHex());
  await installVoiceFfi(page);
  await page.goto(`/?relay=${encodeURIComponent(relayAddr)}#voice-test-room`);
  await page.waitForFunction(() => !!window.sunsetClient, null, {
    timeout: 15_000,
  });
  return { page, ctx };
}

test("desktop: voice minibar renders at the top of the chat panel when in a call", async ({
  browser,
}, testInfo) => {
  // The phone path is exercised by voice_two_way.spec.js (which opens
  // the channels drawer first). This test pins the desktop placement
  // specifically — the rail is already visible, no drawer to open.
  test.skip(
    testInfo.project.name === "mobile-chrome",
    "desktop-only placement test (phone path covered in voice_two_way.spec.js)",
  );

  const alice = await openDesktopPeer(browser, relay.addr);

  // Before joining: no minibar — and the channels rail's last child
  // must NOT be a fixed self-controls bar (the bar at the column
  // bottom moved out as part of this refactor).
  await expect(
    alice.page.locator('[data-testid="voice-minibar"]'),
  ).not.toBeVisible();
  const railLastChildHeight = await alice.page.evaluate(() => {
    const rail = document.querySelector('[data-testid="channels-rail"]');
    const last = rail.children[rail.children.length - 1];
    return last.getBoundingClientRect().height;
  });
  // A scrollable list takes most of the rail's height; a fixed
  // self-controls bar used to be 64 px. Pin the absence by asserting
  // the last child is much taller than that pre-refactor row.
  expect(railLastChildHeight).toBeGreaterThan(200);

  // Join voice from the channels rail.
  await alice.page.locator('[data-testid="voice-channel-row"]').first().click();

  // The minibar must appear inside the <main> column (i.e. the chat
  // panel), positioned below the channel header and above the
  // messages list.
  const minibar = alice.page.locator('[data-testid="voice-minibar"]');
  await expect(minibar).toBeVisible({ timeout: 2_000 });

  const placement = await alice.page.evaluate(() => {
    const main = document.querySelector("main");
    const bar = document.querySelector('[data-testid="voice-minibar"]');
    const channelHeader = main.firstElementChild;
    const messagesList = document.querySelector(
      '[data-testid="messages-list"]',
    );
    return {
      barInMain: !!main && main.contains(bar),
      barTop: bar.getBoundingClientRect().top,
      headerBottom: channelHeader.getBoundingClientRect().bottom,
      messagesTop: messagesList.getBoundingClientRect().top,
      barRectLeft: bar.getBoundingClientRect().left,
      mainRectLeft: main.getBoundingClientRect().left,
      barRectRight: bar.getBoundingClientRect().right,
      mainRectRight: main.getBoundingClientRect().right,
    };
  });

  // Lives inside <main>, not in the channels rail.
  expect(placement.barInMain).toBe(true);
  // Sits below the channel header (the small "# voice-channel" row).
  expect(placement.barTop).toBeGreaterThanOrEqual(placement.headerBottom - 1);
  // Sits above the messages list.
  expect(placement.barTop).toBeLessThanOrEqual(placement.messagesTop + 1);
  // Spans the full width of the chat panel.
  expect(Math.abs(placement.barRectLeft - placement.mainRectLeft)).toBeLessThanOrEqual(
    1,
  );
  expect(
    Math.abs(placement.barRectRight - placement.mainRectRight),
  ).toBeLessThanOrEqual(1);

  // Tapping the channel-name button on the minibar opens the user's
  // own voice popover — the new home for send-quality / per-self
  // volume. Denoise is per-peer (applied locally to incoming streams)
  // so it doesn't appear on the self row.
  await alice.page
    .getByRole("button", { name: /Voice controls for/i })
    .click();
  await expect(
    alice.page.locator('[data-testid="voice-popover"]'),
  ).toBeVisible({ timeout: 1_000 });
  await expect(
    alice.page.locator('[data-testid="voice-popover-denoise"]'),
  ).toHaveCount(0);

  // Leaving the call removes the minibar from the chat panel.
  await alice.page.locator('[data-testid="voice-popover-close"]').click();
  await alice.page.locator('[data-testid="voice-leave"]').click();
  await expect(minibar).not.toBeVisible({ timeout: 2_000 });

  await alice.ctx.close();
});
