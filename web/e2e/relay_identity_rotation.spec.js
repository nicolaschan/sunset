// Acceptance test for the user-reported "stuck disconnected after relay
// restart" failure mode, *under the specific hypothesis* that the relay
// has come back with a different identity (e.g. its data dir was not
// persisted across deploys).
//
// Mechanism under test:
//   * `Client::add_relay(host:port)` registers a `Connectable::Resolving`
//     intent. The supervisor's redial path runs the resolver
//     (`HTTP GET /`) on every attempt, so a new x25519 published by the
//     restarted relay is picked up automatically — the supervisor dials
//     the *new* canonical `wss://host:port#x25519=<NEW_HEX>`.
//   * The Noise handshake then completes against the new key, Hello
//     exchange yields the new peer_id, and the intent flips to
//     Connected without any user reload.
//
// Distinct from `relay_restart.spec.js` (same identity reload via a
// shared data_dir) and `relay_deploy.spec.js` (initial dial during a
// deploy gap, identity unchanged across the gap). Here the identity
// genuinely rotates: the second relay process uses a fresh data_dir,
// so its `<data_dir>/identity.key` is a freshly-generated keypair.

import { test, expect } from "@playwright/test";
import { spawn } from "child_process";
import { mkdtempSync, rmSync } from "fs";
import { tmpdir } from "os";
import { join } from "path";

let relayProcess = null;
let relayAddress = null;
let relayPort = null;
let relayEd25519V1 = null;
let relayX25519V1 = null;
let relayDataDirA = null;
let relayDataDirB = null;
let configPathA = null;
let configPathB = null;

