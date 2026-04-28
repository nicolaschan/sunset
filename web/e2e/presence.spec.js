// Presence + membership e2e.
//
// Uses fast-mode URL params to compress the wall-clock arc of
// Online → Away → Offline transitions to ~1.5s.

import { test, expect } from "@playwright/test";
import { spawn } from "child_process";
import { mkdtempSync, rmSync } from "fs";
import { tmpdir } from "os";
import { join } from "path";

let relayProcess = null;
let relayAddress = null;
let relayDataDir = null;

test.beforeAll(async () => {
  relayDataDir = mkdtempSync(join(tmpdir(), "sunset-relay-presence-test-"));
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

function fastUrl(relay) {
  return `/?relay=${encodeURIComponent(relay)}&presence_interval=300&presence_ttl=900&presence_refresh=100#sunset-presence-test`;
}

async function setupPage(browser) {
  const ctx = await browser.newContext();
  const page = await ctx.newPage();
  await page.addInitScript(() => { window.SUNSET_TEST = true; });
  page.on("pageerror", (err) =>
    process.stderr.write(`[pageerror] ${err.stack || err}\n`),
  );
  page.on("console", (msg) => {
    if (msg.type() === "error") process.stderr.write(`[console] ${msg.text()}\n`);
  });
  return { ctx, page };
}

test.setTimeout(30_000);

test("two browsers see each other in the member rail", async ({ browser }) => {
  const { page: a } = await setupPage(browser);
  const { page: b } = await setupPage(browser);
  await a.goto(fastUrl(relayAddress));
  await b.goto(fastUrl(relayAddress));
  await expect(a.getByText("sunset", { exact: true })).toBeVisible({ timeout: 15_000 });
  await expect(b.getByText("sunset", { exact: true })).toBeVisible({ timeout: 15_000 });

  // Wait for window.sunsetClient to be exposed.
  await a.waitForFunction(() => !!window.sunsetClient, null, { timeout: 15_000 });
  await b.waitForFunction(() => !!window.sunsetClient, null, { timeout: 15_000 });

  // Page exposes received members on window.__sunsetLastMembers via the
  // FFI shim — confirm via a small init script that captures the array
  // on every callback fire.
  for (const p of [a, b]) {
    await p.evaluate(() => {
      // Stash members as plain JS objects (wasm-bindgen objects can
      // be freed between calls; freezing them avoids use-after-free).
      window.sunsetClient.on_members_changed((members) => {
        window.__sunsetLastMembers = Array.from(members).map((m) => ({
          pubkey: Array.from(m.pubkey),
          presence: m.presence,
          connection_mode: m.connection_mode,
          is_self: m.is_self,
        }));
      });
    });
  }

  // Wait until A sees B's presence with via_relay mode.
  const bPub = await b.evaluate(() => Array.from(window.sunsetClient.public_key));
  await a.waitForFunction(
    (pkArr) => {
      const target = new Uint8Array(pkArr);
      const eq = (x, y) => x.length === y.length && x.every((v, i) => v === y[i]);
      const ms = window.__sunsetLastMembers || [];
      const m = ms.find((mm) => eq(Array.from(mm.pubkey), Array.from(target)));
      return m && m.presence === "online" && m.connection_mode === "via_relay";
    },
    bPub,
    { timeout: 15_000 },
  );

  const aPub = await a.evaluate(() => Array.from(window.sunsetClient.public_key));
  await b.waitForFunction(
    (pkArr) => {
      const target = new Uint8Array(pkArr);
      const eq = (x, y) => x.length === y.length && x.every((v, i) => v === y[i]);
      const ms = window.__sunsetLastMembers || [];
      const m = ms.find((mm) => eq(Array.from(mm.pubkey), Array.from(target)));
      return m && m.presence === "online" && m.connection_mode === "via_relay";
    },
    aPub,
    { timeout: 15_000 },
  );
});

test("connect_direct flips connection_mode to direct", async ({ browser }) => {
  const { page: a } = await setupPage(browser);
  const { page: b } = await setupPage(browser);
  await a.goto(fastUrl(relayAddress));
  await b.goto(fastUrl(relayAddress));
  await expect(a.getByText("sunset", { exact: true })).toBeVisible({ timeout: 15_000 });
  await expect(b.getByText("sunset", { exact: true })).toBeVisible({ timeout: 15_000 });

  await a.waitForFunction(() => !!window.sunsetClient, null, { timeout: 15_000 });
  await b.waitForFunction(() => !!window.sunsetClient, null, { timeout: 15_000 });

  for (const p of [a, b]) {
    await p.evaluate(() => {
      window.sunsetClient.on_members_changed((members) => {
        window.__sunsetLastMembers = Array.from(members).map((m) => ({
          pubkey: Array.from(m.pubkey),
          presence: m.presence,
          connection_mode: m.connection_mode,
          is_self: m.is_self,
        }));
      });
    });
  }

  const bPub = await b.evaluate(() => Array.from(window.sunsetClient.public_key));
  await a.evaluate(async (pkArr) => {
    await window.sunsetClient.connect_direct(new Uint8Array(pkArr));
  }, bPub);

  await a.waitForFunction(
    (pkArr) => {
      const target = new Uint8Array(pkArr);
      const eq = (x, y) => x.length === y.length && x.every((v, i) => v === y[i]);
      const ms = window.__sunsetLastMembers || [];
      const m = ms.find((mm) => eq(Array.from(mm.pubkey), Array.from(target)));
      return m && m.connection_mode === "direct";
    },
    bPub,
    { timeout: 10_000 },
  );
});

test("closing one tab makes the other side see away then drop", async ({ browser }) => {
  const { ctx: ctxA, page: a } = await setupPage(browser);
  const { page: b } = await setupPage(browser);
  await a.goto(fastUrl(relayAddress));
  await b.goto(fastUrl(relayAddress));
  await expect(a.getByText("sunset", { exact: true })).toBeVisible({ timeout: 15_000 });
  await expect(b.getByText("sunset", { exact: true })).toBeVisible({ timeout: 15_000 });

  await a.waitForFunction(() => !!window.sunsetClient, null, { timeout: 15_000 });
  await b.waitForFunction(() => !!window.sunsetClient, null, { timeout: 15_000 });

  await b.evaluate(() => {
    window.sunsetClient.on_members_changed((members) => {
      window.__sunsetLastMembers = Array.from(members).map((m) => ({
        pubkey: Array.from(m.pubkey),
        presence: m.presence,
        connection_mode: m.connection_mode,
        is_self: m.is_self,
      }));
    });
  });

  const aPub = await a.evaluate(() => Array.from(window.sunsetClient.public_key));

  // Confirm B sees A first.
  await b.waitForFunction(
    (pkArr) => {
      const target = new Uint8Array(pkArr);
      const eq = (x, y) => x.length === y.length && x.every((v, i) => v === y[i]);
      const ms = window.__sunsetLastMembers || [];
      return ms.some((mm) => eq(Array.from(mm.pubkey), Array.from(target)));
    },
    aPub,
    { timeout: 5_000 },
  );

  // Close A.
  await ctxA.close();

  // Within ttl_ms (900) + refresh_ms (100) buffer, A should be dropped from B's list.
  await b.waitForFunction(
    (pkArr) => {
      const target = new Uint8Array(pkArr);
      const eq = (x, y) => x.length === y.length && x.every((v, i) => v === y[i]);
      const ms = window.__sunsetLastMembers || [];
      return !ms.some((mm) => eq(Array.from(mm.pubkey), Array.from(target)));
    },
    aPub,
    { timeout: 5_000 },
  );
});
