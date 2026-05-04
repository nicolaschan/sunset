// Reproduces the user-reported "stuck disconnected after relay restart"
// failure mode WITH the production-shaped failure signal: a
// Cloudflare-style proxy in front of the relay returns HTTP 504 during
// the relay-down gap, and forcibly drops in-flight connections at the
// moment the relay goes away.
//
// Why this matters separately from `relay_restart.spec.js`:
//   * `relay_restart` exercises a same-port same-identity restart with no
//     proxy. The browser sees TCP RST → fast `recv ws closed` → quick
//     reconnect.
//   * Production has Cloudflare in front of the relay. During a
//     deploy/restart Cloudflare returns 504 (after ~30 s upstream wait)
//     for both the WS upgrade and the resolver's `GET /`. The browser
//     log the user reported showed exactly this sequence:
//        peer disconnected reason = recv reliable: ws closed
//        GET wss://relay.sunset.chat/ HTTP/1.1 504 Gateway Timeout 30023ms
//        peer disconnected reason = send reliable: ws closed
//        Firefox can't establish a connection to wss://relay.sunset.chat/
//
// This spec puts a tiny Node proxy between the browser and the relay
// that:
//   * In "alive" mode forwards HTTP and WebSocket transparently.
//   * In "504" mode answers every HTTP request and every WS upgrade
//     attempt with `HTTP/1.1 504 Gateway Timeout`, AND tears down any
//     in-flight forward (so existing browser WS connections see the
//     close, just as they would when CF can't reach the upstream).
//
// The relay is restarted with the same data_dir, so its identity is
// preserved — this test isolates the 504 path from the (separately
// covered) identity-rotation path.

import { test, expect } from "@playwright/test";
import { spawn } from "child_process";
import http from "http";
import net from "net";
import { mkdtempSync, rmSync } from "fs";
import { tmpdir } from "os";
import { join } from "path";

let relayProcess = null;
let relayBareAddr = null; // ws://127.0.0.1:<relayPort>
let relayPort = null;
let relayDataDir = null;
let configPath = null;
let proxy = null;
let proxyPort = null;
// Buffered relay stderr — used post-restart to assert the relay isn't
// logging the prod symptom (`promote failed: noise responder: raw
// transport error: …`). The user-reported prod log showed many such
// failures clustered at restart; if we reproduce that here, this
// buffer will surface it.
let relayStderr = "";

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
      const text = chunk.toString();
      relayStderr += text;
      process.stderr.write(`[relay] ${text}`);
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

// Minimal HTTP+WebSocket forward proxy with a `mode` switch:
//   "alive" → forward transparently to (upstreamHost, upstreamPort)
//   "504"   → respond `HTTP/1.1 504 Gateway Timeout` to all HTTP and
//             all WS upgrade attempts; close existing forwards.
//
// Models Cloudflare in front of the sunset-relay. We don't try to
// emulate CF's 30 s upstream-wait timeout — we fail fast and assert the
// supervisor recovers; if there's a code-path bug it surfaces either
// way.
class Fake504Proxy {
  constructor(upstreamHost, upstreamPort) {
    this.upstreamHost = upstreamHost;
    this.upstreamPort = upstreamPort;
    this.mode = "alive";
    this.activeForwards = new Set();

    this.server = http.createServer();

    this.server.on("request", (req, res) => {
      if (this.mode === "504") {
        res.writeHead(504, { "Content-Type": "text/plain" });
        res.end("Gateway Timeout");
        return;
      }
      const upstreamReq = http.request(
        {
          host: this.upstreamHost,
          port: this.upstreamPort,
          method: req.method,
          path: req.url,
          headers: req.headers,
        },
        (upstreamRes) => {
          res.writeHead(upstreamRes.statusCode, upstreamRes.headers);
          upstreamRes.pipe(res);
        },
      );
      upstreamReq.on("error", () => {
        if (!res.headersSent) {
          res.writeHead(504);
          res.end();
        } else {
          res.destroy();
        }
      });
      req.pipe(upstreamReq);
    });

    this.server.on("upgrade", (req, clientSocket, head) => {
      if (this.mode === "504") {
        clientSocket.write(
          "HTTP/1.1 504 Gateway Timeout\r\n" +
            "Content-Length: 0\r\n" +
            "Connection: close\r\n" +
            "\r\n",
        );
        clientSocket.destroy();
        return;
      }
      const upstreamSocket = net.connect(
        this.upstreamPort,
        this.upstreamHost,
      );
      let opened = false;
      upstreamSocket.on("connect", () => {
        opened = true;
        const lines = [
          `${req.method} ${req.url} HTTP/${req.httpVersion}`,
          ...Object.entries(req.headers).map(([k, v]) => `${k}: ${v}`),
          "",
          "",
        ];
        upstreamSocket.write(lines.join("\r\n"));
        if (head && head.length) {
          upstreamSocket.write(head);
        }
        clientSocket.pipe(upstreamSocket);
        upstreamSocket.pipe(clientSocket);
        const pair = { clientSocket, upstreamSocket };
        this.activeForwards.add(pair);
        const cleanup = () => {
          this.activeForwards.delete(pair);
        };
        clientSocket.on("close", cleanup);
        upstreamSocket.on("close", cleanup);
        clientSocket.on("error", () => {});
        upstreamSocket.on("error", () => {});
      });
      upstreamSocket.on("error", () => {
        if (!opened) {
          clientSocket.write(
            "HTTP/1.1 504 Gateway Timeout\r\n" +
              "Content-Length: 0\r\n" +
              "Connection: close\r\n" +
              "\r\n",
          );
          clientSocket.destroy();
        }
      });
    });
  }

