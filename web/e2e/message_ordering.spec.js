// Sender-claimed-time ordering — the messages timeline must render
// messages in the order their authors claim they were sent, not the
// order they happened to arrive over the network.
//
// Set-up: two browser contexts share a relay. Context B's `Date.now()`
// is shifted 10s into the past via `addInitScript`, so a message B
// sends *after* A's send still carries an earlier `sent_at_ms`. Both
// browsers must render the messages with B's message above A's
// regardless of arrival order.
//
// Without the Rust-side sort, the UI would show messages in arrival
// order — A would see [from A, from B] because its local insert lands
// before the remote one. With the sort, both browsers converge on the
// same claimed-time order: [from B, from A].

import { test, expect } from "@playwright/test";
import { spawn } from "child_process";
import { mkdtempSync, rmSync } from "fs";
import { tmpdir } from "os";
import { join } from "path";

let relayProcess = null;
let relayAddress = null;
let relayDataDir = null;

test.beforeAll(async () => {
  relayDataDir = mkdtempSync(join(tmpdir(), "sunset-relay-order-test-"));

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

test("messages render in sender-claimed-time order on both peers", async ({
  browser,
}) => {
  const url = `/?relay=${encodeURIComponent(relayAddress)}#sunset-ordering-test`;

  const ctxA = await browser.newContext();
  const ctxB = await browser.newContext();

  // Shift B's wall clock 10 seconds into the past. The Gleam UI's
  // `currentTimeMs()` shim is a plain `Date.now()` call, so this
  // changes the `sent_at_ms` of every message B composes — without
  // touching A's clock and without any test-only hook in the Rust /
  // Gleam code.
  const SKEW_MS = 10_000;
  await ctxB.addInitScript((skew) => {
    const realNow = Date.now.bind(Date);
    Date.now = () => realNow() - skew;
  }, SKEW_MS);

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

  const inputA = pageA.getByPlaceholder(/^Message #/);
  const inputB = pageB.getByPlaceholder(/^Message #/);
  await expect(inputA).toBeVisible({ timeout: 15_000 });
  await expect(inputB).toBeVisible({ timeout: 15_000 });

  // A sends first (clock = real wall-clock now).
  const fromA = `A-after-${Date.now()}`;
  await inputA.fill(fromA);
  await inputA.press("Enter");

  // Wait for A's text to arrive on B before B sends, so the *arrival*
  // order on both sides is well-defined: A's lands first on each peer.
  // The fix has to put B's claimed-earlier message above A's despite
  // A arriving first.
  await expect(pageB.getByText(fromA)).toBeVisible({ timeout: 15_000 });

  // B sends second. Its `sent_at_ms` is `now - SKEW_MS`, so the
  // claimed-time order is (B, A) even though A inserted first.
  const fromB = `B-before-${Date.now()}`;
  await inputB.fill(fromB);
  await inputB.press("Enter");

  await expect(pageA.getByText(fromB)).toBeVisible({ timeout: 15_000 });

  // Read the rendered order on each peer. The timeline is a list of
  // article elements with role="article" (the message rows). Filter
  // to the two test messages and assert claimed-time order.
  const readOrder = async (page) =>
    page.evaluate(({ a, b }) => {
      // Walk the visible chat timeline in DOM order. We look for any
      // element whose textContent contains either marker — the
      // specific markup (`<article>` / `<li>` / etc.) is treated as
      // an implementation detail here so the test stays robust to UI
      // refactors. The earliest occurrence wins per marker.
      const candidates = Array.from(document.querySelectorAll("*"))
        .map((el) => el.textContent || "")
        .filter((t) => t.includes(a) || t.includes(b));
      // Find the *outermost* (smallest) wrapper that contains exactly
      // one marker per element — i.e. the per-message row. We
      // approximate that by picking the first element whose direct
      // text contains the marker (no marker also appearing in a child
      // alone).
      const findIndex = (marker) =>
        candidates.findIndex(
          (t) => t.includes(marker) && !t.includes(marker === a ? b : a),
        );
      return [findIndex(a), findIndex(b)];
    }, { a: fromA, b: fromB });

  const expectOrder = async (page, label) => {
    // Wait until both markers are visible in the timeline.
    await expect(page.getByText(fromA)).toBeVisible({ timeout: 15_000 });
    await expect(page.getByText(fromB)).toBeVisible({ timeout: 15_000 });
    const [idxA, idxB] = await readOrder(page);
    expect(idxA, `${label}: A marker not found`).toBeGreaterThanOrEqual(0);
    expect(idxB, `${label}: B marker not found`).toBeGreaterThanOrEqual(0);
    // B claims it was sent 10s before A → B must render above A.
    expect(
      idxB,
      `${label}: B's earlier-claimed message must render before A's later-claimed one (got B@${idxB}, A@${idxA})`,
    ).toBeLessThan(idxA);
  };

  await expectOrder(pageA, "peer A");
  await expectOrder(pageB, "peer B");

  await ctxA.close();
  await ctxB.close();
});
