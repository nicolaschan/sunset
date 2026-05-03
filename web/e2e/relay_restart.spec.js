// Acceptance test for the connection-liveness + supervisor implementation:
// when the relay restarts (process killed and a new one starts on the same
// port with the same identity), the browser-side `PeerSupervisor` should
// detect the disconnect via send-side error or heartbeat timeout, redial
// with backoff, and chat traffic should resume — all without the user
// reloading the page.
//
// Mechanism under test:
//   1. Send-side reliable failure → InboundEvent::Disconnected (fast)
//      OR heartbeat timeout (15s/45s defaults) → Disconnected (slower)
//   2. Engine emits PeerRemoved
//   3. PeerSupervisor sees PeerRemoved, schedules backoff (1s initial)
//   4. fire_due_backoffs calls engine.add_peer(addr) again
//   5. New connection completes Hello → PeerAdded → state = Connected
//
// To make the relay's identity stable across restarts we share the
// `data_dir` between the two relay processes. With `identity_secret =
// "auto"`, the relay reads / creates `<data_dir>/identity.key`; the second
// process loads the file the first one wrote. This means the canonical
// `wss://host:port#x25519=<hex>` URL the supervisor stored on first dial
// remains valid after restart.

import { test, expect } from "@playwright/test";
import { spawn } from "child_process";
import { mkdtempSync, rmSync } from "fs";
import { tmpdir } from "os";
import { join } from "path";

let relayProcess = null;
let relayAddress = null;
let relayPort = null;
let relayDataDir = null;
let configPath = null;

async function startRelay(listenAddr) {
  const fs = await import("fs/promises");
  await fs.writeFile(
    configPath,
    [
      `listen_addr = "${listenAddr}"`,
      `data_dir = "${relayDataDir}"`,
      `interest_filter = "all"`,
      `identity_secret = "auto"`,
      `peers = []`,
      "",
    ].join("\n"),
  );

  const proc = spawn("sunset-relay", ["--config", configPath], {
    stdio: ["ignore", "pipe", "pipe"],
  });

  const addr = await new Promise((resolve, reject) => {
    const timer = setTimeout(
      () => reject(new Error("relay didn't print address banner within 15s")),
      15_000,
    );
    let buffer = "";
    proc.stdout.on("data", (chunk) => {
      buffer += chunk.toString();
      const m = buffer.match(/address:\s+(ws:\/\/[^\s]+)/);
      if (m) {
        clearTimeout(timer);
        resolve(m[1]);
      }
    });
    proc.stderr.on("data", (chunk) => {
      process.stderr.write(`[relay] ${chunk}`);
    });
    proc.on("error", (e) => {
      clearTimeout(timer);
      reject(e);
    });
    proc.on("exit", (code) => {
      if (code !== null && code !== 0) {
        clearTimeout(timer);
        reject(new Error(`relay exited prematurely with code ${code}`));
      }
    });
  });

  return { proc, addr };
}

test.beforeAll(async () => {
  relayDataDir = mkdtempSync(join(tmpdir(), "sunset-relay-restart-"));
  configPath = join(relayDataDir, "relay.toml");

  const result = await startRelay("127.0.0.1:0");
  relayProcess = result.proc;
  relayAddress = result.addr;

  const m = relayAddress.match(/^ws:\/\/[^:]+:(\d+)/);
  if (!m) {
    throw new Error(`couldn't parse port from address: ${relayAddress}`);
  }
  relayPort = parseInt(m[1]);
});

test.afterAll(async () => {
  if (relayProcess && relayProcess.exitCode === null) {
    relayProcess.kill("SIGTERM");
  }
  if (relayDataDir) {
    rmSync(relayDataDir, { recursive: true, force: true });
  }
});

test.setTimeout(180_000);
test("chat resumes after relay restart without page reload", async ({ browser }) => {
  const url = `/?relay=${encodeURIComponent(relayAddress)}#sunset-restarttest`;

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

  // Sanity: relay-mediated chat works before the restart.
  const msgPre = `pre-restart from A — ${Date.now()}`;
  await inputA.fill(msgPre);
  await inputA.press("Enter");
  await expect(pageB.getByText(msgPre)).toBeVisible({ timeout: 15_000 });

  // Kill the relay. The browser-side WebSocket onclose should fire
  // reasonably quickly because the relay closes the TCP connection on
  // shutdown.
  relayProcess.kill("SIGTERM");
  // Wait for the process to actually exit before we try to bind the port.
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

  // Restart the relay on the SAME port + SAME data_dir so its identity
  // (loaded from <data_dir>/identity.key) is unchanged. The supervisor's
  // stored canonical URL (with the identity hex in the fragment) remains
  // valid for redial.
  const restarted = await startRelay(`127.0.0.1:${relayPort}`);
  relayProcess = restarted.proc;
  expect(restarted.addr).toBe(relayAddress);

  // Wait for the supervisor on each side to detect the disconnect and
  // reconnect. Design budget:
  //   - send-side detection on next heartbeat tick: ≤ heartbeat_interval (15s default)
  //   - supervisor backoff: ~1s initial (with ±20% jitter)
  //   - re-dial connect + Hello handshake: <1s on localhost
  // Typical total: ~17s. We wait 25s as a tight bound — anything slower
  // than this is a regression in the supervisor's reconnect path itself.
  //
  // (Catch-up of messages sent during the gap is covered by
  // catchup_after_disconnect.spec.js, which keeps the relay up so a
  // live producer can actually publish during the gap. Killing the
  // relay disconnects everyone, so this test asserts only that chat
  // resumes for new traffic post-redial.)
  await new Promise((r) => setTimeout(r, 25_000));

  // Send a message — should arrive promptly on the freshly-redialed
  // connection.
  const msgPost = `post-restart from A — ${Date.now()}`;
  await inputA.fill(msgPost);
  await inputA.press("Enter");
  await expect(pageB.getByText(msgPost)).toBeVisible({ timeout: 15_000 });

  // Bidirectional sanity: B → A also works after redial.
  const msgPostB = `post-restart from B — ${Date.now()}`;
  await inputB.fill(msgPostB);
  await inputB.press("Enter");
  await expect(pageA.getByText(msgPostB)).toBeVisible({ timeout: 15_000 });
});