  setMode(mode) {
    this.mode = mode;
    if (mode === "504") {
      // Tear down everything that's currently in-flight so the browser
      // observes the same `recv ws closed` signal it would see when CF
      // can no longer reach the upstream.
      for (const { clientSocket, upstreamSocket } of this.activeForwards) {
        clientSocket.destroy();
        upstreamSocket.destroy();
      }
      this.activeForwards.clear();
    }
  }

  listen() {
    return new Promise((resolve) => {
      this.server.listen(0, "127.0.0.1", () => {
        resolve(this.server.address().port);
      });
    });
  }

  close() {
    return new Promise((resolve) => this.server.close(resolve));
  }
}

test.beforeAll(async () => {
  relayDataDir = mkdtempSync(join(tmpdir(), "sunset-relay-504-"));
  configPath = join(relayDataDir, "relay.toml");

  const result = await startRelay("127.0.0.1:0");
  relayProcess = result.proc;
  relayBareAddr = result.addr;
  const m = relayBareAddr.match(/^ws:\/\/[^:]+:(\d+)/);
  if (!m) throw new Error(`couldn't parse port: ${relayBareAddr}`);
  relayPort = parseInt(m[1]);

  proxy = new Fake504Proxy("127.0.0.1", relayPort);
  proxyPort = await proxy.listen();
});

test.afterAll(async () => {
  await stopRelay(relayProcess);
  if (proxy) {
    await proxy.close();
  }
  if (relayDataDir) {
    rmSync(relayDataDir, { recursive: true, force: true });
  }
});

