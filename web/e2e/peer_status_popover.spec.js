// Acceptance test for the peer status popover.
//
// Two browser contexts join the same room via a real relay. Each one
// publishes presence heartbeats. We click peer B's row in page A, assert
// the popover shows the expected transport label ("Via relay" since both
// go through the relay), and that the heartbeat-age readout matches the
// pattern "just now" / "Ns ago". After waiting ~3s we assert the age
// string changed (popover ticker keeps it live). Finally we click the
// close button and assert the popover is gone.

import { test, expect } from "@playwright/test";
import { spawn } from "child_process";
import { mkdtempSync, rmSync } from "fs";
import { tmpdir } from "os";
import { join } from "path";
import { openMembersDrawer } from "./helpers/viewport.js";

let relayProcess = null;
let relayAddress = null;
let relayDataDir = null;

test.beforeAll(async () => {
  relayDataDir = mkdtempSync(join(tmpdir(), "sunset-relay-peerstatus-"));

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

test.afterAll(async () => {
  if (relayProcess && relayProcess.exitCode === null) {
    relayProcess.kill("SIGTERM");
  }
  if (relayDataDir) {
    rmSync(relayDataDir, { recursive: true, force: true });
  }
});

test.setTimeout(60_000);
test("clicking a peer row opens the status popover with live age", async ({ browser }, testInfo) => {
  // Use a faster presence cadence so the test sees a heartbeat and an
  // age tick within the test timeout.
  const url = `/?relay=${encodeURIComponent(relayAddress)}` +
              `&presence_interval=2000` +
              `&presence_ttl=10000` +
              `&presence_refresh=1000` +
              `#sunset-peerstatus`;

  const ctxA = await browser.newContext();
  const ctxB = await browser.newContext();
  const pageA = await ctxA.newPage();
  const pageB = await ctxB.newPage();

  for (const [name, page] of [["A", pageA], ["B", pageB]]) {
    page.on("pageerror", (err) =>
      process.stderr.write(`[${name} pageerror] ${err.stack || err}\n`),
    );
    page.on("console", (msg) => {
      if (msg.type() === "error") {
        process.stderr.write(`[${name} console] ${msg.text()}\n`);
      }
    });
  }

  await pageA.goto(url);
  await pageB.goto(url);

  // Wait for the chat shell + at least one non-self member row on page A.
  await expect(pageA.getByText("sunset", { exact: true })).toBeVisible({
    timeout: 15_000,
  });
  await expect(pageB.getByText("sunset", { exact: true })).toBeVisible({
    timeout: 15_000,
  });

  // Page A should eventually see TWO member rows (self + B).
  await expect(pageA.locator('[data-testid="member-row"]')).toHaveCount(2, {
    timeout: 20_000,
  });

  // On mobile-chrome the members rail lives behind a drawer; tapping
  // phone-members-toggle is the real-user path to reaching member rows.
  // No-op on desktop.
  await openMembersDrawer(pageA, testInfo);

  // Identify the non-self row (the second one, since self is sorted first).
  const otherRow = pageA.locator('[data-testid="member-row"]').nth(1);

  // Click it to open the popover.
  await otherRow.click();

  const popover = pageA.locator('[data-testid="peer-status-popover"]');
  await expect(popover).toBeVisible({ timeout: 5_000 });

  // Should show "Via relay" since both browsers go through the relay.
  await expect(popover).toContainText("Via relay");

  // Wait until a heartbeat from B has been observed and rendered.
  // With presence_interval=2000 the first heartbeat from B should land
  // within a few seconds. Until then the popover may show "never".
  await expect(popover).toContainText(/heard from (just now|\d+s ago)/, {
    timeout: 15_000,
  });

  // Should show an age string.
  const initialText = await popover.textContent();
  expect(initialText).toMatch(/heard from (just now|\d+s ago|\d+m ago)/);

  // Poll until the age readout changes — the popover ticker bumps
  // `now_ms` every 1 s and re-renders, so any later snapshot must
  // differ from the initial one. 5 s budget covers the worst-case
  // case where the initial sample landed 0.99 s into a tick and the
  // next visible flip happens after 1.01 s of waiting plus jitter.
  await expect
    .poll(async () => await popover.textContent(), { timeout: 5_000 })
    .not.toBe(initialText);

  // Close button works.
  await pageA.locator('[data-testid="peer-status-popover-close"]').click();
  await expect(popover).toHaveCount(0);

  await ctxA.close();
  await ctxB.close();
});
