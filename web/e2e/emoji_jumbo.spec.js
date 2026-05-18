// iMessage-style large emoji rendering + desktop composer emoji
// picker.
//
// Two related features that share a spec because they exercise the
// same end-to-end path (compose → send → render):
//
//   1. A message whose body is 1–3 emoji (and nothing else, modulo
//      whitespace) renders at a much larger size than mixed text.
//      Render is tagged with `data-testid="emoji-jumbo"` and the
//      emoji count via `data-emoji-count`.
//
//   2. On desktop, the composer surfaces an emoji button next to
//      the attach button. Clicking it opens the full emoji picker;
//      tapping an emoji splices it into the textarea at the current
//      caret. On mobile the button is absent (the OS keyboard already
//      exposes an emoji panel).

import { spawn } from "child_process";
import { mkdtempSync, rmSync } from "fs";
import { tmpdir } from "os";
import { join } from "path";

import { expect, test } from "@playwright/test";

let relayProcess = null;
let relayAddress = null;
let relayDataDir = null;

test.beforeAll(async () => {
  relayDataDir = mkdtempSync(join(tmpdir(), "sunset-relay-emoji-jumbo-"));
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

async function openChat(browser, hash) {
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
  await page.goto(url);
  await expect(page.getByText("sunset", { exact: true })).toBeVisible({
    timeout: 15_000,
  });
  const composer = page.getByPlaceholder(/^Message #/);
  await expect(composer).toBeVisible({ timeout: 15_000 });
  return { ctx, page, composer };
}

async function sendBody(composer, body) {
  await composer.fill(body);
  await composer.press("Enter");
}

// Read the resolved font-size (in px) of the locator's bounding box.
// `getComputedStyle` returns the value the renderer will actually use
// to lay out the glyph, which is the load-bearing assertion here —
// looking only at the data attribute would let a CSS regression slip
// through (e.g. someone unsets the `font-size` and the bubble still
// carries the test id but renders at normal size).
async function fontSizePx(locator) {
  const raw = await locator.evaluate((el) => getComputedStyle(el).fontSize);
  return parseFloat(raw);
}

test("a 1-emoji-only body renders with the jumbo size + tag", async ({
  browser,
}) => {
  const { ctx, page, composer } = await openChat(browser, "jumbo-one");
  await sendBody(composer, "🌅");

  const jumbo = page.locator('[data-testid="emoji-jumbo"]').last();
  await expect(jumbo).toBeVisible({ timeout: 15_000 });
  await expect(jumbo).toHaveAttribute("data-emoji-count", "1");

  // Body element font-size must be significantly larger than the
  // surrounding chat surface font. The normal body size is
  // ≈16.875px (see main_panel.gleam); jumbo-1 ≥ 48px is the visual
  // contract.
  const size = await fontSizePx(jumbo);
  expect(size).toBeGreaterThanOrEqual(48);

  await ctx.close();
});

test("a 2-emoji-only body renders at a slightly smaller jumbo size", async ({
  browser,
}) => {
  const { ctx, page, composer } = await openChat(browser, "jumbo-two");
  await sendBody(composer, "🌅🌙");

  const jumbo = page.locator('[data-testid="emoji-jumbo"]').last();
  await expect(jumbo).toBeVisible({ timeout: 15_000 });
  await expect(jumbo).toHaveAttribute("data-emoji-count", "2");

  // 2-emoji size scales down from the 1-emoji size — both are still
  // visibly bigger than body text.
  const size = await fontSizePx(jumbo);
  expect(size).toBeGreaterThanOrEqual(38);
  expect(size).toBeLessThan(54);

  await ctx.close();
});

test("a 3-emoji-only body still gets the jumbo treatment", async ({
  browser,
}) => {
  const { ctx, page, composer } = await openChat(browser, "jumbo-three");
  await sendBody(composer, "🌅🌙🔥");

  const jumbo = page.locator('[data-testid="emoji-jumbo"]').last();
  await expect(jumbo).toBeVisible({ timeout: 15_000 });
  await expect(jumbo).toHaveAttribute("data-emoji-count", "3");

  const size = await fontSizePx(jumbo);
  expect(size).toBeGreaterThanOrEqual(32);
  expect(size).toBeLessThan(48);

  await ctx.close();
});

test("a 4+-emoji body renders at normal size (no jumbo tag)", async ({
  browser,
}) => {
  const { ctx, page, composer } = await openChat(browser, "jumbo-four");
  await sendBody(composer, "🌅🌙🔥👀");

  // The just-sent row must be visible, but it must NOT carry the
  // emoji-jumbo test id — beyond 3 emoji we revert to normal text.
  const msgRow = page.locator(".msg-row").last();
  await expect(msgRow).toBeVisible({ timeout: 15_000 });
  await expect(msgRow.locator('[data-testid="emoji-jumbo"]')).toHaveCount(0);

  await ctx.close();
});

test("a body mixing text + emoji renders at normal size", async ({
  browser,
}) => {
  const { ctx, page, composer } = await openChat(browser, "jumbo-mixed");
  await sendBody(composer, "hi 🌅");

  const msgRow = page.locator(".msg-row", { hasText: "hi" }).last();
  await expect(msgRow).toBeVisible({ timeout: 15_000 });
  await expect(msgRow.locator('[data-testid="emoji-jumbo"]')).toHaveCount(0);

  await ctx.close();
});

// ─────────────────────────────────────────────────────────────────────
// Desktop composer emoji button
// ─────────────────────────────────────────────────────────────────────

test("desktop composer surfaces an emoji button that opens the picker", async ({
  browser,
}, testInfo) => {
  test.skip(
    testInfo.project.name === "mobile-chrome",
    "the composer emoji button is desktop-only",
  );

  const { ctx, page, composer } = await openChat(browser, "composer-emoji");

  // Button is present next to the attach button and rendered before
  // the textarea (left of it in the toolbar row).
  const emojiBtn = page.locator('[data-testid="composer-emoji"]');
  await expect(emojiBtn).toBeVisible({ timeout: 5_000 });

  // Clicking opens the full emoji picker overlay.
  await emojiBtn.click();
  const overlay = page.locator(
    '[data-testid="composer-emoji-picker-overlay"]',
  );
  await expect(overlay).toBeVisible({ timeout: 10_000 });
  await expect(page.locator('[data-testid="full-emoji-picker"]')).toBeVisible({
    timeout: 10_000,
  });

  // The picker dispatches a synthetic `emoji-click` CustomEvent with
  // `detail.unicode` — synthesize it directly rather than driving the
  // shadow-DOM grid by hand. This is the exact pathway the picker
  // uses in production; reaching into its shadow-DOM to click a cell
  // would couple the test to internal markup that the picker can
  // legitimately re-shuffle.
  await page.evaluate(() => {
    const picker = document.querySelector(
      '[data-testid="full-emoji-picker"]',
    );
    picker.dispatchEvent(
      new CustomEvent("emoji-click", {
        bubbles: true,
        composed: true,
        detail: { unicode: "🌅" },
      }),
    );
  });

  // The picked emoji must appear in the textarea.
  await expect(composer).toHaveValue("🌅", { timeout: 5_000 });

  // Picker stays open for a second pick (matches iMessage / Signal
  // UX — you usually want a couple of emoji in a row).
  await expect(overlay).toBeVisible();

  await page.evaluate(() => {
    const picker = document.querySelector(
      '[data-testid="full-emoji-picker"]',
    );
    picker.dispatchEvent(
      new CustomEvent("emoji-click", {
        bubbles: true,
        composed: true,
        detail: { unicode: "🌙" },
      }),
    );
  });

  await expect(composer).toHaveValue("🌅🌙", { timeout: 5_000 });

  await ctx.close();
});

test("desktop composer emoji picker dismisses on backdrop click", async ({
  browser,
}, testInfo) => {
  test.skip(
    testInfo.project.name === "mobile-chrome",
    "the composer emoji picker is desktop-only",
  );

  const { ctx, page } = await openChat(browser, "composer-emoji-backdrop");
  await page.locator('[data-testid="composer-emoji"]').click();
  const overlay = page.locator(
    '[data-testid="composer-emoji-picker-overlay"]',
  );
  await expect(overlay).toBeVisible({ timeout: 10_000 });

  // Click the very top-left of the viewport: well outside the
  // picker overlay (which is anchored above the composer at the
  // bottom-left, never at the top edge).
  await page.mouse.click(2, 2);

  await expect(overlay).not.toBeVisible({ timeout: 5_000 });

  await ctx.close();
});

test("mobile composer does not render an emoji button", async ({
  browser,
}, testInfo) => {
  test.skip(
    testInfo.project.name !== "mobile-chrome",
    "this asserts the *absence* of the desktop-only button",
  );

  const { ctx, page } = await openChat(browser, "composer-emoji-mobile");

  // The phone shell renders the composer in a bottom-sheet-like
  // layout; the emoji button must not appear there because the OS
  // keyboard's emoji panel already covers the affordance.
  await expect(page.locator('[data-testid="composer-emoji"]')).toHaveCount(0);
  // And no composer picker overlay is wired up either.
  await expect(
    page.locator('[data-testid="composer-emoji-picker-overlay"]'),
  ).toHaveCount(0);

  await ctx.close();
});