// Whole-test budget: 3 s gap + ~10 s recovery + ~1 s B→A + setup
// overhead is typically 15–20 s. 60 s is ~3× that.
test.setTimeout(60_000);
test("chat resumes after relay restart while proxy returns 504", async ({
  browser,
}) => {
  // Talk to the proxy, not the relay directly. Bare host:port form so
  // the resolver runs every dial — exercising the same code paths as
  // the production deploy.
  const resolverInput = `127.0.0.1:${proxyPort}`;
  const url = `/?relay=${encodeURIComponent(resolverInput)}#sunset-504`;

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
    await page.addInitScript(() => {
      window.SUNSET_TEST = true;
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

  // Run id mixed into every message so payloads cannot collide with
  // earlier-or-concurrent traffic and msgPre cannot be confused with
  // msgPost.
  const runId = `${Date.now()}-${Math.floor(Math.random() * 1e12).toString(36)}`;
  const nonce = () => Math.floor(Math.random() * 1e15).toString(36);

  // Pre-restart sanity.
  const msgPre = `[e2e ${runId}] pre-504-A nonce=${nonce()}`;
  expect(
    await pageB.getByText(msgPre).count(),
    "B must not see msgPre before A sends it",
  ).toBe(0);
  await inputA.fill(msgPre);
  await inputA.press("Enter");
  await expect(pageB.getByText(msgPre)).toBeVisible({ timeout: 15_000 });

  // Capture B's `performance.timeOrigin` before the kill so we can
  // assert it's unchanged afterwards (proves the page wasn't reloaded
  // — `timeOrigin` only changes when the document is fresh).
  const pageBOriginBefore = await pageB.evaluate(() => performance.timeOrigin);

  // Simulate the production failure: kill the relay AND switch the
  // proxy to 504 mode. The browsers' existing WS forwards are torn
  // down by the proxy, so they observe the close immediately. Every
  // subsequent connect attempt — both the resolver's HTTP GET / and
  // the WS upgrade — gets a 504 response from the proxy.
  proxy.setMode("504");
  await stopRelay(relayProcess);

  // Hold the gap open long enough that the supervisor's first
  // post-disconnect retry definitely lands during 504 mode and fails
  // transiently. That failure is exactly the path we're testing —
  // `spawn_dial`'s background task transitions the intent to
  // `Backoff` while the run loop is parked on a stale
  // `pending::<()>` future. Without the wake-on-Backoff fix, the
  // run loop never re-arms its sleep timer and the supervisor stays
  // stuck even after the proxy returns to alive mode.
  await pageA.waitForTimeout(3_000);

  // Bring the relay back on the same port + same data_dir → same
  // identity. Restore the proxy to forwarding mode. From the
  // browser's perspective, the relay just "came back".
  const restarted = await startRelay(`127.0.0.1:${relayPort}`);
  relayProcess = restarted.proc;
  expect(restarted.addr).toBe(relayBareAddr);
  proxy.setMode("alive");

  // Deliberately do NOT poll `intents()` between recovery and the
  // message sends below. Each `intents()` call sends a
  // `SupervisorCommand::Snapshot` that wakes the supervisor's
  // `cmd_rx.recv()` arm, which would mask the exact liveness bug
  // this test is meant to catch (run loop parked on a stale
  // `pending::<()>()` while a background `spawn_dial` schedules a
  // fresh `Backoff`).
  //
  // The user's UI doesn't poll; it only subscribes via
  // `on_intent_changed`. Message-arrival on B is therefore the
  // load-bearing canary: if A's supervisor were stuck, A could not
  // push msgPost to the relay and B would never see it inside the
  // expect timeout.
  const msgPost = `[e2e ${runId}] post-504-A nonce=${nonce()}`;
  expect(
    await pageB.getByText(msgPost).count(),
    "B must not see msgPost before A sends it",
  ).toBe(0);
  await inputA.fill(msgPost);
  await inputA.press("Enter");
  // Recovery budget: 3 s gap + worst-case backoff escalation
  // (1 s + 2 s + 4 s = 7 s) + handshake + push + B-side render —
  // typically 9–11 s end-to-end on a dev box. 30 s leaves ~3× slack
  // for CI variance without being so loose that a real regression
  // takes a minute to surface.
  await expect(pageB.getByText(msgPost)).toBeVisible({ timeout: 30_000 });

  // Cross-checks: B was not reloaded and pre-restart history is
  // intact. Both prove that the message arrived via the live engine
  // → store → UI subscription path on the same browser context that
  // was open before the kill.
  await expect(
    pageB.getByText(msgPre),
    "msgPre must still be visible on B (proves no reload erased history)",
  ).toBeVisible();
  const pageBOriginAfter = await pageB.evaluate(() => performance.timeOrigin);
  expect(
    pageBOriginAfter,
    "B's performance.timeOrigin must be unchanged across the restart (proves no reload)",
  ).toBe(pageBOriginBefore);

  // Bidirectional sanity.
  const msgPostB = `[e2e ${runId}] post-504-B nonce=${nonce()}`;
  expect(
    await pageA.getByText(msgPostB).count(),
    "A must not see msgPostB before B sends it",
  ).toBe(0);
  await inputB.fill(msgPostB);
  await inputB.press("Enter");
  // Both sides are reconnected to the (now-alive) relay by this
  // point, so this is normal relay-mediated chat — sub-second on
  // localhost.
  await expect(pageA.getByText(msgPostB)).toBeVisible({ timeout: 15_000 });

  // Reproducer check: the user's prod log showed many
  // `promote failed: noise responder: raw transport error: ws recv: …
  // (Connection reset / ws closed by peer)` lines on the relay
  // immediately after restart. By this point both pages are connected
  // and bidirectional chat works — so any promote failures we'd
  // observed must be from connection attempts that happened during
  // recovery. A small number of "ws closed by peer" lines can fire
  // legitimately when the supervisor times out a hung dial and drops
  // the WS; tolerate up to 2 of those per page (4 total). But ANY
  // "Connection reset" on the relay side should be zero — that's the
  // prod symptom we're hunting.
  const promoteResetCount = (
    relayStderr.match(/promote failed:[^\n]*Connection reset/g) || []
  ).length;
  const promoteCloseCount = (
    relayStderr.match(/promote failed:[^\n]*ws closed by peer/g) || []
  ).length;
  expect(
    promoteResetCount,
    `relay logged ${promoteResetCount} "Connection reset" promote failures (prod symptom). Stderr tail:\n${relayStderr.slice(-2000)}`,
  ).toBe(0);
  expect(
    promoteCloseCount,
    `relay logged ${promoteCloseCount} "ws closed by peer" promote failures. Stderr tail:\n${relayStderr.slice(-2000)}`,
  ).toBeLessThanOrEqual(4);

  await ctxA.close();
  await ctxB.close();
});
