// Acceptance test for the Relays rail and popover.
//
// Spawns a real sunset-relay, points one browser at it, and asserts:
//   * Relays section + row appear with the correct hostname.
//   * Row state attribute reaches "connected".
//   * Click opens the popover (hostname, status, heard-from, RTT, label).
//   * Live RTT + heartbeat-age update once a Pong round-trips
//     (heartbeat_interval_ms=2000 keeps the test under 15 s).
//   * On phone viewport, the popover renders inside the bottom sheet.

import { test, expect } from "@playwright/test";
import { spawn } from "child_process";
import { mkdtempSync, rmSync } from "fs";
import { tmpdir } from "os";
import { join } from "path";
import { openChannelsDrawer } from "./helpers/viewport.js";

let relayProcess = null;
let relayAddress = null;
let relayDataDir = null;

test.beforeAll(async () => {
  relayDataDir = mkdtempSync(join(tmpdir(), "sunset-relay-relays-"));
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
    relayProcess.stderr.on("data", (chunk) =>
      process.stderr.write(`[relay] ${chunk}`),
    );
    relayProcess.on("error", (e) => {
      clearTimeout(timer);
      reject(e);
    });
    relayProcess.on("exit", (code) => {
      if (code !== null && code !== 0) {
        clearTimeout(timer);
        reject(new Error(`relay exited prematurely (code ${code})`));
      }
    });
  });
});

test.afterAll(async () => {
  if (relayProcess && relayProcess.exitCode === null) {
    relayProcess.kill("SIGTERM");
  }
  if (relayDataDir) {
    rmSync(relayDataDir, { recursive: true, force: true });
  }
});

const buildUrl = () =>
  `/?relay=${encodeURIComponent(relayAddress)}` +
  `&heartbeat_interval_ms=2000` +
  `#sunset-relays`;

// Compute the host:port the rail row will display, matching what
// `relays.parse_host` produces from the relay's `ws://127.0.0.1:NNN` URL.
function expectedHost() {
  const u = new URL(relayAddress);
  return u.port ? `${u.hostname}:${u.port}` : u.hostname;
}

test.setTimeout(45_000);

test("relay row appears, popover opens, live metrics update", async ({ page }, testInfo) => {
  page.on("pageerror", (err) =>
    process.stderr.write(`[pageerror] ${err.stack || err}\n`),
  );

  await page.goto(buildUrl());

  // On phone the channels rail lives behind a drawer; open it before
  // asserting on the relays section. No-op on desktop.
  await openChannelsDrawer(page, testInfo);

  // The Relays section is hidden when empty; wait for it to materialise.
  await expect(page.locator('[data-testid="relays-section"]')).toBeVisible({
    timeout: 10_000,
  });

  const row = page.locator(
    `[data-testid="relay-row"][data-relay-host="${expectedHost()}"]`,
  );
  await expect(row).toBeVisible();
  await expect(row).toHaveAttribute("data-relay-state", "connected", {
    timeout: 10_000,
  });

  await row.click();

  const popover = page.locator('[data-testid="relay-popover"]');
  await expect(popover).toBeVisible();
  await expect(
    popover.locator('[data-testid="relay-popover-status"]'),
  ).toHaveText("Connected");
  await expect(
    popover.locator('[data-testid="relay-popover-label"]'),
  ).toContainText(relayAddress);

  // Within ~6 s (3 × heartbeat_interval_ms=2000) we should see RTT.
  const rtt = popover.locator('[data-testid="relay-popover-rtt"]');
  await expect(rtt).toHaveText(/^RTT \d+ ms$/, { timeout: 8_000 });
  const heard = popover.locator('[data-testid="relay-popover-heard-from"]');
  await expect(heard).toHaveText(/^heard from (just now|\d+s ago)$/, {
    timeout: 8_000,
  });

  // On phone, the popover lives inside the bottom-sheet host. (mobile-chrome
  // project applies a Pixel 7 viewport automatically.)
  if (testInfo.project.name.startsWith("mobile")) {
    const sheetAncestor = popover.locator(
      'xpath=ancestor::*[@data-testid="relay-sheet"]',
    );
    await expect(sheetAncestor).toHaveCount(1);
  }

  // Close button works.
  await popover.locator('[data-testid="relay-popover-close"]').click();
  await expect(popover).toBeHidden();
});

test("desktop relay popover docks next to the channels rail, not the right column", async ({
  page,
}, testInfo) => {
  test.skip(
    testInfo.project.name === "mobile-chrome",
    "phone uses the relay-sheet bottom sheet instead",
  );
  page.on("pageerror", (err) =>
    process.stderr.write(`[pageerror] ${err.stack || err}\n`),
  );

  await page.goto(buildUrl());
  await expect(page.locator('[data-testid="relays-section"]')).toBeVisible({
    timeout: 10_000,
  });

  const row = page.locator(
    `[data-testid="relay-row"][data-relay-host="${expectedHost()}"]`,
  );
  await expect(row).toBeVisible();
  await row.click();

  const popover = page.locator('[data-testid="relay-popover"]');
  await expect(popover).toBeVisible();

  // The popover's left edge should be just to the right of the
  // relays listing (which lives at the bottom of the channels rail).
  // Pre-fix the popover was anchored to `right: 260px`, putting it
  // past the main chat column on the far right of the viewport —
  // visually disconnected from the row that triggered it. We assert:
  // the popover sits to the right of the relays section but in the
  // left half of the viewport, and doesn't run off-screen.
  const rects = await page.evaluate(() => {
    const popoverEl = document.querySelector('[data-testid="relay-popover"]');
    const sectionEl = document.querySelector('[data-testid="relays-section"]');
    return {
      popover: popoverEl.getBoundingClientRect(),
      section: sectionEl.getBoundingClientRect(),
      vw: window.innerWidth,
    };
  });
  expect(rects.popover.left).toBeGreaterThanOrEqual(rects.section.right);
  expect(rects.popover.left).toBeLessThan(rects.vw / 2);
  expect(rects.popover.right).toBeLessThan(rects.vw);
});
