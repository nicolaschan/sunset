// Headline acceptance for V1 (browser WebRTC RawTransport).
//
// Sets up two browsers connected through a relay (same as
// two_browser_chat.spec.js), then triggers `connect_direct(...)` on
// both ends, waits for both sides to report `peer_connection_mode` ==
// "direct", kills the relay subprocess, and verifies that subsequent
// chat traffic still flows over the direct WebRTC datachannel.

import { test, expect } from "@playwright/test";
import { spawn } from "child_process";
import { mkdtempSync, rmSync } from "fs";
import { tmpdir } from "os";
import { join } from "path";

let relayProcess = null;
let relayAddress = null;
let relayDataDir = null;

test.beforeAll(async () => {
  relayDataDir = mkdtempSync(join(tmpdir(), "sunset-relay-killtest-"));

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

test.setTimeout(120_000);
test("chat survives relay death once direct WebRTC is up", async ({ browser }) => {
  const url = `/?relay=${encodeURIComponent(relayAddress)}#sunset-killtest`;

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
    // Set the test hook BEFORE navigation so the FFI shim can latch on.
    await page.addInitScript(() => {
      window.SUNSET_TEST = true;
    });
  }

  await pageA.goto(url);
  await pageB.goto(url);

  await expect(pageA.getByText("sunset", { exact: true })).toBeVisible({ timeout: 15_000 });
  await expect(pageB.getByText("sunset", { exact: true })).toBeVisible({ timeout: 15_000 });

  const inputA = pageA.getByPlaceholder(/^Message #/);
  const inputB = pageB.getByPlaceholder(/^Message #/);
  await expect(inputA).toBeVisible({ timeout: 15_000 });
  await expect(inputB).toBeVisible({ timeout: 15_000 });

  // First sanity check: relay-mediated chat works.
  const msgPre = `pre-direct from A — ${Date.now()}`;
  await inputA.fill(msgPre);
  await inputA.press("Enter");
  await expect(pageB.getByText(msgPre)).toBeVisible({ timeout: 15_000 });

  // Wait for window.sunsetClient to be exposed by the FFI shim. The
  // wasm bundle initialises asynchronously after the first FFI call.
  await pageA.waitForFunction(() => !!window.sunsetClient, null, { timeout: 15_000 });
  await pageB.waitForFunction(() => !!window.sunsetClient, null, { timeout: 15_000 });

  // peer_connection_mode reads from the membership tracker's peer_kinds
  // map, which is only populated once start_presence is called (the
  // tracker subscribes to engine events + seeds from a snapshot). The
  // production Gleam UI calls start_presence on bootstrap; we mirror
  // that here so peer_connection_mode returns real values. Use fast
  // params (compressed cadence is harmless for this test).
  for (const p of [pageA, pageB]) {
    await p.evaluate(async () => {
      await window.sunsetClient.start_presence(300, 900, 100);
    });
  }

  // Grab each peer's pubkey.
  const aPub = await pageA.evaluate(() =>
    Array.from(window.sunsetClient.public_key),
  );
  const bPub = await pageB.evaluate(() =>
    Array.from(window.sunsetClient.public_key),
  );

  // Trigger direct-connect from A → B. The signaling rides over the
  // existing relay-mediated CRDT replication, encrypted under Noise_KK.
  // Both sides build a PC; A initiates, B's background accept worker
  // handles the inbound offer + completes the WebRTC handshake.
  await pageA.evaluate(async (pkArr) => {
    await window.sunsetClient.connect_direct(new Uint8Array(pkArr));
  }, bPub);

  // A reports "direct" once the engine.add_peer completes (set by
  // Client::connect_direct). B doesn't track its accept-side peers in
  // direct_peers so it stays "via_relay" — that's a v1 cosmetic gap;
  // the real acceptance is whether messages flow after the relay dies.
  await pageA.waitForFunction(
    (pkArr) =>
      window.sunsetClient.peer_connection_mode(new Uint8Array(pkArr)) === "direct",
    bPub,
    { timeout: 30_000 },
  );

  // Kill the relay. peer_connection_mode == "direct" already means A's
  // engine has fired PeerAdded for the WebRTC peer, which only happens
  // once the datachannel is open and the Hello handshake is complete —
  // i.e. both sides have an established connection. Tearing the relay
  // down at this point doesn't disturb the direct path; ICE/DTLS for
  // the localhost peer-pair is settled by then.
  relayProcess.kill("SIGTERM");
  // Wait for the relay process to fully exit so the WS connections die,
  // rather than guessing with a fixed sleep.
  await new Promise((resolve, reject) => {
    if (relayProcess.exitCode !== null) {
      resolve();
      return;
    }
    const timer = setTimeout(
      () => reject(new Error("relay didn't exit within 5s of SIGTERM")),
      5_000,
    );
    relayProcess.once("exit", () => {
      clearTimeout(timer);
      resolve();
    });
  });

  // Send a message in each direction; verify arrival via the direct
  // WebRTC datachannel.
  const msg1 = `post-relay-death from A — ${Date.now()}`;
  await inputA.fill(msg1);
  await inputA.press("Enter");
  await expect(pageB.getByText(msg1)).toBeVisible({ timeout: 30_000 });

  const msg2 = `post-relay-death from B — ${Date.now()}`;
  await inputB.fill(msg2);
  await inputB.press("Enter");
  await expect(pageA.getByText(msg2)).toBeVisible({ timeout: 30_000 });

  await ctxA.close();
  await ctxB.close();
});
