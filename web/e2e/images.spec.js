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
import { deflateSync } from "node:zlib";

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

// Build a synthetic ~300 KB PNG by filling a 320×320 RGB image with
// deterministic pseudo-random per-pixel colour. High entropy defeats
// deflate so the encoded PNG is close to the raw RGB size (~300 KB),
// which is well past the 65 KB Noise per-message ceiling — exactly
// the case the in-chain `ChunkedConnection` inside `NoiseConnection`
// has to fire across multiple times each direction.
//
// Kept programmatic so the spec has zero on-disk binary assets; the
// size knob is `widthHeight` here, not a file to regenerate.
function makeLargePng(widthHeight = 320) {
  const w = widthHeight;
  const h = widthHeight;
  // PNG signature.
  const sig = Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]);

  // IHDR data: width, height, bit depth 8, color type 2 (RGB),
  // compression 0, filter 0, interlace 0.
  const ihdrData = Buffer.alloc(13);
  ihdrData.writeUInt32BE(w, 0);
  ihdrData.writeUInt32BE(h, 4);
  ihdrData[8] = 8;
  ihdrData[9] = 2;

  // Raw scanlines: filter byte + RGB triples per pixel. Pixel
  // colour comes from a tiny LCG seeded with the pixel index so the
  // resulting bytes look uncorrelated to deflate. Reproducible
  // across runs and across browsers (no Math.random).
  const raw = Buffer.alloc((1 + w * 3) * h);
  let seed = 0x9e3779b9 >>> 0; // arbitrary non-zero starting state
  for (let y = 0; y < h; y++) {
    const rowStart = y * (1 + w * 3);
    raw[rowStart] = 0; // filter byte
    for (let x = 0; x < w; x++) {
      // LCG (Numerical Recipes constants); take 3 bytes per pixel.
      seed = (Math.imul(seed, 1664525) + 1013904223) >>> 0;
      const r = (seed >>> 16) & 0xff;
      seed = (Math.imul(seed, 1664525) + 1013904223) >>> 0;
      const g = (seed >>> 16) & 0xff;
      seed = (Math.imul(seed, 1664525) + 1013904223) >>> 0;
      const b = (seed >>> 16) & 0xff;
      const off = rowStart + 1 + x * 3;
      raw[off] = r;
      raw[off + 1] = g;
      raw[off + 2] = b;
    }
  }
  // level 0 = no compression — for high-entropy data this is faster
  // and yields a PNG very close to the raw RGB size.
  const idatPayload = deflateSync(raw, { level: 0 });

  return Buffer.concat([
    sig,
    pngChunk("IHDR", ihdrData),
    pngChunk("IDAT", idatPayload),
    pngChunk("IEND", Buffer.alloc(0)),
  ]);
}

function pngChunk(type, data) {
  const len = Buffer.alloc(4);
  len.writeUInt32BE(data.length, 0);
  const typeBuf = Buffer.from(type, "ascii");
  const crc = pngCrc(Buffer.concat([typeBuf, data]));
  return Buffer.concat([len, typeBuf, data, crc]);
}

// Streaming CRC32 over `buf` with the PNG-standard polynomial
// (0xedb88320). Inlined so the spec doesn't pull in a new dev-dep
// just for one helper.
function pngCrc(buf) {
  let crc = 0xffffffff;
  for (let i = 0; i < buf.length; i++) {
    crc = (crc ^ buf[i]) >>> 0;
    for (let k = 0; k < 8; k++) {
      crc = (crc >>> 1) ^ ((crc & 1) ? 0xedb88320 : 0);
    }
  }
  crc = (crc ^ 0xffffffff) >>> 0;
  const out = Buffer.alloc(4);
  out.writeUInt32BE(crc, 0);
  return out;
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

test("large image (~300 KB) survives noise chunking end-to-end", async ({
  browser,
}) => {
  const { ctx: ctxA, page: pageA, composer: inputA } = await openPage(
    browser,
    "large",
  );
  const { ctx: ctxB, page: pageB } = await openPage(browser, "large");

  // 600×600 pure-red PNG. The deflate of 600 identical scanlines
  // compresses well, but base64 inflation + envelope overhead
  // pushes the on-wire payload past the 65 KB Noise ceiling
  // multiple times — exactly the case the chunker has to handle.
  const bigPng = makeLargePng(600);
  expect(bigPng.length).toBeGreaterThan(150_000);

  await stageImages(pageA, [
    { name: "big-red.png", mimeType: "image/png", buffer: bigPng },
  ]);
  await expect(pageA.getByTestId("composer-attachment")).toHaveCount(1, {
    timeout: 15_000,
  });

  const text = `large-image — ${Date.now()}`;
  await inputA.fill(text);
  await inputA.press("Enter");

  // Sender's composer clears.
  await expect(pageA.getByTestId("composer-attachment")).toHaveCount(0, {
    timeout: 15_000,
  });

  // Both sides render the image. Use a longer timeout: the chunker
  // does ~5 noise round-trips for a ~300 KB payload and there's
  // also store-insert + relay-forward latency.
  await expect(pageB.getByText(text)).toBeVisible({ timeout: 30_000 });
  await expect(pageB.getByTestId("message-image")).toHaveCount(1, {
    timeout: 30_000,
  });
  await expect(pageA.getByTestId("message-image")).toHaveCount(1, {
    timeout: 30_000,
  });

  // Byte-for-byte check on the receiver: the base64 in the `<img src>`
  // must equal the base64 of what the sender staged.
  const expectedBase64 = bigPng.toString("base64");
  const src = await pageB
    .getByTestId("message-image")
    .first()
    .getAttribute("src");
  expect(src).toBe(`data:image/png;base64,${expectedBase64}`);

  await ctxA.close();
  await ctxB.close();
});
