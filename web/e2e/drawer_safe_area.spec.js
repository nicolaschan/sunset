// iOS PWA safe-area regression test for the drawer primitive.
//
// In standalone "Add to Home Screen" mode on iOS — combined with the
// `apple-mobile-web-app-status-bar-style: black-translucent` meta —
// the page extends under the iOS status bar and the home indicator.
// Without explicit `padding-top: env(safe-area-inset-top)` on the
// drawer wrapper, the drawer's first row (rooms brand, channels
// header, etc.) sits behind the status bar and can't receive taps.
//
// Playwright doesn't directly simulate the iOS safe-area insets, so
// this file does two things:
//   1. Contract test: the drawer wrapper's inline style declares the
//      env(safe-area-inset-*) padding on both axes. If someone strips
//      it the contract test fails — and that's the only signal a
//      non-notch CI run can give us.
//   2. Layout test on a real viewport (no notch, env() resolves to 0):
//      the drawer's content fits inside the drawer's clipping box and
//      doesn't overflow vertically. This catches the case where the
//      inner rail's height is bare 100dvh and overflows past the
//      drawer's safe-area-padded content area on PWA runtimes.

import { spawn } from "child_process";
import { mkdtempSync, rmSync } from "fs";
import { tmpdir } from "os";
import { join } from "path";

import { expect, test } from "@playwright/test";

let relayProcess = null;
let relayAddress = null;
let relayDataDir = null;