async function startRelay(listenAddr, dataDir, configPath) {
  const fs = await import("fs/promises");
  await fs.writeFile(
    configPath,
    [
      `listen_addr = "${listenAddr}"`,
      `data_dir = "${dataDir}"`,
      `interest_filter = "all"`,
      `identity_secret = "auto"`,
      `peers = []`,
      "",
    ].join("\n"),
  );

  const proc = spawn("sunset-relay", ["--config", configPath], {
    stdio: ["ignore", "pipe", "pipe"],
  });

  // Parse both ed25519 and x25519 from the relay's startup banner so
  // we can assert they truly differ across the rotation. The banner
  // shape is:
  //   sunset-relay starting
  //     ed25519: <hex>
  //     x25519:  <hex>
  //     listen:  ws://<addr>
  //     address: ws://<addr>#x25519=<hex>
  const { addr, ed25519, x25519 } = await new Promise((resolve, reject) => {
    const timer = setTimeout(
      () => reject(new Error("relay didn't print full identity banner within 15s")),
      15_000,
    );
    let buffer = "";
    proc.stdout.on("data", (chunk) => {
      buffer += chunk.toString();
      const ed = buffer.match(/ed25519:\s+([0-9a-f]+)/);
      const x = buffer.match(/x25519:\s+([0-9a-f]+)/);
      const a = buffer.match(/address:\s+(ws:\/\/[^\s]+)/);
      if (ed && x && a) {
        clearTimeout(timer);
        resolve({ ed25519: ed[1], x25519: x[1], addr: a[1] });
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

  return { proc, addr, ed25519, x25519 };
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
  relayDataDirA = mkdtempSync(join(tmpdir(), "sunset-relay-rotA-"));
  relayDataDirB = mkdtempSync(join(tmpdir(), "sunset-relay-rotB-"));
  configPathA = join(relayDataDirA, "relay.toml");
  configPathB = join(relayDataDirB, "relay.toml");

  // Boot the v1 relay on an ephemeral port. We capture (port, identity_A).
  const result = await startRelay("127.0.0.1:0", relayDataDirA, configPathA);
  relayProcess = result.proc;
  relayAddress = result.addr;
  relayEd25519V1 = result.ed25519;
  relayX25519V1 = result.x25519;

  const m = relayAddress.match(/^ws:\/\/[^:]+:(\d+)/);
  if (!m) {
    throw new Error(`couldn't parse port from address: ${relayAddress}`);
  }
  relayPort = parseInt(m[1]);
});

test.afterAll(async () => {
  await stopRelay(relayProcess);
  if (relayDataDirA) {
    rmSync(relayDataDirA, { recursive: true, force: true });
  }
  if (relayDataDirB) {
    rmSync(relayDataDirB, { recursive: true, force: true });
  }
});

test.setTimeout(180_000);
test("chat resumes after relay restarts with a new identity", async ({
  browser,
}) => {
  // Bare `host:port` so `Client::add_relay` routes through the
  // resolver — a canonical `wss://...#x25519=<hex>` would short-circuit
  // resolution and pin us to the original identity, which is precisely
  // the failure mode being tested for.
  const resolverInput = `127.0.0.1:${relayPort}`;
  const url = `/?relay=${encodeURIComponent(resolverInput)}#sunset-rotation`;

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

  // Sanity: relay-mediated chat works against the v1 identity.
  const msgPre = `pre-rotation from A — ${Date.now()}`;
  await inputA.fill(msgPre);
  await inputA.press("Enter");
  await expect(pageB.getByText(msgPre)).toBeVisible({ timeout: 15_000 });

  // Capture the v1 connected peer_pubkey from the supervisor's intent
  // snapshot. This is the relay's ed25519 pubkey as observed by the
  // browser via the live WS+Hello path, so it cross-checks the
  // identity reported in the relay's startup banner.
  function bytesToHex(bytes) {
    return Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join("");
  }
  const v1PeerPubkeyHex = await pageA.evaluate(async () => {
    const intents = await window.sunsetClient.intents();
    const c = intents.find((i) => i.state === "connected");
    if (!c || !c.peer_pubkey) return null;
    return Array.from(c.peer_pubkey, (b) => b.toString(16).padStart(2, "0")).join(
      "",
    );
  });
  expect(
    v1PeerPubkeyHex,
    "v1 connected intent must expose a peer_pubkey",
  ).not.toBeNull();
  expect(v1PeerPubkeyHex).toBe(relayEd25519V1);

  // Kill the v1 relay, then bring up a v2 relay on the same port with a
  // FRESH data_dir. Its `identity.key` is freshly generated → identity_B
  // ≠ identity_A. Resolver reads the new identity on the next dial.
  await stopRelay(relayProcess);
  const restarted = await startRelay(
    `127.0.0.1:${relayPort}`,
    relayDataDirB,
    configPathB,
  );
  relayProcess = restarted.proc;
  const relayEd25519V2 = restarted.ed25519;
  const relayX25519V2 = restarted.x25519;
  // Belt-and-braces: the v2 relay's identity must differ from v1 across
  // every observable axis. If `data_dir = "auto"` ever silently shared
  // an identity file, this would catch it.
  process.stderr.write(
    `[identity-rotation] v1 ed25519=${relayEd25519V1} x25519=${relayX25519V1}\n` +
      `[identity-rotation] v2 ed25519=${relayEd25519V2} x25519=${relayX25519V2}\n`,
  );
  expect(
    relayEd25519V2,
    "v2 ed25519 must differ from v1 — fresh data_dir means fresh keypair",
  ).not.toBe(relayEd25519V1);
  expect(
    relayX25519V2,
    "v2 x25519 must differ from v1 — fresh data_dir means fresh keypair",
  ).not.toBe(relayX25519V1);
  expect(restarted.addr).not.toBe(relayAddress);

  // The supervisor must redial, the resolver must observe the rotated
  // identity, and the intent must reach Connected against the new key.
  // Budget allows for: send-side detection (≤ heartbeat_interval = 15s),
  // backoff (~1 s), resolver fetch + WS connect + Noise handshake +
  // Hello (<1 s on localhost).
  //
  // Critical: the wait condition checks that the intent's peer_pubkey
  // is the V2 ed25519 — *not* just `state === "connected"`. Without
  // this, a stale `connected` snapshot from before the disconnect (or
  // a no-op reconnect that somehow kept the V1 peer_pubkey) would
  // satisfy the wait. We want positive proof the supervisor is now
  // talking to V2.
  async function waitForConnectedToV2(page) {
    await page.waitForFunction(
      (expectedEdHex) => {
        if (!window.sunsetClient || !window.sunsetClient.intents) return false;
        return window.sunsetClient.intents().then((arr) =>
          arr.some((s) => {
            if (s.state !== "connected" || !s.peer_pubkey) return false;
            const hex = Array.from(s.peer_pubkey, (b) =>
              b.toString(16).padStart(2, "0"),
            ).join("");
            return hex === expectedEdHex;
          }),
        );
      },
      relayEd25519V2,
      { timeout: 60_000, polling: 250 },
    );
  }
  await waitForConnectedToV2(pageA);
  await waitForConnectedToV2(pageB);

  // End-to-end: chat works against the new relay identity.
  const msgPost = `post-rotation from A — ${Date.now()}`;
  await inputA.fill(msgPost);
  await inputA.press("Enter");
  await expect(pageB.getByText(msgPost)).toBeVisible({ timeout: 30_000 });

  // Bidirectional sanity.
  const msgPostB = `post-rotation from B — ${Date.now()}`;
  await inputB.fill(msgPostB);
  await inputB.press("Enter");
  await expect(pageA.getByText(msgPostB)).toBeVisible({ timeout: 30_000 });

  await ctxA.close();
  await ctxB.close();
});
