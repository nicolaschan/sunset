// UX-tweak bundle e2e:
//   1. The composer's send affordance is a real, clickable button —
//      tapping/clicking it sends the same way Enter does. Disabled when
//      there is nothing to send (empty draft + no attachments).
//   2. Pasting an image (Cmd/Ctrl+V from clipboard) into the composer
//      textarea attaches the image, identical to the file-picker flow.
//   3. The active-channel highlight, and the press-tap feedback on
//      channel rows, look the same on desktop and mobile — specifically
//      the OS default tap-highlight (e.g. Android Chrome's translucent
//      cyan) is suppressed so a tap doesn't flash a non-brand colour
//      across the row before transitioning to the active state.
//
// Uses the same relay-spawn pattern as composer.spec.js / images.spec.js.

import { spawn } from "child_process";
import { mkdtempSync, rmSync } from "fs";
import { tmpdir } from "os";
import { join } from "path";

import { expect, test } from "@playwright/test";

const PNG_1X1_RED_BASE64 =
  "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABAQMAAAAl21bKAAAAA1BMVEX/AAAZ4gk3AAAAAXRSTlPM" +
  "0jRW/QAAAApJREFUeJxjYAAAAAIAAUivpHEAAAAASUVORK5CYII=";

let relayProcess = null;
let relayAddress = null;
let relayDataDir = null;

test.beforeAll(async () => {
  relayDataDir = mkdtempSync(join(tmpdir(), "sunset-relay-uxbundle-"));
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

async function openChat(browser, hash) {
  const url = `/?relay=${encodeURIComponent(relayAddress)}#${hash}`;
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

// --- Tweak 1: the send button is a real, clickable button -------------

test("send button click sends the draft, identical to pressing Enter", async ({
  browser,
}) => {
  const { ctx, page, composer } = await openChat(browser, "ux-send-click");

  const sendBtn = page.getByTestId("composer-send");
  // Disabled (or otherwise non-interactive) on an empty draft — sending
  // empty text would be a no-op message.
  await expect(sendBtn).toBeVisible();
  await expect(sendBtn).toBeDisabled();

  const body = `send-click-${Date.now()}`;
  await composer.fill(body);
  // Once there's draft text, the send button must become enabled.
  await expect(sendBtn).toBeEnabled();

  await sendBtn.click();

  // The message appears in the timeline and the composer clears, exactly
  // as if the user had pressed Enter.
  await expect(
    page.locator(".msg-row").filter({ hasText: body }).first(),
  ).toBeVisible({ timeout: 15_000 });
  await expect(composer).toHaveValue("");
  await expect(sendBtn).toBeDisabled();

  await ctx.close();
});

// --- Tweak 2: pasting an image into the textarea attaches it ---------

test("pasting an image into the composer textarea stages it as an attachment", async ({
  browser,
}) => {
  const { ctx, page, composer } = await openChat(browser, "ux-paste-image");

  // Simulate a real clipboard paste: build a synthetic ClipboardEvent
  // whose dataTransfer carries a File of MIME image/png. This is the
  // exact shape `composer.ffi.mjs`'s paste handler must consume — the
  // same shape Chromium delivers when the user copies a PNG from
  // Preview / a screenshot tool and Cmd/Ctrl+V's into our textarea.
  await composer.focus();
  await page.evaluate(async (b64) => {
    const bin = atob(b64);
    const bytes = new Uint8Array(bin.length);
    for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
    const file = new File([bytes], "pasted.png", { type: "image/png" });
    const dt = new DataTransfer();
    dt.items.add(file);
    const evt = new ClipboardEvent("paste", {
      clipboardData: dt,
      bubbles: true,
      cancelable: true,
    });
    document
      .getElementById("composer-textarea")
      .dispatchEvent(evt);
  }, PNG_1X1_RED_BASE64);

  // The pasted image must appear in the composer's attachment strip,
  // exactly as if it had been picked through the file picker.
  await expect(page.getByTestId("composer-attachment")).toHaveCount(1, {
    timeout: 10_000,
  });

  // Pasting an image must NOT also dump base64 bytes into the textarea
  // (the handler should preventDefault when it consumes a clipboard
  // image). The draft should remain empty / clean.
  await expect(composer).toHaveValue("");

  await ctx.close();
});

// --- Tweak 3: channel highlight + tap feedback consistent ------------

const tapHighlightTest = (label, hash) =>
  test(`channel rows suppress the OS default tap-highlight (${label})`, async ({
    browser,
  }) => {
    const { ctx, page } = await openChat(browser, `ux-tap-${hash}`);

    // On phone, the channel rail lives behind the rooms drawer.
    const isMobile = page.viewportSize().width < 700;
    if (isMobile) {
      await page.locator('[data-testid="phone-rooms-toggle"]').click();
    }

    const row = page.locator('[data-testid="text-channel-row"]').first();
    await expect(row).toBeVisible({ timeout: 15_000 });

    // The OS-default tap-highlight (translucent gray on iOS, translucent
    // cyan on Android Chrome) is jarring against the brand accent — a
    // tap should produce no contrasting flash before the row settles
    // into its active-channel style. We assert transparency rather than
    // a specific colour: any non-zero alpha means a flash will leak.
    const tap = await row.evaluate(
      (el) => getComputedStyle(el).webkitTapHighlightColor,
    );
    // Acceptable forms (all alpha-zero):
    //   rgba(0, 0, 0, 0)
    //   transparent  → computed as rgba(0, 0, 0, 0)
    expect(tap.replace(/\s+/g, "")).toBe("rgba(0,0,0,0)");

    await ctx.close();
  });

tapHighlightTest("both viewports", "tap");
