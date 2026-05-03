// Markdown rendering e2e — verifies the full WASM parse → Gleam decode →
// Lustre render pipeline produces real HTML elements for bold, links, and
// inline code.
//
// A single browser context sends a message with mixed markdown syntax and
// then asserts the rendered DOM contains <strong>, <a href=...>, and <code>
// elements. No second browser is needed — the local WASM store delivers the
// sent message back to the same tab's subscription, which is the path we care
// about exercising.

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

test("markdown renders bold, link, and inline-code in the message stream", async ({
  browser,
}) => {
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

  const input = page.getByPlaceholder(/^Message #/);
  await expect(input).toBeVisible({ timeout: 15_000 });

  // Send a message with bold, a masked link, and inline code.
  await input.fill("**bold** [link](https://example.com) `code`");
  await input.press("Enter");

  // Wait for the message row to appear. The plain-text representation
  // includes all three tokens, so we can anchor on any unique fragment.
  const msgRow = page.locator(".msg-row", { hasText: "bold" }).first();
  await expect(msgRow).toBeVisible({ timeout: 15_000 });

  // Assert <strong> contains "bold". Search within the message stream
  // area directly to avoid any lazy-locator re-evaluation pitfalls.
  const strong = page.locator(".msg-row strong").filter({ hasText: "bold" });
  await expect(strong).toBeVisible({ timeout: 10_000 });

  // Assert <a> carries the correct href, target, and rel.
  const anchor = page.locator('.msg-row a[href="https://example.com"]');
  await expect(anchor).toBeVisible({ timeout: 10_000 });
  await expect(anchor).toHaveText("link");
  await expect(anchor).toHaveAttribute("target", "_blank");
  await expect(anchor).toHaveAttribute("rel", "noopener noreferrer");

  // Assert <code> contains "code".
  const code = page.locator(".msg-row code").filter({ hasText: "code" });
  await expect(code).toBeVisible({ timeout: 10_000 });

  await ctx.close();
});
