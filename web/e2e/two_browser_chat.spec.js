// Two-browser e2e test — the headline acceptance for Plan E.
//
// Spins up a local sunset-relay subprocess on a random port, captures the
// relay's address line from stdout, then opens two browser contexts at
// `/?relay=<encoded>` and exercises real send/receive through the relay.
//
// Requires `sunset-relay` on PATH (provided by the webTestRunner wrapper
// in flake.nix). Fails informatively otherwise.

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

  // Write a minimal config so the relay binds a random port + uses our
  // tempdir for storage.
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

  // Spawn the relay; capture its `address: ws://...` banner.
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

test("two browsers exchange a message via relay", async ({ browser }) => {
  const url = `/?relay=${encodeURIComponent(relayAddress)}#sunset-demo`;

  const ctxA = await browser.newContext();
  const ctxB = await browser.newContext();
  const pageA = await ctxA.newPage();
  const pageB = await ctxB.newPage();

  // Surface browser console errors to the test output for easier debug.
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

  // Each browser context starts with empty localStorage so identities
  // are distinct (per-context isolation).
  await pageA.goto(url);
  await pageB.goto(url);

  // Wait for the chat shell to mount (brand text in the rooms rail).
  await expect(pageA.getByText("sunset", { exact: true })).toBeVisible({
    timeout: 15_000,
  });
  await expect(pageB.getByText("sunset", { exact: true })).toBeVisible({
    timeout: 15_000,
  });

  // The composer placeholder is "Message #<channel>". Find the input via
  // its placeholder. Each browser should have one.
  const inputA = pageA.getByPlaceholder(/^Message #/);
  const inputB = pageB.getByPlaceholder(/^Message #/);
  await expect(inputA).toBeVisible({ timeout: 15_000 });
  await expect(inputB).toBeVisible({ timeout: 15_000 });

  // A sends; B receives.
  const msg1 = `hello from A — ${Date.now()}`;
  await inputA.fill(msg1);
  await inputA.press("Enter");
  await expect(pageB.getByText(msg1)).toBeVisible({ timeout: 15_000 });

  // B replies; A receives.
  const msg2 = `hello from B — ${Date.now()}`;
  await inputB.fill(msg2);
  await inputB.press("Enter");
  await expect(pageA.getByText(msg2)).toBeVisible({ timeout: 15_000 });
});

test("bare host:port in ?relay= triggers GET / resolution", async ({
  browser,
}) => {
  // Strip the canonical `ws://host:port#x25519=hex` down to bare
  // `host:port`. The Gleam UI passes this string straight to
  // `Client::add_relay`, which routes it through `sunset-relay-resolver`
  // → `WebSysFetch::get` → relay's `GET /` JSON → reconstructed
  // canonical PeerAddr → Noise IK handshake. If any link in that chain
  // is broken, the relay status never reaches "connected".
  const hostPort = relayAddress
    .replace(/^ws:\/\//, "")
    .split("#")[0]
    .replace(/\/$/, "");
  const url = `/?relay=${encodeURIComponent(hostPort)}#sunset-resolver-test`;

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

  // Round-trip a message — this is the strict signal that the resolver
  // path worked end-to-end. If the GET / fetch fails or the
  // reconstructed x25519 doesn't match the relay's actual key, the
  // Noise handshake fails and the message never arrives at B.
  const inputA = pageA.getByPlaceholder(/^Message #/);
  const inputB = pageB.getByPlaceholder(/^Message #/);
  await expect(inputA).toBeVisible({ timeout: 15_000 });
  await expect(inputB).toBeVisible({ timeout: 15_000 });

  const msg = `bare hostname dial works — ${Date.now()}`;
  await inputA.fill(msg);
  await inputA.press("Enter");
  await expect(pageB.getByText(msg)).toBeVisible({ timeout: 15_000 });
});
