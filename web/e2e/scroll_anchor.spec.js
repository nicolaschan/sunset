// Auto-scroll-to-bottom behavior: when a new message arrives, the
// chat scrolls to the bottom only if the user was already near the
// bottom. If they've scrolled up to read history, the view stays put.
//
// Driven by `scroll_anchor.ffi.mjs`. The relay isn't required for
// this — sending a message locally inserts it in our own store, the
// `on_message` Replay::All subscription delivers it back as an
// IncomingMsg, and the resulting DOM mutation triggers the same
// scroll-anchor logic that a remote message would.

import { spawn } from "child_process";
import { mkdtempSync, rmSync } from "fs";
import { tmpdir } from "os";
import { join } from "path";

import { expect, test } from "@playwright/test";

let relayProcess = null;
let relayAddress = null;
let relayDataDir = null;

test.beforeAll(async () => {
  relayDataDir = mkdtempSync(join(tmpdir(), "sunset-relay-test-"));
  const configPath = join(relayDataDir, "relay.toml");
  const fs = await import("fs/promises");
  await fs.writeFile(
    configPath,
    [
      `listen_addr = "127.0.0.1:0"`,
      `data_dir = "${relayDataDir}"`,
      `interest_filter = "all"`,
      `identity_secret = "auto"`,
      `peers = []`,
      "",
    ].join("\n"),
  );
  relayProcess = spawn("sunset-relay", ["--config", configPath], {
    stdio: ["ignore", "pipe", "pipe"],
  });
  relayAddress = await new Promise((resolve, reject) => {
    const timer = setTimeout(
      () => reject(new Error("relay didn't print address banner within 15s")),
      15_000,
    );
    let buffer = "";
    relayProcess.stdout.on("data", (chunk) => {
      buffer += chunk.toString();
      const m = buffer.match(/address:\s+(ws:\/\/[^\s]+)/);
      if (m) {
        clearTimeout(timer);
        resolve(m[1]);
      }
    });
    relayProcess.stderr.on("data", (chunk) => {
      process.stderr.write(`[relay] ${chunk}`);
    });
    relayProcess.on("exit", (code) => {
      if (code !== null && code !== 0) {
        clearTimeout(timer);
        reject(new Error(`relay exited prematurely with code ${code}`));
      }
    });
  });
});

test.afterAll(() => {
  if (relayProcess && relayProcess.exitCode === null) {
    relayProcess.kill("SIGTERM");
  }
  if (relayDataDir) {
    rmSync(relayDataDir, { recursive: true, force: true });
  }
});

// Distance from the bottom counted as "still at bottom" by scroll_anchor.
const NEAR_BOTTOM_PX = 80;

test("auto-scrolls to bottom when at bottom; preserves position when scrolled up", async ({
  browser,
}) => {
  const url = `/?relay=${encodeURIComponent(relayAddress)}#sunset-demo`;
  const ctx = await browser.newContext();
  const page = await ctx.newPage();

  page.on("pageerror", (err) =>
    process.stderr.write(`[pageerror] ${err.stack || err}\n`),
  );

  await page.goto(url);
  await expect(page.getByText("sunset", { exact: true })).toBeVisible({
    timeout: 15_000,
  });

  const input = page.getByPlaceholder(/^Message #/);
  await expect(input).toBeVisible({ timeout: 15_000 });

  // Send enough messages that the scroll area overflows. Each message
  // is multi-line so we don't need 50 sends to overflow the column.
  const FILLER_LINES = "\nbuilding up the backlog so the column overflows.\n.";
  for (let i = 0; i < 30; i++) {
    await input.fill(`message ${i}${FILLER_LINES}`);
    await input.press("Enter");
  }

  // Wait for the last message to actually render.
  await expect(page.getByText("message 29")).toBeVisible({ timeout: 15_000 });

  const scrollArea = page.locator(".scroll-area").first();

  // After the burst, the user should be anchored at the bottom.
  // The anchor uses requestAnimationFrame to schedule the scroll, so
  // poll rather than reading once.
  await expect
    .poll(
      async () =>
        scrollArea.evaluate(
          (el, near) =>
            el.scrollHeight - (el.scrollTop + el.clientHeight) <= near,
          NEAR_BOTTOM_PX,
        ),
      { timeout: 5_000 },
    )
    .toBe(true);

  // Two-frame helper. The scroll-anchor logic schedules its scroll-to-
  // bottom via `requestAnimationFrame`, so to know it has had its
  // chance to run (or to deliberately decline to run) we yield two
  // animation frames — one for the MutationObserver microtask to land,
  // one for the rAF callback itself.
  const settleAnchor = () =>
    page.evaluate(
      () =>
        new Promise((r) =>
          requestAnimationFrame(() => requestAnimationFrame(r)),
        ),
    );

  // Now simulate the user scrolling up to read history. The anchor
  // updates its at-bottom flag only on user-intent events (wheel /
  // touchmove / scroll-keys), so dispatch a wheel event alongside the
  // programmatic scroll to mirror what a real wheel scroll does.
  await scrollArea.evaluate((el) => {
    el.scrollTop = 0;
    el.dispatchEvent(
      new WheelEvent("wheel", { deltaY: -2000, bubbles: true, cancelable: true }),
    );
  });

  const scrollTopBeforeNew = await scrollArea.evaluate((el) => el.scrollTop);
  expect(scrollTopBeforeNew).toBeLessThan(NEAR_BOTTOM_PX);

  // Send a new message. Since the user is no longer near the bottom,
  // scroll_anchor must preserve the current scrollTop.
  await input.fill("a new message arriving while user is reading history");
  await input.press("Enter");
  await expect(
    page.getByText("a new message arriving while user is reading history"),
  ).toBeVisible({ timeout: 15_000 });

  // Yield the two rAFs the scroll-anchor's mutation observer would
  // ride before it could move scrollTop. After this point, if the
  // anchor was going to scroll us back to the bottom, it has.
  await settleAnchor();

  const scrollTopAfterNew = await scrollArea.evaluate((el) => el.scrollTop);
  // The position should be roughly unchanged — at most a few pixels
  // off from layout shift, but nowhere near scrollHeight.
  expect(scrollTopAfterNew).toBeLessThan(NEAR_BOTTOM_PX * 4);

  // Now scroll back to the bottom and send another message. The
  // anchor should re-engage. Pair the programmatic scroll with a
  // wheel event so the anchor recognises user intent.
  await scrollArea.evaluate((el) => {
    el.scrollTop = el.scrollHeight;
    el.dispatchEvent(
      new WheelEvent("wheel", { deltaY: 2000, bubbles: true, cancelable: true }),
    );
  });

  await input.fill("after returning to bottom");
  await input.press("Enter");
  await expect(page.getByText("after returning to bottom")).toBeVisible({
    timeout: 15_000,
  });

  await expect
    .poll(
      async () =>
        scrollArea.evaluate(
          (el, near) => el.scrollHeight - (el.scrollTop + el.clientHeight) <= near,
          NEAR_BOTTOM_PX,
        ),
      { timeout: 5_000 },
    )
    .toBe(true);

  await ctx.close();
});
