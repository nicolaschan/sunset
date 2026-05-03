// Room-switching isolation test.
//
// Single browser tab. User joins room "alpha", sends "hello-alpha".
// Switches to room "beta", sends "hello-beta". Verifies:
//   - While viewing alpha: "hello-alpha" visible, "hello-beta" not.
//   - While viewing beta:  "hello-beta" visible, "hello-alpha" not.
//   - Switch back to alpha: "hello-alpha" reappears (replayed from
//     local store via on_message), "hello-beta" not visible.
//
// This is the load-bearing regression gate for the multi-room PR — it
// proves that the room rail's selection actually routes messages to the
// correct room and that switching never leaks messages across rooms.

import { test, expect } from "@playwright/test";
import { spawn } from "child_process";
import { mkdtempSync, rmSync } from "fs";
import { tmpdir } from "os";
import { join } from "path";

let relayProcess = null;
let relayAddress = null;
let relayDataDir = null;

test.beforeAll(async () => {
  relayDataDir = mkdtempSync(join(tmpdir(), "sunset-relay-room-switch-test-"));
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

test("messages in room alpha do not appear in room beta", async ({ browser }) => {
  const ctx = await browser.newContext();
  const page = await ctx.newPage();

  // Surface browser console errors to the test output for easier debug.
  page.on("pageerror", (err) =>
    process.stderr.write(`[pageerror] ${err.stack || err}\n`),
  );
  page.on("console", (msg) => {
    if (msg.type() === "error") {
      process.stderr.write(`[console] ${msg.text()}\n`);
    }
  });

  // Set the test hook BEFORE the bundle loads so clientOpenRoom sets
  // window.sunsetRoom on each open_room call.
  await page.addInitScript(() => { window.SUNSET_TEST = true; });

  // ── Navigate to room "alpha" ──────────────────────────────────────────
  await page.goto(`/?relay=${encodeURIComponent(relayAddress)}#alpha`);

  // Wait for the Lustre app to mount (brand text in the rooms rail).
  await expect(page.getByText("sunset", { exact: true })).toBeVisible({
    timeout: 15_000,
  });

  // Wait for the sunset engine + initial room handle to be ready.
  await page.waitForFunction(() => !!window.sunsetClient, null, { timeout: 15_000 });
  await page.waitForFunction(() => !!window.sunsetRoom, null, { timeout: 15_000 });

  // The composer placeholder is "Message #<channel>". The default
  // channel within every room is "general", so the placeholder is
  // identical for all rooms.
  const composer = page.getByPlaceholder(/^Message #/);
  await expect(composer).toBeVisible({ timeout: 15_000 });

  // ── Send "hello-alpha" in room alpha ────────────────────────────────
  await composer.fill("hello-alpha");
  await composer.press("Enter");

  // Wait until the message is delivered back via on_message and rendered.
  await expect(page.getByText("hello-alpha")).toBeVisible({ timeout: 15_000 });

  // ── Switch to room "beta" ────────────────────────────────────────────
  // Stash the current room handle so we can detect when the new room is
  // open. clientOpenRoom reassigns window.sunsetRoom for every open_room
  // call; waiting for it to change tells us the beta room handle is
  // live and its on_message callback is wired.
  //
  // Note: this pattern only works for rooms that haven't been opened
  // before (open_room is skipped when the room is already in the dict).
  // The return trip to alpha uses the content assertion instead.
  await page.evaluate(() => { window.__prevRoom = window.sunsetRoom; });

  // Changing the URL hash fires HashChanged("beta") → open_room("beta") →
  // clientOpenRoom → window.sunsetRoom = betaHandle → RoomOpened dispatch.
  await page.evaluate(() => { location.hash = "#beta"; });

  // Wait for window.sunsetRoom to be replaced with the beta handle.
  await page.waitForFunction(
    () => !!window.sunsetRoom && window.sunsetRoom !== window.__prevRoom,
    null,
    { timeout: 15_000 },
  );

  // ── Send "hello-beta" in room beta ───────────────────────────────────
  await composer.fill("hello-beta");
  await composer.press("Enter");

  await expect(page.getByText("hello-beta")).toBeVisible({ timeout: 15_000 });

  // ── Isolation check: alpha's message must NOT appear while in beta ──
  await expect(page.getByText("hello-alpha")).not.toBeVisible();

  // ── Switch back to alpha ─────────────────────────────────────────────
  // Alpha was already opened earlier, so open_room is skipped and
  // window.sunsetRoom stays pointing to the beta handle. The Lustre
  // model simply switches view → RoomView("alpha") and re-renders the
  // messages list from the already-populated per-room state.
  await page.evaluate(() => { location.hash = "#alpha"; });

  // "hello-alpha" reappearing is the completion signal — it means the
  // model has re-rendered with alpha's messages (replayed from the local
  // store when the room was first opened).
  await expect(page.getByText("hello-alpha")).toBeVisible({ timeout: 15_000 });

  // ── Isolation check: beta's message must NOT appear while in alpha ──
  await expect(page.getByText("hello-beta")).not.toBeVisible();

  await ctx.close();
});
