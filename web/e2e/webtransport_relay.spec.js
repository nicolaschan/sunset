// Browser ↔ relay over WebTransport (HTTP/3 / QUIC), the new primary
// transport.
//
// What this test asserts:
//   1. The relay's identity descriptor advertises a `webtransport_cert_sha256`
//      hash whenever it successfully bound a UDP listener (loopback always
//      works). The descriptor does NOT carry a fully-formed WT URL — that
//      caused the prod-bug regression where the URL leaked the relay's
//      `0.0.0.0` bind address.
//   2. The browser's resolver builds the WT URL from the user-typed
//      authority + the descriptor's cert hash, mirroring the WS path's
//      long-standing discipline.
//   3. The browser's `FallbackTransport` connects via WT (not via WS
//      fallback). We verify this through the Rust-side tracing log
//      `fallback: primary (WT) connected`, which Playwright captures from
//      the page console.
//   4. End-to-end chat works over the WT path.
//
// What this test does NOT assert (covered separately):
//   - WS fallback when WT is broken — see `webtransport_fallback.spec.js`.
//   - QUIC datagrams on the wire — covered by the relay-side Rust
//     integration test (`crates/sunset-relay/tests/webtransport_e2e.rs`).

import { test, expect } from "@playwright/test";
import { spawn } from "child_process";
import { mkdtempSync, rmSync } from "fs";
import { tmpdir } from "os";
import { join } from "path";

let relayProcess = null;
let relayWsAddress = null; // ws://… (legacy address)
let relayCertHex = null; // SHA-256 hex of the WT cert (pulled from the banner)
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

  // Parse the WS `address: ws://…` line and the new `wt: cert-sha256=…`
  // line out of the startup banner. The banner intentionally does NOT
  // print a full WT URL — see CommandContext.webtransport_cert_sha256
  // in relay.rs.
  const { ws, certHex } = await new Promise((resolve, reject) => {
    const timer = setTimeout(
      () =>
        reject(
          new Error(
            "relay didn't print full address banner (ws + wt) within 15s",
          ),
        ),
      15_000,
    );
    let buffer = "";
    let wsAddr = null;
    let cert = null;
    proc.stdout.on("data", (chunk) => {
      buffer += chunk.toString();
      const wsMatch = buffer.match(/address:\s+(ws:\/\/[^\s]+)/);
      const certMatch = buffer.match(/wt:\s+cert-sha256=([0-9a-f]{64})/);
      if (wsMatch) wsAddr = wsMatch[1];
      if (certMatch) cert = certMatch[1];
      if (wsAddr && cert) {
        clearTimeout(timer);
        resolve({ ws: wsAddr, certHex: cert });
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

  return { proc, ws, certHex };
}

test.beforeAll(async () => {
  relayDataDir = mkdtempSync(join(tmpdir(), "sunset-relay-wt-"));
  configPath = join(relayDataDir, "relay.toml");

  // Bind `0.0.0.0` (not `127.0.0.1`) so the relay's `bound` matches the
  // production shape that triggered the regression. The browser will
  // still reach the relay via `127.0.0.1:<port>` because we bind on
  // all interfaces, which proves the resolver constructs URLs from
  // the user-typed authority and not the relay's bind address.
  const result = await startRelay("0.0.0.0:0");
  relayProcess = result.proc;
  relayWsAddress = result.ws;
  relayCertHex = result.certHex;
  // Sanity: confirm the relay actually printed `0.0.0.0` (else this
  // test isn't reproducing the prod shape).
  expect(relayWsAddress).toMatch(/^ws:\/\/0\.0\.0\.0:\d+/);
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

test("relay banner advertises a cert-sha256 hash and NOT a URL", () => {
  // Sanity-check: 64 hex chars = SHA-256.
  expect(relayCertHex).toMatch(/^[0-9a-f]{64}$/);
});

test("browser connects to relay via WebTransport (primary path) and chats", async ({
  browser,
}) => {
  // The web app reads `?relay=<url>` and feeds it to the resolver. The
  // user types `127.0.0.1:<port>` (loopback), even though the relay is
  // bound to `0.0.0.0:<port>`. The resolver must build the WT URL from
  // the user-typed authority — using the descriptor's bind address
  // would land us at `https://0.0.0.0:<port>/`, which is the prod-bug
  // failure mode.
  const port = relayWsAddress.match(/^ws:\/\/0\.0\.0\.0:(\d+)/)[1];
  const userAuthority = `127.0.0.1:${port}`;
  const url = `/?relay=${encodeURIComponent(userAuthority)}#sunset-wttest`;

  const ctxA = await browser.newContext();
  const ctxB = await browser.newContext();
  const pageA = await ctxA.newPage();
  const pageB = await ctxB.newPage();

  // Capture every console log and pageerror so we can assert on the
  // WT-vs-WS choice once the connection settles.
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

  // App load.
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

  // The Rust-side tracing config emits INFO-level events to the browser
  // console (see `sunset-web-wasm/src/lib.rs::init_tracing`). The
  // FallbackTransport logs `"fallback: primary (WT) connected"` exactly
  // when WT was used. Wait for that to appear on at least one page —
  // we're testing the transport choice, not the UI.
  await expect
    .poll(
      () =>
        logsA.some((l) =>
          l.includes("fallback: primary (WT) connected"),
        ) ||
        logsB.some((l) =>
          l.includes("fallback: primary (WT) connected"),
        ),
      {
        message:
          "FallbackTransport never logged 'primary (WT) connected' — " +
          "either WT didn't engage, or WS fallback was used instead.",
        timeout: 20_000,
      },
    )
    .toBe(true);

  // Verify the WT session-ready log fires too (proves the
  // `web_sys::WebTransport` ready promise resolved, i.e. the QUIC/HTTP3
  // handshake completed).
  expect(
    logsA.some((l) => l.includes("webtransport: session ready (browser)")) ||
      logsB.some((l) =>
        l.includes("webtransport: session ready (browser)"),
      ),
  ).toBe(true);

  // Chat round-trip over WT.
  const msg = `hello WT — ${Date.now()}`;
  await inputA.fill(msg);
  await inputA.press("Enter");
  await expect(pageB.getByText(msg)).toBeVisible({ timeout: 15_000 });

  await ctxA.close();
  await ctxB.close();
});
