// IndexedDB persistence + reset.
//
// Verifies the contract introduced by switching the in-browser
// `sunset-store` from `MemoryStore` to `IndexedDbStore`:
//
//   1. A message a user sent before reloading the page is still
//      visible after the reload (i.e. the local store is durable).
//   2. The "reset local state" button in the settings popover wipes
//      the IndexedDB-backed store, so after reload the previously-sent
//      message is gone (alongside the localStorage canary that the
//      existing reset test in `ui_tweaks.spec.js` checks for).
//
// This test boots its own relay so it doesn't depend on routing
// across the rest of the suite. The *single-browser* persistence path
// (the user reloads their own tab) only needs the local store, but
// having a relay around keeps the UI happy (no error toasts) while
// we exercise reload behavior.

import { spawn } from "child_process";
import { mkdtempSync, rmSync } from "fs";
import { tmpdir } from "os";
import { join } from "path";

import { expect, test } from "@playwright/test";

let relayProcess = null;
let relayAddress = null;
let relayDataDir = null;

test.beforeAll(async () => {
  relayDataDir = mkdtempSync(join(tmpdir(), "sunset-relay-idb-test-"));
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

test.afterAll(() => {
  if (relayProcess && relayProcess.exitCode === null) {
    relayProcess.kill("SIGTERM");
  }
  if (relayDataDir) {
    rmSync(relayDataDir, { recursive: true, force: true });
  }
});

test.setTimeout(60_000);

async function openChat(page, hash) {
  const url = `/?relay=${encodeURIComponent(relayAddress)}#${hash}`;
  page.on("pageerror", (err) =>
    process.stderr.write(`[pageerror] ${err.stack || err}\n`),
  );
  await page.addInitScript(() => {
    window.SUNSET_TEST = true;
  });
  await page.goto(url);
  await expect(page.getByText("sunset", { exact: true })).toBeVisible({
    timeout: 15_000,
  });
  const composer = page.getByPlaceholder(/^Message #/);
  await expect(composer).toBeVisible({ timeout: 15_000 });
  return composer;
}

async function openSettings(page, testInfo) {
  const isMobile = testInfo.project.name === "mobile-chrome";
  if (isMobile) {
    await page.getByTestId("phone-rooms-toggle").click();
    await page.getByTestId("channels-room-title").click();
  }
  await page.getByTestId("you-row").click();
}

test("messages a user sent persist across a page reload", async ({
  browser,
}) => {
  // Use a single browser context (cookie + storage scope) so the
  // reload reuses the same per-origin IndexedDB database. Two pages
  // in two contexts would each see a fresh DB.
  const ctx = await browser.newContext();
  const page = await ctx.newPage();

  const composer = await openChat(page, "idb-persistence-reload");
  // Use a unique stamp so a flake on a stale dist couldn't hide a
  // false positive from leftover state.
  const message = `idb-persists-${Date.now()}`;

  await composer.fill(message);
  await composer.press("Enter");
  await expect(page.getByText(message)).toBeVisible({ timeout: 15_000 });

  // Snapshot raw IndexedDB contents both before and after the reload
  // — a baseline at the storage layer, so a downstream UI flake can
  // be cleanly distinguished from a storage regression.
  const beforeReload = await readDbCounts(page);
  expect(beforeReload.entries).toBeGreaterThan(0);
  expect(beforeReload.blobs).toBeGreaterThan(0);

  await page.reload();

  const afterReload = await readDbCounts(page);
  expect(afterReload.entries).toBe(beforeReload.entries);
  expect(afterReload.blobs).toBe(beforeReload.blobs);

  // After the reload the new wasm Client opens the same per-origin
  // IndexedDB database, replays the historical messages, and the UI
  // re-renders the row. Use `attached` instead of `visible` so we
  // don't trip on a row that's correctly in the DOM but happens to
  // be scrolled out of view (the chat panel doesn't always
  // auto-scroll-to-bottom after a reload).
  await expect(page.getByText(message)).toBeAttached({ timeout: 30_000 });

  await ctx.close();
});

test("settings reset wipes the local IndexedDB store", async ({
  browser,
}, testInfo) => {
  const ctx = await browser.newContext();
  const page = await ctx.newPage();

  const composer = await openChat(page, "idb-reset");
  const message = `idb-reset-victim-${Date.now()}`;

  await composer.fill(message);
  await composer.press("Enter");
  await expect(page.getByText(message)).toBeAttached({ timeout: 15_000 });

  // Open settings + click reset. The reset handler clears storage,
  // wipes the IndexedDB-backed store, and triggers `location.reload`.
  // We then poll the URL until the fragment clears as a proof the
  // reset's reload landed (the click itself returns synchronously
  // and the wasm-side IDB delete is async).
  await openSettings(page, testInfo);
  await expect(page.getByTestId("settings-reset")).toBeVisible({
    timeout: 5_000,
  });
  // Sanity: the message landed in IndexedDB (entries + at least one
  // blob). This pins down what we expect to *not* be there post-reset.
  const beforeReset = await readDbCounts(page);
  expect(beforeReset.entries).toBeGreaterThan(0);
  expect(beforeReset.blobs).toBeGreaterThan(0);

  // Mark the current page so we can detect when the reset's reload
  // has produced a fresh document. `window.__resetMarker` is wiped
  // by `location.reload()` — checking for its absence is a robust
  // post-reload checkpoint regardless of the async-IDB-delete
  // ordering inside `resetLocalStateAndReload`.
  await page.evaluate(() => {
    window.__resetMarker = "before-reset";
  });
  await page.getByTestId("settings-reset").click({ force: true });
  await page.waitForFunction(
    () =>
      typeof window !== "undefined" &&
      typeof window.__resetMarker === "undefined" &&
      // The reset path clears the URL fragment before reloading, so
      // the post-reset page lands on the LandingView (not the chat
      // shell). Wait for that to appear so we know the new app is
      // mounted before navigating again.
      document.querySelector('[data-testid="landing-view"]') !== null,
    null,
    { timeout: 15_000 },
  );
  // The reset path also clears the URL fragment via
  // `history.replaceState` immediately before `location.reload()`,
  // so post-reload the hash is empty.
  expect(new URL(page.url()).hash).toBe("");

  // Direct IndexedDB inspection: the previously-stored entries must
  // be gone. We do this BEFORE re-joining the room so the relay
  // hasn't had a chance to push the historical message back over
  // sync — this isolates the "local state was wiped" assertion from
  // the "sync re-populates" pathway.
  //
  // The new page may already have re-created an empty `sunset-store`
  // database via `Client.open` — that's fine; what matters is that
  // it contains no carry-over from the pre-reset session.
  const afterReset = await readDbCounts(page);
  expect(afterReset.entries).toBe(0);

  await ctx.close();
});

/// Read `(entries.count, blobs.count)` from the in-page IndexedDB
/// database. Used by the IDB persistence + reset tests to pin
/// expectations directly at the storage layer.
async function readDbCounts(page) {
  return await page.evaluate(async () => {
    return await new Promise((resolve, reject) => {
      const req = indexedDB.open("sunset-store");
      req.onsuccess = () => {
        const db = req.result;
        // The schema upgrade in `sunset-store-indexeddb` always creates
        // these stores; if a store is missing we treat the database as
        // effectively empty (count = 0).
        const names = Array.from(db.objectStoreNames);
        if (!names.includes("entries") || !names.includes("blobs")) {
          db.close();
          resolve({ entries: 0, blobs: 0 });
          return;
        }
        const txn = db.transaction(["entries", "blobs"], "readonly");
        const entries = txn.objectStore("entries").count();
        const blobs = txn.objectStore("blobs").count();
        Promise.all([
          new Promise((r, e) => {
            entries.onsuccess = () => r(entries.result);
            entries.onerror = () => e(entries.error);
          }),
          new Promise((r, e) => {
            blobs.onsuccess = () => r(blobs.result);
            blobs.onerror = () => e(blobs.error);
          }),
        ])
          .then(([eCount, bCount]) => {
            db.close();
            resolve({ entries: eCount, blobs: bCount });
          })
          .catch(reject);
      };
      req.onerror = () => reject(req.error);
    });
  });
}
