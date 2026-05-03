// emoji-picker-element integration:
//   1. On mobile the full picker is rendered inside a bottom sheet
//      with full sheet width. The picker host (`<emoji-picker>`) has
//      its own intrinsic width (~400px), so the wrapper must center
//      it horizontally — otherwise it sits left-aligned with empty
//      space on the right.
//   2. The picker auto-detects light/dark via prefers-color-scheme by
//      default. We override that with an explicit `class="dark"` /
//      `class="light"` so the picker tracks the *app's* resolved
//      theme (System/Light/Dark preference) instead of the OS scheme.

import { spawn } from "child_process";
import { mkdtempSync, rmSync } from "fs";
import { tmpdir } from "os";
import { join } from "path";

import { expect, test } from "@playwright/test";

let relayProcess = null;
let relayAddress = null;
let relayDataDir = null;

test.beforeAll(async () => {
  relayDataDir = mkdtempSync(join(tmpdir(), "sunset-relay-emoji-test-"));
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

async function openChat(browser, { hash = "emoji-picker", theme = null } = {}) {
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
  // Seed the persisted theme preference before the page boots so the
  // app's init reads it directly. This avoids driving the settings
  // sheet UI just to flip the theme — the sheet is exercised in
  // ui_tweaks.spec.js, and going through it here would leak open-drawer
  // state into the picker interaction afterwards.
  await page.addInitScript((t) => {
    window.SUNSET_TEST = true;
    if (t === "dark" || t === "light") {
      try {
        localStorage.setItem("sunset-web/theme", t);
      } catch {}
    }
  }, theme);
  await page.goto(url);
  await expect(page.getByText("sunset", { exact: true })).toBeVisible({
    timeout: 15_000,
  });
  const composer = page.getByPlaceholder(/^Message #/);
  await expect(composer).toBeVisible({ timeout: 15_000 });
  return { ctx, page, composer };
}

async function openFullPicker(page, isMobile, composer) {
  // Send a message so we have a row to react against.
  await composer.fill(`emoji-picker-${Date.now()}`);
  await composer.press("Enter");
  const msgRow = page.locator(".msg-row").last();
  await expect(msgRow).toBeVisible({ timeout: 15_000 });

  if (isMobile) {
    // Tap the row to flip is-selected so the action toolbar picks up
    // pointer events on touch.
    await msgRow.click();
    await expect(msgRow).toHaveClass(/is-selected/);
  }
  await msgRow.locator('button[aria-label="React"]').click({ force: true });
  await expect(page.locator('[data-testid="reaction-picker"]').first()).toBeVisible({
    timeout: 5_000,
  });
  await page.locator('[data-testid="reaction-picker-more"]').first().click();

  const picker = page.locator('[data-testid="full-emoji-picker"]');
  await expect(picker).toBeVisible({ timeout: 10_000 });
  return picker;
}

test("on mobile, the full emoji picker is centered horizontally in the sheet", async ({
  browser,
}, testInfo) => {
  test.skip(
    testInfo.project.name !== "mobile-chrome",
    "the off-center bug only manifests on the bottom-sheet variant",
  );
  const { ctx, page, composer } = await openChat(browser, { hash: "picker-center" });

  await openFullPicker(page, true, composer);

  const { pickerCenter, sheetCenter } = await page.evaluate(() => {
    const p = document.querySelector('[data-testid="full-emoji-picker"]');
    const s = document.querySelector('[data-testid="full-emoji-picker-sheet"]');
    const pr = p.getBoundingClientRect();
    const sr = s.getBoundingClientRect();
    return {
      pickerCenter: pr.left + pr.width / 2,
      sheetCenter: sr.left + sr.width / 2,
    };
  });
  // Allow ±2px for sub-pixel rounding; the picker must align with the
  // sheet's horizontal center, not be left-flushed.
  expect(Math.abs(pickerCenter - sheetCenter)).toBeLessThan(3);

  await ctx.close();
});

test("emoji picker carries the app's resolved theme class (light)", async ({
  browser,
}, testInfo) => {
  // Pre-seed the theme preference in localStorage so the app boots in
  // light mode regardless of the runner's prefers-color-scheme. The
  // picker auto-detects via prefers-color-scheme by default; the app
  // overriding that with `class="light"` is exactly what we're testing.
  const isMobile = testInfo.project.name === "mobile-chrome";
  const { ctx, page, composer } = await openChat(browser, {
    hash: "picker-light",
    theme: "light",
  });

  const picker = await openFullPicker(page, isMobile, composer);
  await expect(picker).toHaveClass(/light/);
  await expect(picker).not.toHaveClass(/dark/);

  await ctx.close();
});

test("emoji picker carries the app's resolved theme class (dark)", async ({
  browser,
}, testInfo) => {
  const isMobile = testInfo.project.name === "mobile-chrome";
  const { ctx, page, composer } = await openChat(browser, {
    hash: "picker-dark",
    theme: "dark",
  });

  const picker = await openFullPicker(page, isMobile, composer);
  await expect(picker).toHaveClass(/dark/);
  await expect(picker).not.toHaveClass(/light/);

  // The picker's `--background` CSS custom property must come from
  // the dark palette (which is nowhere near white). We can't read
  // custom-prop values directly via getComputedStyle in all browsers,
  // but the resolved background of the host element must be a dark
  // tone — assert it isn't a near-white colour.
  const bg = await picker.evaluate(
    (el) => getComputedStyle(el).getPropertyValue("--background").trim(),
  );
  expect(bg).not.toBe("");
  // Light palette `surface` is `#ffffff`; dark is `#1c1814`. Cheap
  // dark/light check: take the hex (or rgb) of `--background`, strip
  // non-hex chars, and assert the average byte is below 128.
  const norm = bg.startsWith("#")
    ? bg.slice(1)
    : (bg.match(/\d+/g) || []).slice(0, 3).map((n) => parseInt(n, 10).toString(16).padStart(2, "0")).join("");
  if (norm.length >= 6) {
    const r = parseInt(norm.slice(0, 2), 16);
    const g = parseInt(norm.slice(2, 4), 16);
    const b = parseInt(norm.slice(4, 6), 16);
    expect((r + g + b) / 3).toBeLessThan(128);
  }

  await ctx.close();
});
