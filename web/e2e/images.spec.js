// Image attachment e2e — verifies the composer's image picker flow:
//   * attach button opens the OS file picker (mocked via setInputFiles)
//   * picked images render as thumbnails above the textarea
//   * remove (×) button drops a staged image before send
//   * sending forwards the images alongside the text body
//   * the receiving browser renders each image inline below the body
//   * an image-only send (empty draft + at least one image) works
//
// Uses the same relay-spawn pattern as composer.spec.js / two_browser_chat.spec.js.

import { spawn } from "child_process";
import { mkdtempSync, rmSync } from "fs";
import { tmpdir } from "os";
import { join } from "path";

import { expect, test } from "@playwright/test";

let relayProcess = null;
let relayAddress = null;
let relayDataDir = null;

// 1×1 raster fixtures, kept as base64 so the spec doesn't depend on any
// on-disk binary file. Each one is a real, well-formed file the browser
// will actually decode + render — that's the contract: if `<img src>`
// fails to decode, the test fails like a real user would notice.
const PNG_1X1_RED_BASE64 =
  "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABAQMAAAAl21bKAAAAA1BMVEX/AAAZ4gk3AAAAAXRSTlPM" +
  "0jRW/QAAAApJREFUeJxjYAAAAAIAAUivpHEAAAAASUVORK5CYII=";
const GIF_1X1_BASE64 =
  "R0lGODlhAQABAIAAAP///wAAACH5BAEAAAAALAAAAAABAAEAAAICRAEAOw==";

test.beforeAll(async () => {
  relayDataDir = mkdtempSync(join(tmpdir(), "sunset-relay-images-"));
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
  const url = `/?relay=${encodeURIComponent(relayAddress)}#sunset-images-${hashSuffix}`;
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

// Stage `files` (array of `{ name, mimeType, buffer }`) on the
// composer's hidden file input. The Gleam side lazily creates the
// input on first attach-button click and immediately calls
// `input.click()` to open the OS picker — Playwright intercepts that
// via the `filechooser` event, where `setFiles` injects the test
// payload as if the user had selected those files.
async function stageImages(page, files) {
  const chooserPromise = page.waitForEvent("filechooser");
  await page.getByTestId("composer-attach").click();
  const chooser = await chooserPromise;
  await chooser.setFiles(files);
}

function fileFrom(name, mimeType, base64) {
  return {
    name,
    mimeType,
    buffer: Buffer.from(base64, "base64"),
  };
}

test("two browsers exchange a text+image message via relay", async ({
  browser,
}) => {
  const { ctx: ctxA, page: pageA, composer: inputA } = await openPage(
    browser,
    "textimg",
  );
  const { ctx: ctxB, page: pageB } = await openPage(browser, "textimg");

  // Stage one PNG on A.
  await stageImages(pageA, [
    fileFrom("red.png", "image/png", PNG_1X1_RED_BASE64),
  ]);

  // Thumbnail strip should now show exactly one attachment.
  const previews = pageA.getByTestId("composer-attachment");
  await expect(previews).toHaveCount(1, { timeout: 15_000 });

  // Send text + image.
  const text = `from-A with image — ${Date.now()}`;
  await inputA.fill(text);
  await inputA.press("Enter");

  // Composer's attachment strip clears on send.
  await expect(previews).toHaveCount(0, { timeout: 15_000 });

  // B sees both the text body and the image inline.
  await expect(pageB.getByText(text)).toBeVisible({ timeout: 15_000 });
  const imagesOnB = pageB.getByTestId("message-image");
  await expect(imagesOnB).toHaveCount(1, { timeout: 15_000 });
  const src = await imagesOnB.first().getAttribute("src");
  expect(src).toBeTruthy();
  expect(src.startsWith("data:image/png;base64,")).toBeTruthy();
  // The image is what A sent, byte-for-byte (base64 transports unchanged).
  expect(src.endsWith(PNG_1X1_RED_BASE64)).toBeTruthy();

  await ctxA.close();
  await ctxB.close();
});

test("image-only send works when the draft text is empty", async ({
  browser,
}) => {
  const { ctx: ctxA, page: pageA, composer: inputA } = await openPage(
    browser,
    "imgonly",
  );
  const { ctx: ctxB, page: pageB } = await openPage(browser, "imgonly");

  await stageImages(pageA, [
    fileFrom("dot.gif", "image/gif", GIF_1X1_BASE64),
  ]);

  // Without touching the textarea (draft remains empty) press Enter
  // from the focused input. Sunset.chat treats this as image-only send.
  await inputA.click();
  await inputA.press("Enter");

  // Staging clears after send.
  await expect(pageA.getByTestId("composer-attachment")).toHaveCount(0, {
    timeout: 15_000,
  });

  // B receives the image even though the body is empty.
  const imagesOnB = pageB.getByTestId("message-image");
  await expect(imagesOnB).toHaveCount(1, { timeout: 15_000 });
  const src = await imagesOnB.first().getAttribute("src");
  expect(src.startsWith("data:image/gif;base64,")).toBeTruthy();

  await ctxA.close();
  await ctxB.close();
});

test("removing a staged image before send drops it from the post", async ({
  browser,
}) => {
  const { ctx: ctxA, page: pageA, composer: inputA } = await openPage(
    browser,
    "remove",
  );
  const { ctx: ctxB, page: pageB } = await openPage(browser, "remove");

  await stageImages(pageA, [
    fileFrom("first.png", "image/png", PNG_1X1_RED_BASE64),
    fileFrom("second.gif", "image/gif", GIF_1X1_BASE64),
  ]);
  await expect(pageA.getByTestId("composer-attachment")).toHaveCount(2, {
    timeout: 15_000,
  });

  // Remove the first staged image. The remaining one is the GIF.
  await pageA.getByTestId("composer-attachment-remove").first().click();
  await expect(pageA.getByTestId("composer-attachment")).toHaveCount(1);

  const text = `kept-one — ${Date.now()}`;
  await inputA.fill(text);
  await inputA.press("Enter");

  await expect(pageB.getByText(text)).toBeVisible({ timeout: 15_000 });
  // Exactly one image — the GIF, not the PNG that was removed.
  const imagesOnB = pageB.getByTestId("message-image");
  await expect(imagesOnB).toHaveCount(1, { timeout: 15_000 });
  const src = await imagesOnB.first().getAttribute("src");
  expect(src.startsWith("data:image/gif;base64,")).toBeTruthy();

  await ctxA.close();
  await ctxB.close();
});

test("multiple images on a single message all render", async ({ browser }) => {
  const { ctx: ctxA, page: pageA, composer: inputA } = await openPage(
    browser,
    "multi",
  );
  const { ctx: ctxB, page: pageB } = await openPage(browser, "multi");

  await stageImages(pageA, [
    fileFrom("a.png", "image/png", PNG_1X1_RED_BASE64),
    fileFrom("b.gif", "image/gif", GIF_1X1_BASE64),
  ]);
  await expect(pageA.getByTestId("composer-attachment")).toHaveCount(2);

  const text = `two-images — ${Date.now()}`;
  await inputA.fill(text);
  await inputA.press("Enter");

  await expect(pageB.getByText(text)).toBeVisible({ timeout: 15_000 });
  await expect(pageB.getByTestId("message-image")).toHaveCount(2, {
    timeout: 15_000,
  });

  await ctxA.close();
  await ctxB.close();
});
