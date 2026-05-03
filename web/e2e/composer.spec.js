// Composer e2e — verifies Enter sends, Shift+Enter inserts a newline, and
// Ctrl+B (or Cmd+B on macOS) wraps a selection with **…** bold markers.
//
// Uses the same relay-spawn pattern as markdown.spec.js.

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

async function openComposerPage(browser) {
  const url = `/?relay=${encodeURIComponent(relayAddress)}#sunset-demo`;
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

test("Enter sends, Shift+Enter inserts newline, Ctrl+B wraps selection", async ({
  browser,
}) => {
  const { ctx, page, composer } = await openComposerPage(browser);

  // --- Enter sends a message ---
  await composer.fill("first");
  await composer.press("Enter");
  // The sent message should appear in the message stream.
  await expect(
    page.locator(".msg-row").filter({ hasText: "first" }).first(),
  ).toBeVisible({ timeout: 15_000 });
  // Composer should be cleared after send.
  await expect(composer).toHaveValue("");

  // --- Shift+Enter inserts a newline (does not send) ---
  await composer.fill("a");
  await composer.press("Shift+Enter");
  await composer.type("b");
  await expect(composer).toHaveValue("a\nb");

  // --- Ctrl+B (or Cmd+B) wraps the selection with **...** ---
  await composer.fill("hello");
  await composer.press("ControlOrMeta+a"); // select all
  await composer.press("ControlOrMeta+b"); // wrap with bold markers
  await expect(composer).toHaveValue("**hello**");

  await ctx.close();
});

// Regression: the auto-grow FFI sets `style.height` imperatively on
// every input, but Lustre's value-prop update on submit doesn't clear
// it. Without an explicit auto-grow re-run on submit the textarea
// stays sized for the just-sent multi-line draft, hovering over the
// (now empty) composer.
test("textarea height collapses to one line after submitting a multi-line message", async ({
  browser,
}) => {
  const { ctx, page, composer } = await openComposerPage(browser);

  // Baseline: empty 1-line height.
  const oneLineHeight = await composer.evaluate(
    (el) => el.getBoundingClientRect().height,
  );
  expect(oneLineHeight).toBeGreaterThan(0);

  // Compose three lines so the textarea grows past 1-line height.
  await composer.fill("line one");
  await composer.press("Shift+Enter");
  await composer.type("line two");
  await composer.press("Shift+Enter");
  await composer.type("line three");
  await expect(composer).toHaveValue("line one\nline two\nline three");

  const grownHeight = await composer.evaluate(
    (el) => el.getBoundingClientRect().height,
  );
  expect(grownHeight).toBeGreaterThan(oneLineHeight + 8);

  // Submit; verify the textarea cleared AND visually collapsed back.
  await composer.press("Enter");
  await expect(composer).toHaveValue("");
  await expect(
    page.locator(".msg-row").filter({ hasText: "line three" }).first(),
  ).toBeVisible({ timeout: 15_000 });

  // Allow the resize effect to flush (Lustre dispatches it on the next
  // tick). Poll the height — a small tolerance on top of the baseline
  // covers sub-pixel rounding from the line-height-rounded measurement.
  await expect
    .poll(async () => composer.evaluate((el) => el.getBoundingClientRect().height), {
      timeout: 5_000,
    })
    .toBeLessThanOrEqual(oneLineHeight + 2);

  await ctx.close();
});
