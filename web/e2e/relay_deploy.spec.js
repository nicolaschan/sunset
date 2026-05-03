// Acceptance test for the resolver-path retry behavior on a fresh page
// load that lands during a relay deploy.
//
// Scenario reproduced (from a production report):
//   * User loads sunset.chat while the relay is briefly down (deploy gap).
//   * The browser hits `Client::add_relay`, which routes a non-canonical
//     input (e.g. `relay.sunset.chat`) through `sunset-relay-resolver`'s
//     HTTP `GET /` to learn the relay's x25519 key.
//   * During the deploy the upstream relay is down; the reverse proxy (or
//     plain TCP, in this localhost test) fails the request. In production
//     this surfaces as a 503 with no `Access-Control-Allow-Origin` header,
//     blocking the browser from reading the response and reporting:
//     "Cross-Origin Request Blocked … Status code: 503".
//   * Without retry, the Lustre frontend handles `RelayConnectResult(Error)`
//     by setting `relay_status: "error"` and never tries again. Even after
//     the relay comes back up, chat is dead until the user reloads.
//
// This test binds and tears down a relay to allocate `(port, identity)`,
// then opens two browsers while the port is closed. The fix makes the
// frontend retry `add_relay` with backoff, so when the relay restarts on
// the same port+data_dir both clients converge to "connected" and chat
// works end-to-end.

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

async function stopRelay(proc) {
  if (!proc || proc.exitCode !== null) {
    return;
  }
  proc.kill("SIGTERM");
  await new Promise((resolve, reject) => {
    if (proc.exitCode !== null) {
      resolve();
      return;
    }
    const timer = setTimeout(
      () => reject(new Error("relay didn't exit within 5s of SIGTERM")),
      5_000,
    );
    proc.once("exit", () => {
      clearTimeout(timer);
      resolve();
    });
  });
}

test.beforeAll(async () => {
  relayDataDir = mkdtempSync(join(tmpdir(), "sunset-relay-deploy-"));
  configPath = join(relayDataDir, "relay.toml");

  // Boot once on an ephemeral port to pin (port, identity), then shut it
  // down so the page loads find the port closed. Identity is persisted at
  // <data_dir>/identity.key so the second start serves the same x25519.
  const result = await startRelay("127.0.0.1:0");
  relayProcess = result.proc;
  relayAddress = result.addr;

  const m = relayAddress.match(/^ws:\/\/[^:]+:(\d+)/);
  if (!m) {
    throw new Error(`couldn't parse port from address: ${relayAddress}`);
  }
  relayPort = parseInt(m[1]);

  await stopRelay(relayProcess);
  relayProcess = null;
});

test.afterAll(async () => {
  await stopRelay(relayProcess);
  if (relayDataDir) {
    rmSync(relayDataDir, { recursive: true, force: true });
  }
});

test.setTimeout(180_000);
test("client recovers when initial add_relay lands during a deploy outage", async ({
  browser,
}) => {
  // Use the bare `host:port` form — this forces `Client::add_relay`
  // through `sunset-relay-resolver` (HTTP GET /). Canonical
  // `wss://...#x25519=...` URLs short-circuit the resolver and would
  // not exercise this code path.
  const resolverInput = `127.0.0.1:${relayPort}`;
  const url = `/?relay=${encodeURIComponent(resolverInput)}#sunset-deploy`;

  const ctxA = await browser.newContext();
  const ctxB = await browser.newContext();
  const pageA = await ctxA.newPage();
  const pageB = await ctxB.newPage();

  for (const [name, page] of [
    ["A", pageA],
    ["B", pageB],
  ]) {
    page.on("pageerror", (err) =>
      process.stderr.write(`[${name} pageerror] ${err.stack || err}\n`),
    );
    page.on("console", (msg) => {
      if (msg.type() === "error") {
        process.stderr.write(`[${name} console] ${msg.text()}\n`);
      }
    });
  }

  // Open both pages while the relay is DOWN. The resolver fetch will
  // fail (TCP refused on localhost; in production this is a 503 from
  // the proxy with no CORS, hence the browser-blocked report).
  await pageA.goto(url);
  await pageB.goto(url);

  await expect(pageA.getByText("sunset", { exact: true })).toBeVisible({
    timeout: 15_000,
  });
  await expect(pageB.getByText("sunset", { exact: true })).toBeVisible({
    timeout: 15_000,
  });

  // Sanity: input fields are visible (the page renders even when the
  // relay is unreachable; status indicators show the disconnected state).
  const inputA = pageA.getByPlaceholder(/^Message #/);
  const inputB = pageB.getByPlaceholder(/^Message #/);
  await expect(inputA).toBeVisible({ timeout: 15_000 });
  await expect(inputB).toBeVisible({ timeout: 15_000 });

  // Give the initial `add_relay` calls time to fail at least once. With
  // the fix, the frontend transitions to a retry state after this. (The
  // exact label is deliberately unchecked here — the binding behavior
  // we care about is "eventually reaches connected after the relay is
  // back".)
  await pageA.waitForTimeout(2_000);

  // Bring the relay back up. Same port + same data_dir so identity is
  // unchanged; the first successful resolver fetch yields the same
  // canonical wss://...#x25519=<hex> as a fresh boot would.
  const restarted = await startRelay(`127.0.0.1:${relayPort}`);
  relayProcess = restarted.proc;
  expect(restarted.addr).toBe(relayAddress);

  // Wait for both clients to retry-connect + publish their
  // subscriptions before sending the post-deploy message. Without this
  // the test races the reconnect path: A's send can land in its local
  // store before A's subscription has synced to the relay, and the
  // relay then has no way to attribute the message to a subscriber set
  // and forward it.
  await new Promise((r) => setTimeout(r, 15_000));

  const msgPost = `post-deploy from A — ${Date.now()}`;
  await inputA.fill(msgPost);
  await inputA.press("Enter");
  await expect(pageB.getByText(msgPost)).toBeVisible({ timeout: 30_000 });

  // Bidirectional sanity.
  const msgPostB = `post-deploy from B — ${Date.now()}`;
  await inputB.fill(msgPostB);
  await inputB.press("Enter");
  await expect(pageA.getByText(msgPostB)).toBeVisible({ timeout: 30_000 });
});
