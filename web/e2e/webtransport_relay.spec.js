// Browser ↔ relay over WebTransport (HTTP/3 / QUIC), the new primary
// transport.
//
// What this test asserts:
//   1. The relay's identity descriptor advertises a `webtransport_address`
//      whenever it successfully bound a UDP listener (loopback always works).
//   2. The browser's `Client` resolves that descriptor and uses the WT URL
//      as the canonical address.
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
//     A datagram-aware browser e2e is a follow-up; this PR proves the
//     wire path works end-to-end.

import { test, expect } from "@playwright/test";
import { spawn } from "child_process";
import { mkdtempSync, rmSync } from "fs";
import { tmpdir } from "os";
import { join } from "path";

let relayProcess = null;
let relayWsAddress = null; // ws://… (legacy address)
let relayWtAddress = null; // wt://… with #x25519=&cert-sha256= fragment
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

  // Parse both the WS `address: ws://…` line and the new `wt: wt://…`
  // line out of the startup banner. Both must arrive within the
  // banner-print window; if WT bind failed, the banner says
  // `wt: (disabled — UDP bind failed)` and we'll see that here too.
  const { ws, wt } = await new Promise((resolve, reject) => {
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

  return { proc, ws, wt };
}

test.beforeAll(async () => {
  relayDataDir = mkdtempSync(join(tmpdir(), "sunset-relay-wt-"));
  configPath = join(relayDataDir, "relay.toml");

  const result = await startRelay("127.0.0.1:0");
  relayProcess = result.proc;
  relayWsAddress = result.ws;
  relayWtAddress = result.wt;
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

test("relay banner advertises a wt:// URL with cert-sha256 fragment", () => {
  expect(relayWtAddress).toBeTruthy();
  expect(relayWtAddress).toMatch(/^wt:\/\/127\.0\.0\.1:\d+/);
  expect(relayWtAddress).toContain("#x25519=");
  expect(relayWtAddress).toContain("&cert-sha256=");
});

test("browser connects to relay via WebTransport (primary path) and chats", async ({
  browser,
}) => {
  // The web app reads `?relay=<url>` and feeds it to the resolver. We
  // pass the WS host:port (no scheme) so the resolver fetches the
  // descriptor JSON and picks `webtransport_address` as the primary —
  // matching the real production flow where the user types a relay
  // hostname.
  const hostPort = relayWsAddress
    .replace(/^ws:\/\//, "")
    .split("#")[0];
  const url = `/?relay=${encodeURIComponent(hostPort)}#sunset-wttest`;

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
