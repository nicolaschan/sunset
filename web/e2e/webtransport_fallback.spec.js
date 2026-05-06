// FallbackTransport's WS fallback path: when WebTransport fails (e.g.
// a stale or wrong cert hash), the browser must transparently retry
// over WebSocket and the user shouldn't notice anything more than a
// brief connect delay.
//
// Setup:
//   - Spawn a relay that binds both WS (TCP) and WT (UDP) on the same
//     port — same as production / `webtransport_relay.spec.js`.
//   - Hand the browser a *canonical* relay URL of the form
//     `wt://host:port#x25519=<real>&cert-sha256=<BOGUS>`. The
//     `parse_input` parser passes canonical URLs (those starting with
//     `#x25519=`) straight through to the engine without an HTTP fetch
//     to the descriptor — so the browser uses our injected URL verbatim.
//   - The WT handshake will fail (cert hash doesn't match the actual
//     self-signed cert). The Rust-side `FallbackTransport::connect`
//     catches the failure, scheme-rewrites to `ws://`, and retries.
//
// We assert:
//   1. The "fallback: primary (WT) failed, trying fallback (WS)" log
//      appears (the fallback path engaged).
//   2. The "fallback: WS fallback connected after WT failure" log
//      appears (the retry succeeded).
//   3. End-to-end chat round-trips — the user sees a working app.

import { test, expect } from "@playwright/test";
import { spawn } from "child_process";
import { mkdtempSync, rmSync } from "fs";
import { tmpdir } from "os";
import { join } from "path";

let relayProcess = null;
let relayWsAddress = null;
let relayWtAddress = null;
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

  const { ws, wt } = await new Promise((resolve, reject) => {
    const timer = setTimeout(
      () => reject(new Error("relay banner not seen within 15s")),
      15_000,
    );
    let buffer = "";
    let wsAddr = null;
    let wtAddr = null;
    proc.stdout.on("data", (chunk) => {
      buffer += chunk.toString();
      const wsMatch = buffer.match(/address:\s+(ws:\/\/[^\s]+)/);
      const wtMatch = buffer.match(/wt:\s+(wt:\/\/[^\s]+)/);
      if (wsMatch) wsAddr = wsMatch[1];
      if (wtMatch) wtAddr = wtMatch[1];
      if (wsAddr && wtAddr) {
        clearTimeout(timer);
        resolve({ ws: wsAddr, wt: wtAddr });
      }
    });
    proc.stderr.on("data", (chunk) =>
      process.stderr.write(`[relay] ${chunk}`),
    );
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

  return { proc, ws, wt };
}

test.beforeAll(async () => {
  relayDataDir = mkdtempSync(join(tmpdir(), "sunset-relay-wt-fallback-"));
  configPath = join(relayDataDir, "relay.toml");
  const r = await startRelay("127.0.0.1:0");
  relayProcess = r.proc;
  relayWsAddress = r.ws;
  relayWtAddress = r.wt;
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

test("browser falls back from WT to WS when the cert hash is wrong", async ({
  browser,
}) => {
  // Extract the actual `x25519=<hex>` from the WS address (its only
  // fragment key), then build a WT URL with a deliberately-wrong
  // cert-sha256 fragment so the WT handshake fails and the
  // FallbackTransport drops to WS.
  const x25519Match = relayWsAddress.match(/#x25519=([0-9a-f]{64})/);
  expect(x25519Match).toBeTruthy();
  const x25519Hex = x25519Match[1];
  const hostPort = relayWsAddress
    .replace(/^ws:\/\//, "")
    .split("#")[0];
  const bogusCertHex = "ee".repeat(32); // any value other than the real one
  const sabotagedWtUrl =
    `wt://${hostPort}#x25519=${x25519Hex}&cert-sha256=${bogusCertHex}`;

  const url = `/?relay=${encodeURIComponent(sabotagedWtUrl)}#sunset-wtfallback`;

  const ctxA = await browser.newContext();
  const ctxB = await browser.newContext();
  const pageA = await ctxA.newPage();
  const pageB = await ctxB.newPage();

  const logsA = [];
  const logsB = [];
  for (const [name, page, sink] of [
    ["A", pageA, logsA],
    ["B", pageB, logsB],
  ]) {
    page.on("pageerror", (err) =>
      process.stderr.write(`[${name} pageerror] ${err.stack || err}\n`),
    );
    page.on("console", (msg) => {
      const text = msg.text();
      sink.push(text);
      if (msg.type() === "error") {
        process.stderr.write(`[${name} console] ${text}\n`);
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

  // Fallback path engaged.
  await expect
    .poll(
      () =>
        logsA.some((l) =>
          l.includes("fallback: primary (WT) failed, trying fallback (WS)"),
        ) ||
        logsB.some((l) =>
          l.includes("fallback: primary (WT) failed, trying fallback (WS)"),
        ),
      {
        message:
          "FallbackTransport never logged the WT-failure-before-WS-fallback " +
          "warning — either WT didn't fail (cert mismatch should always fail) " +
          "or the fallback path didn't engage.",
        timeout: 20_000,
      },
    )
    .toBe(true);

  // WS fallback connected.
  await expect
    .poll(
      () =>
        logsA.some((l) =>
          l.includes("fallback: WS fallback connected after WT failure"),
        ) ||
        logsB.some((l) =>
          l.includes("fallback: WS fallback connected after WT failure"),
        ),
      { timeout: 20_000 },
    )
    .toBe(true);

  // End-to-end chat works post-fallback. Same user-visible contract as
  // a healthy WT or healthy WS-only relay.
  const msg = `WT-failed → WS path — ${Date.now()}`;
  await inputA.fill(msg);
  await inputA.press("Enter");
  await expect(pageB.getByText(msg)).toBeVisible({ timeout: 15_000 });

  await ctxA.close();
  await ctxB.close();
});
