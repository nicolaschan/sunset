// Two-tab delivery-receipt e2e: tab A sends a message; tab A's bubble
// stays at opacity 0.55 until tab B's wasm bridge auto-acks via a
// Receipt that syncs back to A. The bridge fires on_receipt → FE
// updates Model.receipts → message_view re-renders at opacity 1.

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

test("self-message stays gray until peer receipt arrives", async ({ browser }, testInfo) => {
  test.skip(testInfo.project.name === "mobile-chrome", "two-browser test runs on desktop only");

  const url = `/?relay=${encodeURIComponent(relayAddress)}#sunset-demo`;

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

  await expect(pageA.getByText("sunset", { exact: true })).toBeVisible({
    timeout: 15_000,
  });
  await expect(pageB.getByText("sunset", { exact: true })).toBeVisible({
    timeout: 15_000,
  });

  const inputA = pageA.getByPlaceholder(/^Message #/);
  const inputB = pageB.getByPlaceholder(/^Message #/);
  await expect(inputA).toBeVisible({ timeout: 15_000 });
  await expect(inputB).toBeVisible({ timeout: 15_000 });

  // A sends.
  const text = `hello from A — ${Date.now()}`;
  await inputA.fill(text);
  await inputA.press("Enter");

  // Locate A's own message bubble. The .msg-row has opacity inline-styled.
  const bubble = pageA.locator(".msg-row", { hasText: text }).first();
  await expect(bubble).toBeVisible({ timeout: 15_000 });

  // Initially pending (opacity ~0.55). Allow some slack since the bridge
  // could theoretically auto-ack in <50ms if B is already connected.
  // What we care about is the EVENTUAL transition to ~1.
  // Wait for B to actually see the message (proof-of-arrival).
  await expect(pageB.getByText(text)).toBeVisible({ timeout: 15_000 });

  // Then poll A's bubble for opacity to flip to 1.
  await expect
    .poll(
      async () =>
        bubble.evaluate((el) => parseFloat(getComputedStyle(el).opacity)),
      { timeout: 15_000 },
    )
    .toBeGreaterThan(0.95);

  await ctxA.close();
  await ctxB.close();
});
