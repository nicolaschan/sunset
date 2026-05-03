// Reactions e2e: one tab sends a message, opens the quick-picker via
// the "React" toolbar button, picks 👍, and the reaction chip appears
// on the message row.
//
// Single-browser (single context) — cross-peer propagation is covered
// by the Rust two_peer_reaction integration test; this e2e verifies the
// full FE loop: send → react toolbar → picker → chip renders.

import { test, expect } from "@playwright/test";
import { spawn } from "child_process";
import { mkdtempSync, rmSync } from "fs";
import { tmpdir } from "os";
import { join } from "path";

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

test("react button opens quick-picker and chip appears after picking", async ({
  browser,
}, testInfo) => {
  // The reaction toolbar and picker are desktop-only in the current
  // implementation (main_panel.gleam renders the picker only on Desktop
  // viewport). Skip on mobile-chrome.
  test.skip(
    testInfo.project.name === "mobile-chrome",
    "reaction picker is desktop-only in this implementation",
  );

  const url = `/?relay=${encodeURIComponent(relayAddress)}#sunset-demo`;
  const ctx = await browser.newContext();
  const page = await ctx.newPage();

  page.on("pageerror", (err) =>
    process.stderr.write(`[pageerror] ${err.stack || err}\n`),
  );
  page.on("console", (msg) => {
    if (msg.type() === "error") {
      process.stderr.write(`[console error] ${msg.text()}\n`);
    }
  });

  await page.goto(url);

  // Wait for the app shell to mount.
  await expect(page.getByText("sunset", { exact: true })).toBeVisible({
    timeout: 15_000,
  });

  const input = page.getByPlaceholder(/^Message #/);
  await expect(input).toBeVisible({ timeout: 15_000 });

  // Send a message.
  const msgText = `reactions e2e — ${Date.now()}`;
  await input.fill(msgText);
  await input.press("Enter");

  // The message row must appear.
  const msgRow = page.locator(".msg-row", { hasText: msgText }).first();
  await expect(msgRow).toBeVisible({ timeout: 15_000 });

  // Hover the message row to reveal the action toolbar. The toolbar is
  // CSS-hidden (opacity 0) until the row is hovered; use `force: true`
  // when clicking the button because hover-reveal via CSS may not
  // respond to Playwright's synthetic pointer events in all rendering
  // modes — clicking with force bypasses the visibility check and lets
  // us drive the button directly.
  const reactButton = msgRow.getByTitle("React");
  await reactButton.click({ force: true });

  // The quick-picker should now be visible.
  const picker = page.locator('[data-testid="reaction-picker"]');
  await expect(picker).toBeVisible({ timeout: 5_000 });

  // Pick the 👍 emoji (first quick-reaction button inside the picker).
  const thumbsUpButton = picker.getByTitle("React with 👍");
  await thumbsUpButton.click();

  // The picker should close after picking.
  await expect(picker).not.toBeVisible({ timeout: 5_000 });

  // A reaction chip containing 👍 and the count "1" should appear on
  // the message row.
  const chip = msgRow.locator("span").filter({ hasText: /👍/ }).first();
  await expect(chip).toBeVisible({ timeout: 10_000 });
  // The count badge (a nested span) must show "1".
  await expect(chip.locator("span")).toHaveText("1", { timeout: 5_000 });

  await ctx.close();
});
