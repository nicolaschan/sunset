// Reactions e2e: one tab sends a message, opens the quick-picker via
// the "React" toolbar button, picks 👍, and the reaction pill appears
// on the message row. Pill click toggles the user's own reaction
// (Slack/Discord style).
//
// Single-browser (single context) — cross-peer propagation is covered
// by the Rust two_peer_reaction integration test; this e2e verifies the
// full FE loop: send → react toolbar → picker → pill renders → click
// pill → reaction removed.

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

async function openChatPage(browser) {
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

  return { ctx, page, input };
}

async function sendMessage(page, input, text) {
  await input.fill(text);
  await input.press("Enter");
  const msgRow = page.locator(".msg-row", { hasText: text }).first();
  await expect(msgRow).toBeVisible({ timeout: 15_000 });
  return msgRow;
}

// force=true: the action toolbar is opacity:0 until :hover; without
// force, Playwright's actionability check rejects the click. The click
// itself still dispatches because the toolbar's pointer-events flips
// on at the same time.
async function seedThumbsUpReaction(page, msgRow) {
  await msgRow.getByTitle("React").click({ force: true });
  const picker = page.locator('[data-testid="reaction-picker"]');
  await expect(picker).toBeVisible({ timeout: 5_000 });
  await picker.getByTitle("React with 👍").click();
  await expect(picker).not.toBeVisible({ timeout: 5_000 });
}

const skipOnMobile = (testInfo) =>
  test.skip(
    testInfo.project.name === "mobile-chrome",
    "reaction picker is desktop-only in this implementation",
  );

test("react button opens quick-picker and pill appears after picking", async ({
  browser,
}, testInfo) => {
  skipOnMobile(testInfo);

  const { ctx, page, input } = await openChatPage(browser);
  const msgRow = await sendMessage(page, input, `reactions e2e — ${Date.now()}`);
  await seedThumbsUpReaction(page, msgRow);

  const pill = msgRow.locator(
    '[data-testid="reaction-pill"][data-emoji="👍"]',
  );
  await expect(pill).toBeVisible({ timeout: 10_000 });
  await expect(pill.locator("span")).toHaveText("1", { timeout: 5_000 });

  await ctx.close();
});

// Slack/Discord-style pill toggle: clicking a pill you've already
// reacted with removes your reaction. The mobile skip is for the
// picker-driven seeding step — the pill itself is the same `<button>`
// on both viewports.
test("clicking own reaction pill removes the reaction", async ({
  browser,
}, testInfo) => {
  skipOnMobile(testInfo);

  const { ctx, page, input } = await openChatPage(browser);
  const msgRow = await sendMessage(
    page,
    input,
    `pill-toggle e2e — ${Date.now()}`,
  );
  await seedThumbsUpReaction(page, msgRow);

  const pill = msgRow.locator(
    '[data-testid="reaction-pill"][data-emoji="👍"]',
  );
  await expect(pill).toBeVisible({ timeout: 10_000 });
  await expect(pill).toHaveAttribute("aria-pressed", "true");
  await expect(pill.locator("span")).toHaveText("1", { timeout: 5_000 });

  await pill.click();
  await expect(pill).toHaveCount(0, { timeout: 10_000 });

  await ctx.close();
});

// Pills sit inside the message body's click target (which toggles
// row selection). Without event.stop_propagation a pill click would
// both toggle the reaction AND toggle selection — leaking UI state.
// This test pins selection via the message-details button and verifies
// a pill click preserves it.
test("pill click toggles only the reaction, not row selection", async ({
  browser,
}, testInfo) => {
  skipOnMobile(testInfo);

  const { ctx, page, input } = await openChatPage(browser);
  const msgRow = await sendMessage(
    page,
    input,
    `pill-selection e2e — ${Date.now()}`,
  );
  await seedThumbsUpReaction(page, msgRow);

  // Opening the details panel sets selected_msg_id, giving us a stable
  // .is-selected baseline to assert against after the pill click.
  await msgRow.getByTitle("Message details").click({ force: true });
  await expect(msgRow).toHaveClass(/is-selected/);

  const pill = msgRow.locator(
    '[data-testid="reaction-pill"][data-emoji="👍"]',
  );
  await expect(pill).toBeVisible({ timeout: 10_000 });

  await pill.click();
  await expect(msgRow).toHaveClass(/is-selected/);

  await ctx.close();
});

// Regression: clicking the "+" button in the quick-picker (desktop) or
// the reaction sheet (mobile) must mount the full emoji picker. A
// previous refactor left the OpenFullEmojiPicker handler wired but
// removed the overlay/sheet rendering, so the click was a silent no-op.
// We assert the appropriate container becomes visible — actually
// clicking through the emoji-picker-element web component is shadow-DOM
// territory and out of scope for this regression check.
test("clicking the + button mounts the full emoji picker", async ({
  browser,
}, testInfo) => {
  const isMobile = testInfo.project.name === "mobile-chrome";

  const { ctx, page, input } = await openChatPage(browser);
  const msgRow = await sendMessage(
    page,
    input,
    `more-reactions e2e — ${Date.now()}`,
  );

  // The .msg-actions toolbar (which holds React and Details) is
  // pointer-events: none until the row is selected on touch devices
  // (`@media (hover: none)` in shell.gleam). Tap the message body
  // first to flip is-selected, otherwise the React click on mobile
  // hits the body wrapper instead of the toolbar.
  if (isMobile) {
    await msgRow.click();
    await expect(msgRow).toHaveClass(/is-selected/);
  }

  // Open the quick-picker on both viewports — desktop renders the
  // inline picker next to the row, phone renders a bottom sheet.
  await msgRow.getByTitle("React").click({ force: true });
  const quickPicker = page.locator('[data-testid="reaction-picker"]').first();
  await expect(quickPicker).toBeVisible({ timeout: 5_000 });

  // The "+" button has the same data-testid on desktop and phone.
  const more = page.locator('[data-testid="reaction-picker-more"]').first();
  await expect(more).toBeVisible({ timeout: 5_000 });
  await more.click();

  const fullPicker = isMobile
    ? page.locator('[data-testid="full-emoji-picker-sheet"]')
    : page.locator('[data-testid="full-emoji-picker-overlay"]');
  await expect(fullPicker).toBeVisible({ timeout: 5_000 });

  // The lazy-imported `emoji-picker-element` web component should mount
  // inside the overlay/sheet. Its container has a stable testid set by
  // emoji_picker.view.
  const pickerEl = page.locator('[data-testid="full-emoji-picker"]');
  await expect(pickerEl).toBeVisible({ timeout: 10_000 });

  await ctx.close();
});
