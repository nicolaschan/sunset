// Regression test for PR #13: a client that briefly disconnects without
// reloading the page should catch up on chat sent during the gap once the
// supervisor redials. Relay stays up the whole test — only B's WebSocket
// is dropped — which keeps A live as a producer during the gap. (relay-
// restart.spec.js can't cover this: killing the relay disconnects everyone.)
//
// Mechanism under test:
//   1. B's WebSocket is closed via a page-side hook.
//   2. Engine eventually emits Disconnected → supervisor redials.
//   3. PeerHello calls fan_out_digests_to_peer (PR #13), firing a
//      DigestExchange over B's own published room filter — covering the
//      chat namespace, not just SUBSCRIBE_NAME.
//   4. Relay scans, replies with EventDelivery → B inserts → UI renders.
//
// Without PR #13, step 3 fires only send_bootstrap_digest over
// SUBSCRIBE_NAME, anti-entropy at the 30s tick fires the same, and the
// missed message never arrives until the user reloads the page.
//
// Timing note: disconnect detection in the browser WS transport is
// driven by send-side errors. After ws.close() the socket sits in
// CLOSING for ~2s (the JS WebSocket close handshake) — sends during
// that window are silently dropped, so the engine doesn't notice. Once
// the socket reaches CLOSED, the next heartbeat send (at the 15s
// heartbeat_interval) throws and the engine emits Disconnected. So
// detection takes up to one full heartbeat after the socket transitions
// to CLOSED, ~30s from ws.close() in the worst case. (This is a real
// transport-level bug worth fixing separately; tracked outside this
// PR.) The 60s budget below accommodates that detection delay plus
// the supervisor backoff and digest exchange.

import { test, expect } from "@playwright/test";
import { spawn } from "child_process";
import { mkdtempSync, rmSync } from "fs";
import { tmpdir } from "os";
import { join } from "path";

let relayProcess = null;
let relayAddress = null;
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
  relayDataDir = mkdtempSync(join(tmpdir(), "sunset-catchup-disconnect-"));
  configPath = join(relayDataDir, "relay.toml");

  const result = await startRelay("127.0.0.1:0");
  relayProcess = result.proc;
  relayAddress = result.addr;
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
test("client catches up on chat sent while it was offline", async ({ browser }) => {
  const url = `/?relay=${encodeURIComponent(relayAddress)}#sunset-catchupdisconnect`;

  const ctxA = await browser.newContext();
  const ctxB = await browser.newContext();

  // BrowserContext.setOffline does NOT close existing WebSockets in
  // Chromium — it only blocks new requests. To drop B's WS at will
  // without affecting A or restarting the relay, wrap the WebSocket
  // constructor on B's page so every instance is captured in
  // `window.__sunsetWsInstances`. The test then calls `.close()` on
  // each open instance to force a disconnect; the supervisor's redial
  // creates a new WebSocket which the wrapper also captures.
  await ctxB.addInitScript(() => {
    const RealWS = window.WebSocket;
    const instances = [];
    Object.defineProperty(window, "__sunsetWsInstances", {
      get() {
        return instances;
      },
    });
    function WrappedWS(url, protocols) {
      const ws = new RealWS(url, protocols);
      instances.push(ws);
      return ws;
    }
    WrappedWS.prototype = RealWS.prototype;
    WrappedWS.CONNECTING = RealWS.CONNECTING;
    WrappedWS.OPEN = RealWS.OPEN;
    WrappedWS.CLOSING = RealWS.CLOSING;
    WrappedWS.CLOSED = RealWS.CLOSED;
    window.WebSocket = WrappedWS;
  });

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

  // Sanity: relay-mediated chat is alive before we drop B.
  const msgPre = `pre-disconnect from A — ${Date.now()}`;
  await inputA.fill(msgPre);
  await inputA.press("Enter");
  await expect(pageB.getByText(msgPre)).toBeVisible({ timeout: 15_000 });

  // Drop every WebSocket B currently has open. Closing one socket
  // produces exactly one disconnect/reconnect cycle: the wrapper
  // captures the new WebSocket the supervisor opens during redial,
  // but we don't close that one.
  const closedCount = await pageB.evaluate(() => {
    let n = 0;
    for (const ws of window.__sunsetWsInstances) {
      if (ws.readyState === WebSocket.OPEN) {
        ws.close();
        n += 1;
      }
    }
    return n;
  });
  expect(closedCount).toBeGreaterThan(0);

  // While B is offline, A sends a message that B should later catch up
  // on. A's local insert fires the push to the still-connected relay,
  // so by the time A's UI shows the message the relay has it (or is
  // about to receive it; same assumption as relay_restart.spec.js).
  const msgGap = `gap-message from A — ${Date.now()}`;
  await inputA.fill(msgGap);
  await inputA.press("Enter");
  await expect(pageA.getByText(msgGap)).toBeVisible({ timeout: 15_000 });

  // Confirm B does not have the message yet — without this, a false
  // positive where B never actually went offline would still satisfy
  // the final visibility assertion.
  await expect(pageB.getByText(msgGap)).toBeHidden({ timeout: 3_000 });

  // Wait for B to redial and catch up. Budget covers:
  //   - ~30s WS-close detection (see file header note).
  //   - Supervisor backoff: 1s + jitter.
  //   - Redial + Hello on localhost: <1s.
  //   - PeerHello → fan_out_digests_to_peer → DigestExchange → reply
  //     → insert → UI render: <1s.
  // Observed ~33s. 60s leaves margin without hiding regressions: the
  // pre-PR-#13 case has no catch-up path at all, so it would hang
  // beyond any practical timeout.
  await expect(pageB.getByText(msgGap)).toBeVisible({ timeout: 60_000 });
});
