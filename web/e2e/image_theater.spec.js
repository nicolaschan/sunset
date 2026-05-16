// Image theater (lightbox) e2e — verifies:
//   * clicking an inline message image opens a full-screen overlay
//     showing the same image enlarged
//   * clicking the backdrop (anywhere outside the image) closes it
//   * pressing Escape closes it
//   * clicking the enlarged image itself does NOT close it
//
// Mirrors the relay-spawn pattern in images.spec.js: a real sunset-relay,
// two browser contexts so one peer sends and the other receives, then
// the theater interactions are driven on the receiving peer.

import { spawn } from "child_process";
import { mkdtempSync, rmSync } from "fs";
import { tmpdir } from "os";
import { join } from "path";

import { expect, test } from "@playwright/test";

let relayProcess = null;
let relayAddress = null;
let relayDataDir = null;

// 1×1 red PNG, kept as base64 so the spec has no on-disk binary asset.
const PNG_1X1_RED_BASE64 =
  "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABAQMAAAAl21bKAAAAA1BMVEX/AAAZ4gk3AAAAAXRSTlPM" +
  "0jRW/QAAAApJREFUeJxjYAAAAAIAAUivpHEAAAAASUVORK5CYII=";

test.beforeAll(async () => {
  relayDataDir = mkdtempSync(join(tmpdir(), "sunset-relay-theater-"));
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

async function openPage(browser, hashSuffix) {
  const url = `/?relay=${encodeURIComponent(relayAddress)}#sunset-theater-${hashSuffix}`;
  const ctx = await browser.newContext();
  const page = await ctx.newPage();
  page.on("pageerror", (err) =>
    process.stderr.write(`[pageerror] ${err.stack || err}\n`),
  );
  page.on("console", (msg) => {
    if (msg.type() === "error") {
      process.stderr.write(`[console] ${msg.text()}\n`);
    }
  });
  await page.goto(url);
  await expect(page.getByText("sunset", { exact: true })).toBeVisible({
    timeout: 15_000,
  });
  const composer = page.getByPlaceholder(/^Message #/);
  await expect(composer).toBeVisible({ timeout: 15_000 });
  return { ctx, page, composer };
}

async function sendImageFromAToB(browser, suffix) {
  const { ctx: ctxA, page: pageA, composer: inputA } = await openPage(
    browser,
    suffix,
  );
  const { ctx: ctxB, page: pageB } = await openPage(browser, suffix);

  const chooserPromise = pageA.waitForEvent("filechooser");
  await pageA.getByTestId("composer-attach").click();
  const chooser = await chooserPromise;
  await chooser.setFiles([
    {
      name: "red.png",
      mimeType: "image/png",
      buffer: Buffer.from(PNG_1X1_RED_BASE64, "base64"),
    },
  ]);

  const text = `theater-fixture — ${Date.now()}`;
  await inputA.fill(text);
  await inputA.press("Enter");

  // Wait until B has rendered the inline image.
  await expect(pageB.getByText(text)).toBeVisible({ timeout: 15_000 });
  await expect(pageB.getByTestId("message-image")).toHaveCount(1, {
    timeout: 15_000,
  });

  return { ctxA, ctxB, pageA, pageB };
}

test("clicking an inline image opens the theater overlay", async ({
  browser,
}) => {
  const { ctxA, ctxB, pageB } = await sendImageFromAToB(browser, "open");

  // Theater is hidden before the click.
  await expect(pageB.getByTestId("image-theater")).toHaveCount(0);

  await pageB.getByTestId("message-image").first().click();

  // Theater appears with the enlarged image carrying the same src as
  // the inline image (data-URL is identity-stable in this app).
  const theater = pageB.getByTestId("image-theater");
  await expect(theater).toBeVisible({ timeout: 5_000 });
  const enlarged = pageB.getByTestId("image-theater-image");
  await expect(enlarged).toBeVisible();
  const srcEnlarged = await enlarged.getAttribute("src");
  const srcInline = await pageB
    .getByTestId("message-image")
    .first()
    .getAttribute("src");
  expect(srcEnlarged).toBe(srcInline);

  await ctxA.close();
  await ctxB.close();
});

test("clicking the backdrop (outside the image) closes the theater", async ({
  browser,
}) => {
  const { ctxA, ctxB, pageB } = await sendImageFromAToB(browser, "backdrop");

  await pageB.getByTestId("message-image").first().click();
  const theater = pageB.getByTestId("image-theater");
  await expect(theater).toBeVisible();

  // The enlarged image is centered; click near the top-left corner of
  // the overlay so the click lands on the backdrop and not the image.
  await theater.click({ position: { x: 5, y: 5 } });
  await expect(theater).toHaveCount(0);
});

test("pressing Escape closes the theater", async ({ browser }) => {
  const { ctxA, ctxB, pageB } = await sendImageFromAToB(browser, "escape");

  await pageB.getByTestId("message-image").first().click();
  const theater = pageB.getByTestId("image-theater");
  await expect(theater).toBeVisible();

  await pageB.keyboard.press("Escape");
  await expect(theater).toHaveCount(0);
});

test("clicking the enlarged image itself keeps the theater open", async ({
  browser,
}) => {
  const { ctxA, ctxB, pageB } = await sendImageFromAToB(browser, "inside");

  await pageB.getByTestId("message-image").first().click();
  const theater = pageB.getByTestId("image-theater");
  await expect(theater).toBeVisible();

  // Click on the enlarged image — the theater must stay up.
  await pageB.getByTestId("image-theater-image").click();
  await expect(theater).toBeVisible();
  // And it should still be a real opaque overlay, not just rendered
  // but dismissed mid-frame.
  await expect(pageB.getByTestId("image-theater-image")).toBeVisible();
});
