// Self-name e2e tests.
//
// 1. Persistence: set a name in the settings popover, reload, verify the
//    value persists in the input and the you-row shows the name.
// 2. Cross-peer rename: peer 1 sends a message; peer 2 sees it. Peer 1
//    sets a name → peer 2's already-rendered message re-authors to that
//    name. Peer 1 clears the name → peer 2's message reverts to the
//    short-pubkey fallback.
//
// Requires `sunset-relay` on PATH (provided by the webTestRunner wrapper
// in flake.nix). Spins up a local relay subprocess the same way
// two_browser_chat.spec.js does.

import { test, expect } from "@playwright/test";
import { spawn } from "child_process";
import { mkdtempSync, rmSync } from "fs";
import { tmpdir } from "os";
import { join } from "path";
import {
  openRoomsDrawer,
  closeDrawer,
} from "./helpers/viewport.js";

let relayProcess = null;
let relayAddress = null;
let relayDataDir = null;

test.beforeAll(async () => {
  relayDataDir = mkdtempSync(join(tmpdir(), "sunset-relay-self-name-"));

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

test.describe("self-name", () => {
  test("name persists across reload", async ({ page }, testInfo) => {
    const url = `/?relay=${encodeURIComponent(relayAddress)}#self-name-persist`;

    page.on("pageerror", (err) =>
      process.stderr.write(`[pageerror] ${err.stack || err}\n`),
    );
    page.on("console", (msg) => {
      if (msg.type() === "error") {
        process.stderr.write(`[console] ${msg.text()}\n`);
      }
    });

    await page.goto(url);

    // Wait for the chat shell to mount (brand text in the rooms rail).
    await expect(page.getByText("sunset", { exact: true })).toBeVisible({
      timeout: 15_000,
    });

    // Open the rooms drawer on mobile; no-op on desktop.
    await openRoomsDrawer(page, testInfo);

    // Click the you-row to open the settings popover.
    await page.getByTestId("you-row").click();

    // Fill in a name.
    const nameInput = page.getByTestId("settings-name-input");
    await expect(nameInput).toBeVisible({ timeout: 5_000 });
    await nameInput.fill("Alice");
    // Trigger the on_input handler by pressing Tab (blur), then wait for
    // the 300 ms debounce to flush to localStorage.
    await nameInput.press("Tab");
    await page.waitForTimeout(500);

    // Reload and re-open the settings popover.
    await page.reload();
    await expect(page.getByText("sunset", { exact: true })).toBeVisible({
      timeout: 15_000,
    });

    await openRoomsDrawer(page, testInfo);
    await page.getByTestId("you-row").click();

    // The name should have been restored from localStorage.
    await expect(page.getByTestId("settings-name-input")).toHaveValue("Alice", {
      timeout: 5_000,
    });
  });

  test("rename updates already-sent messages on the peer", async ({
    browser,
  }, testInfo) => {
    const roomUrl = `/?relay=${encodeURIComponent(relayAddress)}#self-name-rename`;

    const ctx1 = await browser.newContext();
    const ctx2 = await browser.newContext();
    const page1 = await ctx1.newPage();
    const page2 = await ctx2.newPage();

    for (const [name, page] of [
      ["1", page1],
      ["2", page2],
    ]) {
      page.on("pageerror", (err) =>
        process.stderr.write(`[peer${name} pageerror] ${err.stack || err}\n`),
      );
      page.on("console", (msg) => {
        if (msg.type() === "error") {
          process.stderr.write(`[peer${name} console] ${msg.text()}\n`);
        }
      });
    }

    await page1.goto(roomUrl);
    await page2.goto(roomUrl);

    // Wait for both peers to have the chat shell mounted.
    await expect(page1.getByText("sunset", { exact: true })).toBeVisible({
      timeout: 15_000,
    });
    await expect(page2.getByText("sunset", { exact: true })).toBeVisible({
      timeout: 15_000,
    });

    // Wait for both composers to be ready.
    const input1 = page1.getByPlaceholder(/^Message #/);
    const input2 = page2.getByPlaceholder(/^Message #/);
    await expect(input1).toBeVisible({ timeout: 15_000 });
    await expect(input2).toBeVisible({ timeout: 15_000 });

    // Peer 1 sends a message.
    const msg = `hello-rename-${Date.now()}`;
    await input1.fill(msg);
    await input1.press("Enter");

    // Peer 2 sees the message arrive.
    await expect(page2.getByText(msg)).toBeVisible({ timeout: 15_000 });

    // Capture peer 2's rendering of the message author — should be the
    // short-pubkey fallback since peer 1 has no name yet.
    const p2MessageAuthor = page2
      .locator("[data-testid='message-author']")
      .first();
    const initialAuthor = await p2MessageAuthor.textContent();
    // The initial author should be non-empty (it's the short pubkey).
    expect(initialAuthor).toBeTruthy();

    // Peer 1 opens settings and sets a name.
    await openRoomsDrawer(page1, testInfo);
    await page1.getByTestId("you-row").click();
    const nameInput1 = page1.getByTestId("settings-name-input");
    await expect(nameInput1).toBeVisible({ timeout: 5_000 });
    await nameInput1.fill("Alice");
    await nameInput1.press("Tab");
    await page1.waitForTimeout(500);

    // Peer 2's already-rendered message should now show "Alice".
    await expect(p2MessageAuthor).toHaveText("Alice", { timeout: 15_000 });

    // Peer 1 clears the name — reopen settings if the popover closed.
    const popoverVisible = await page1
      .getByTestId("settings-popover")
      .isVisible()
      .catch(() => false);
    if (!popoverVisible) {
      await openRoomsDrawer(page1, testInfo);
      await page1.getByTestId("you-row").click();
      await expect(page1.getByTestId("settings-name-input")).toBeVisible({
        timeout: 5_000,
      });
    }
    await page1.getByTestId("settings-name-input").fill("");
    await page1.getByTestId("settings-name-input").press("Tab");
    await page1.waitForTimeout(500);

    // Peer 2's message should revert to the original short-pubkey.
    await expect(p2MessageAuthor).toHaveText(initialAuthor, {
      timeout: 15_000,
    });

    await ctx1.close();
    await ctx2.close();
  });
});
