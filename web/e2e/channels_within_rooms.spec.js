// Channels-within-rooms isolation test.
//
// Two browsers join the same room. Bob types "links" into the new-channel
// input + Enter to switch into #links locally; Alice stays on #general.
// Bob posts "via links". The test asserts:
//
//   1. Alice's messages region (still on #general) does NOT show "via links".
//   2. #links appears in Alice's channels rail dynamically (because Bob's
//      post arrived under channel="links" and on_channels_changed fires).
//   3. Alice clicks the #links row — her messages region now shows "via links".
//   4. Alice posts "ack" in #links; Bob (on #links) sees it.
//   5. Bob clicks back to #general — his messages region does NOT show "ack".
//
// This is the load-bearing regression gate for the channels-within-rooms PR:
// it proves channel labels actually segregate messages between peers in the
// same room.

import { test, expect } from "@playwright/test";
import { spawn } from "child_process";
import { mkdtempSync, rmSync } from "fs";
import { tmpdir } from "os";
import { join } from "path";

let relayProcess = null;
let relayAddress = null;
let relayDataDir = null;

test.beforeAll(async () => {
  relayDataDir = mkdtempSync(join(tmpdir(), "sunset-relay-channels-test-"));
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

// On phone the channels rail lives inside a drawer (data-testid=
// "channels-drawer") with aria-hidden="true" until the user taps the
// hamburger toggle. The drawer is also rendered with translateX(-100%)
// when closed, which keeps it in the DOM but off-screen and non-tappable.
// On desktop the rail is part of the layout grid and the phone toggle
// isn't rendered — this helper becomes a no-op.
async function openChannelsRailIfPhone(page) {
  const drawer = page.locator('[data-testid="channels-drawer"]');
  if ((await drawer.count()) === 0) return; // desktop layout
  const open = (await drawer.getAttribute("aria-hidden")) === "false";
  if (open) return;
  await page.locator('[data-testid="phone-rooms-toggle"]').click();
  await expect(drawer).toHaveAttribute("aria-hidden", "false", {
    timeout: 2_000,
  });
}

test("channels segregate messages between #general and #links", async ({
  browser,
}) => {
  // Per-run room name so reusing the relay's data dir between test runs
  // doesn't replay stale messages into this test.
  const room = `channels-${Date.now()}`;
  const url = `/?relay=${encodeURIComponent(relayAddress)}#${room}`;

  const ctxAlice = await browser.newContext();
  const ctxBob = await browser.newContext();
  const alice = await ctxAlice.newPage();
  const bob = await ctxBob.newPage();

  // Surface browser console errors to the test output for easier debug.
  for (const [name, page] of [
    ["alice", alice],
    ["bob", bob],
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

  await alice.goto(url);
  await bob.goto(url);

  // Wait for the chat shell to mount in both browsers (brand text in
  // the rooms rail), and the composer to be ready.
  for (const page of [alice, bob]) {
    await expect(page.getByText("sunset", { exact: true })).toBeVisible({
      timeout: 15_000,
    });
    await expect(page.locator("#composer-textarea")).toBeVisible({
      timeout: 15_000,
    });
  }

  // ── Bob switches to #links via the new-channel input ──────────────────
  // On phone the channels rail lives inside a drawer that's closed by
  // default — open it so a real user could interact with the rail. On
  // desktop the toggle isn't rendered, so this is a no-op.
  await openChannelsRailIfPhone(bob);
  const bobNewChannelInput = bob.locator('[data-testid="new-channel-input"]');
  await expect(bobNewChannelInput).toBeVisible({ timeout: 5_000 });
  await bobNewChannelInput.fill("links");
  await bobNewChannelInput.press("Enter");

  // The composer placeholder reflects the active channel — wait for it
  // to update so we know the SwitchChannel reducer has run before the
  // post goes out.
  await expect(bob.locator("#composer-textarea")).toHaveAttribute(
    "placeholder",
    "Message #links",
    { timeout: 5_000 },
  );

  // ── Bob posts "via links" in #links ───────────────────────────────────
  const bobComposer = bob.locator("#composer-textarea");
  await bobComposer.fill("via links");
  await bobComposer.press("Enter");

  // Bob's own #links view shows the message (round-tripped via on_message).
  await expect(
    bob.locator('[data-testid="messages-list"]').getByText("via links"),
  ).toBeVisible({ timeout: 5_000 });

  // ── Negative: Alice (still on #general) must NOT see "via links" ──────
  // We give the assertion a 3s window; the message has already round-
  // tripped through Bob (≤5s above), so anything that hasn't arrived at
  // Alice's filtered view in another 3s is the right outcome.
  const aliceMessages = alice.locator('[data-testid="messages-list"]');
  await expect(aliceMessages).not.toContainText("via links", {
    timeout: 3_000,
  });

  // ── #links appears in Alice's channels rail dynamically ───────────────
  // on_channels_changed fires when the first message under channel
  // "links" reaches Alice's local store. The rail re-renders to add the
  // row.
  //
  // We select by text content rather than role because on phone the
  // channels rail sits inside an aria-hidden="true" drawer (the channels
  // drawer is closed by default). aria-hidden flips off the accessible
  // tree, which makes `getByRole` skip the button — but the row still
  // exists in the DOM, and a real user (or a peer) can still observe
  // that the engine has registered the channel.
  const aliceRail = alice.locator('[data-testid="channels-rail"]');
  const aliceLinksRow = aliceRail.locator("button", { hasText: "links" });
  await aliceLinksRow.waitFor({ state: "attached", timeout: 5_000 });

  // ── Open the channels drawer on phone so the rail rows are tappable ──
  // On desktop the rail is already in the layout grid; on phone it lives
  // inside a translateX(-100%) drawer until the user taps the open
  // button.
  await openChannelsRailIfPhone(alice);

  // ── Alice clicks #links — her messages region now shows "via links" ──
  await aliceLinksRow.click();
  await expect(alice.locator("#composer-textarea")).toHaveAttribute(
    "placeholder",
    "Message #links",
    { timeout: 5_000 },
  );
  await expect(aliceMessages.getByText("via links")).toBeVisible({
    timeout: 5_000,
  });

  // ── Alice posts "ack" in #links; Bob sees it ──────────────────────────
  const aliceComposer = alice.locator("#composer-textarea");
  await aliceComposer.fill("ack");
  await aliceComposer.press("Enter");

  await expect(
    bob.locator('[data-testid="messages-list"]').getByText("ack"),
  ).toBeVisible({ timeout: 5_000 });

  // ── Bob switches back to #general; "ack" must NOT be in his view ──────
  const bobRail = bob.locator('[data-testid="channels-rail"]');
  await openChannelsRailIfPhone(bob);
  await bobRail.locator("button", { hasText: "general" }).click();
  await expect(bob.locator("#composer-textarea")).toHaveAttribute(
    "placeholder",
    "Message #general",
    { timeout: 5_000 },
  );

  const bobMessages = bob.locator('[data-testid="messages-list"]');
  await expect(bobMessages).not.toContainText("ack", { timeout: 3_000 });

  // Close contexts explicitly. Playwright doesn't auto-close contexts
  // created via `browser.newContext()` between tests in the same worker,
  // and a leaked context keeps its wasm + supervisor retry loops running.
  await ctxAlice.close();
  await ctxBob.close();
});