test.beforeAll(async () => {
  relayDataDir = mkdtempSync(join(tmpdir(), "sunset-relay-drawer-test-"));
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
    relayProcess.on("error", (e) => {
      clearTimeout(timer);
      reject(e);
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

test.setTimeout(60_000);

async function openChat(browser, hash = "drawer-safe-area") {
  const url = `/?relay=${encodeURIComponent(relayAddress)}#${hash}`;
  const ctx = await browser.newContext();
  const page = await ctx.newPage();
  page.on("pageerror", (err) =>
    process.stderr.write(`[pageerror] ${err.stack || err}\n`),
  );
  page.on("console", (msg) => {
    if (msg.type() === "error") {
      process.stderr.write(`[console] ${msg.text()}\n`);
    }
  });
  await page.addInitScript(() => {
    window.SUNSET_TEST = true;
  });
  await page.goto(url);
  await expect(page.getByText("sunset", { exact: true })).toBeVisible({
    timeout: 15_000,
  });
  return { ctx, page };
}

const skipDesktop = (testInfo) =>
  test.skip(
    testInfo.project.name !== "mobile-chrome",
    "drawers only render on the phone viewport",
  );

test("drawer wrapper declares iOS safe-area padding on both axes", async ({
  browser,
}, testInfo) => {
  skipDesktop(testInfo);
  const { ctx, page } = await openChat(browser, "drawer-contract");

  for (const id of ["channels-drawer", "rooms-drawer", "members-drawer"]) {
    const style = await page.getByTestId(id).getAttribute("style");
    // The contract: each drawer wrapper carries the env() padding so
    // its content shifts below the iOS status bar / above the home
    // indicator in PWA standalone mode. These assertions don't depend
    // on the runner's actual notch — env() resolves to 0 on desktop /
    // emulated viewports — they regression-gate the styling rule.
    expect(style, `drawer ${id} missing safe-area padding`).toMatch(
      /padding-top:\s*env\(safe-area-inset-top\)/,
    );
    expect(style, `drawer ${id} missing safe-area padding`).toMatch(
      /padding-bottom:\s*env\(safe-area-inset-bottom\)/,
    );
  }

  await ctx.close();
});

// Open a single drawer of `kind` and snapshot the rail bottom + the
// drawer bottom. Used by the layout-bound test to verify the rail
// fits inside the drawer's clipping box for each of the three kinds
// the shell renders.
async function openSingleDrawerAndMeasure(browser, kind) {
  const { ctx, page } = await openChat(browser, "drawer-layout-" + kind);

  if (kind === "channels") {
    await page.getByTestId("phone-rooms-toggle").click();
  } else if (kind === "members") {
    await page.getByTestId("phone-members-toggle").click();
  } else if (kind === "rooms") {
    // Rooms drawer is reachable only by swapping from the channels
    // drawer's room-title button (this is the only entry point on phone).
    await page.getByTestId("phone-rooms-toggle").click();
    await page.getByTestId("channels-room-title").click();
  }

  const drawerId = kind + "-drawer";
  const drawer = page.getByTestId(drawerId);
  await expect(drawer).toHaveAttribute("aria-hidden", "false");

  const dims = await drawer.evaluate((el) => {
    const rail = el.firstElementChild;
    const dr = el.getBoundingClientRect();
    const rr = rail.getBoundingClientRect();
    return { railBottom: rr.bottom, drawerBottom: dr.bottom };
  });

  await ctx.close();
  return dims;
}

test("channels drawer content fits inside the drawer's clipping box", async ({
  browser,
}, testInfo) => {
  skipDesktop(testInfo);
  // The rail's height should resolve against the drawer's content
  // box (which is padded by env(safe-area-inset-*)), not the raw
  // viewport. Without that, on iOS PWA mode the rail overflows by
  // `safe_top + safe_bottom` and the bottom row hides behind the
  // home indicator. Even at zero inset (Playwright's default) a
  // regression that uses bare `100dvh` shows up as an overflow.
  const dims = await openSingleDrawerAndMeasure(browser, "channels");
  expect(dims.railBottom).toBeLessThanOrEqual(dims.drawerBottom + 2);
});

test("rooms drawer content fits inside the drawer's clipping box", async ({
  browser,
}, testInfo) => {
  skipDesktop(testInfo);
  const dims = await openSingleDrawerAndMeasure(browser, "rooms");
  expect(dims.railBottom).toBeLessThanOrEqual(dims.drawerBottom + 2);
});

test("members drawer content fits inside the drawer's clipping box", async ({
  browser,
}, testInfo) => {
  skipDesktop(testInfo);
  const dims = await openSingleDrawerAndMeasure(browser, "members");
  expect(dims.railBottom).toBeLessThanOrEqual(dims.drawerBottom + 2);
});

test("rooms-rail you-row stays inside the drawer's tappable area", async ({
  browser,
}, testInfo) => {
  skipDesktop(testInfo);
  // The you-row (settings trigger) sits at the bottom of the rooms
  // rail. Before the safe-area fix, with iOS home-indicator inset the
  // row was clipped behind the indicator and couldn't be tapped. This
  // test asserts the row's centre is within the drawer's bounding box
  // even at zero inset — the contract test above guards the env()
  // rule, this one guards the layout's robustness against the rule
  // ever expanding to non-zero values.
  const { ctx, page } = await openChat(browser, "drawer-you-row");

  await page.getByTestId("phone-rooms-toggle").click();
  await page.getByTestId("channels-room-title").click();
  const drawer = page.getByTestId("rooms-drawer");
  await expect(drawer).toHaveAttribute("aria-hidden", "false");

  const youRow = page.getByTestId("you-row");
  await expect(youRow).toBeVisible();

  const { youCenter, drawerBottom } = await page.evaluate(() => {
    const y = document.querySelector('[data-testid="you-row"]');
    const d = document.querySelector('[data-testid="rooms-drawer"]');
    const yr = y.getBoundingClientRect();
    const dr = d.getBoundingClientRect();
    return {
      youCenter: yr.top + yr.height / 2,
      drawerBottom: dr.bottom,
    };
  });
  // The you-row's centre point must be tappable — i.e., inside the
  // drawer's bottom edge. (Drawer's overflow:hidden would clip
  // anything past that.)
  expect(youCenter).toBeLessThan(drawerBottom);

  await ctx.close();
});
