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

  // A reaction pill containing 👍 and the count "1" should appear on
  // the message row.
  const pill = msgRow.locator(
    '[data-testid="reaction-pill"][data-emoji="👍"]',
  );
  await expect(pill).toBeVisible({ timeout: 10_000 });
  // The count badge (a nested span) must show "1".
  await expect(pill.locator("span")).toHaveText("1", { timeout: 5_000 });

  await ctx.close();
});

// Slack/Discord-style pill toggle: once a reaction exists on a message,
// clicking the pill itself adds-or-removes the user's own reaction.
// Skipped on mobile-chrome because the picker flow used to seed the
// reaction is desktop-only in this implementation (matches the skip on
// the picker test above). The click handler on the pill itself is the
// same `<button>` on both viewports — a mobile user with a pill on a
// message would tap it the same way.
test("clicking own reaction pill removes the reaction", async ({
  browser,
}, testInfo) => {
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

  await expect(page.getByText("sunset", { exact: true })).toBeVisible({
    timeout: 15_000,
  });

  const input = page.getByPlaceholder(/^Message #/);
  await expect(input).toBeVisible({ timeout: 15_000 });

  const msgText = `pill-toggle e2e — ${Date.now()}`;
  await input.fill(msgText);
  await input.press("Enter");

  const msgRow = page.locator(".msg-row", { hasText: msgText }).first();
  await expect(msgRow).toBeVisible({ timeout: 15_000 });

  // Open quick-picker and seed a 👍 reaction (force=true bypasses the
  // hover-only visibility on the toolbar — same pattern as the picker
  // test above).
  await msgRow.getByTitle("React").click({ force: true });
  const picker = page.locator('[data-testid="reaction-picker"]');
  await expect(picker).toBeVisible({ timeout: 5_000 });
  await picker.getByTitle("React with 👍").click();
  await expect(picker).not.toBeVisible({ timeout: 5_000 });

  // The pill is now present, marked as our own reaction (aria-pressed),
  // and shows count 1.
  const pill = msgRow.locator(
    '[data-testid="reaction-pill"][data-emoji="👍"]',
  );
  await expect(pill).toBeVisible({ timeout: 10_000 });
  await expect(pill).toHaveAttribute("aria-pressed", "true");
  await expect(pill.locator("span")).toHaveText("1", { timeout: 5_000 });

  // Click the pill → engine sends "remove" → ReactionsChanged drops
  // our pubkey from the snapshot → snapshot_to_reactions filters out
  // the now-zero-count entry → the pill is no longer rendered.
  await pill.click();
  await expect(pill).toHaveCount(0, { timeout: 10_000 });

  await ctx.close();
});

test("pill click toggles only the reaction, not row selection", async ({
  browser,
}, testInfo) => {
  // Pills sit inside the message body's click target (which toggles
  // row selection). Without event.stop_propagation a pill click would
  // both toggle the reaction AND toggle selection — leaking UI state.
  // This test pins selection via the action toolbar and verifies a
  // pill click preserves it.
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

  await expect(page.getByText("sunset", { exact: true })).toBeVisible({
    timeout: 15_000,
  });

  const input = page.getByPlaceholder(/^Message #/);
  await expect(input).toBeVisible({ timeout: 15_000 });

  const msgText = `pill-selection e2e — ${Date.now()}`;
  await input.fill(msgText);
  await input.press("Enter");

  const msgRow = page.locator(".msg-row", { hasText: msgText }).first();
  await expect(msgRow).toBeVisible({ timeout: 15_000 });

  // Seed a 👍 via picker (this also pins selection, but go via tap
  // explicitly for clarity).
  await msgRow.getByTitle("React").click({ force: true });
  const picker = page.locator('[data-testid="reaction-picker"]');
  await expect(picker).toBeVisible({ timeout: 5_000 });
  await picker.getByTitle("React with 👍").click();
  await expect(picker).not.toBeVisible({ timeout: 5_000 });

  // Open the message details panel — that pins selection on the row
  // (sunset_web.gleam OpenDetail also sets selected_msg_id). With the
  // panel pinned, .is-selected is a stable expectation.
  await msgRow.getByTitle("Message details").click({ force: true });
  await expect(msgRow).toHaveClass(/is-selected/);

  const pill = msgRow.locator(
    '[data-testid="reaction-pill"][data-emoji="👍"]',
  );
  await expect(pill).toBeVisible({ timeout: 10_000 });

  // Click the pill — selection must NOT toggle, even though the pill
  // sits inside the message body's click target.
  await pill.click();
  await expect(msgRow).toHaveClass(/is-selected/);

  await ctx.close();
});
