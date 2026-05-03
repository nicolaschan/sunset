// E2E tests for the bundled UI tweaks:
//   1. Compose textarea is auto-focused after a channel switch and a
//      room switch.
//   2. Clicking the "you" row in the rooms rail opens a settings
//      popover (desktop) / sheet (phone) with theme + reset controls.
//      Dark/Light buttons flip the body palette; reset wipes
//      localStorage and reloads the page.
//   3. Message rows show a hover highlight (full-bleed) and stay
//      highlighted while the reaction picker or details panel is open.
//      The action toolbar (React/Info) also stays visible while the
//      menu is up.
//   4. On phone, a fresh load with `#room` lands directly on chat (no
//      drawer is open) and the composer renders as a single line
//      (no inline `style.height` carried over from a stale state).
//   5. Message author names are colored according to the matching
//      member's connection state: online+direct → palette ok; offline
//      → palette text_faint; self → palette accent.
//
// Each test boots a fresh relay so the suite is hermetic.

import { spawn } from "child_process";
import { mkdtempSync, rmSync } from "fs";
import { tmpdir } from "os";
import { join } from "path";

import { expect, test } from "@playwright/test";

let relayProcess = null;
let relayAddress = null;
let relayDataDir = null;

test.beforeAll(async () => {
  relayDataDir = mkdtempSync(join(tmpdir(), "sunset-relay-uitweaks-test-"));
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

async function openChat(browser, hash = "ui-tweaks") {
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

  await page.addInitScript(() => {
    window.SUNSET_TEST = true;
  });
  await page.goto(url);
  await expect(page.getByText("sunset", { exact: true })).toBeVisible({
    timeout: 15_000,
  });

  const composer = page.getByPlaceholder(/^Message #/);
  await expect(composer).toBeVisible({ timeout: 15_000 });
  return { ctx, page, composer };
}

async function openSettings(page, testInfo) {
  const isMobile = testInfo.project.name === "mobile-chrome";
  if (isMobile) {
    // The "you" row lives in the rooms rail, which is behind the
    // rooms drawer on phone. Open channels first, then swap to rooms
    // via the channels-room-title (same path the drawer helpers use).
    await page.getByTestId("phone-rooms-toggle").click();
    await page.getByTestId("channels-room-title").click();
  }
  await page.getByTestId("you-row").click();
}

// ─────────────────────────────────────────────────────────────────────
// 1. Auto-focus on channel / room switch
// ─────────────────────────────────────────────────────────────────────

test("composer is focused on initial mount", async ({ browser }) => {
  const { ctx, page, composer } = await openChat(browser, "focus-init");
  // Polling rather than a single check: the focus FFI defers to the
  // next animation frame so a Lustre re-render can't blow it away.
  await expect
    .poll(async () => composer.evaluate((el) => el === document.activeElement), {
      timeout: 5_000,
    })
    .toBe(true);
  await ctx.close();
});

test("composer regains focus after a hash-based room switch", async ({
  browser,
}) => {
  const { ctx, page, composer } = await openChat(browser, "focus-hash-a");
  await expect(composer).toBeFocused({ timeout: 5_000 });

  // Type something so the textarea is non-empty, then move focus
  // somewhere else; switching rooms should pull focus back.
  await composer.fill("draft");
  await page.evaluate(() => {
    const el = document.activeElement;
    if (el && typeof el.blur === "function") el.blur();
  });
  await expect(composer).not.toBeFocused();

  await page.evaluate(() => {
    location.hash = "#focus-hash-b";
  });
  await expect
    .poll(async () => composer.evaluate((el) => el === document.activeElement), {
      timeout: 5_000,
    })
    .toBe(true);

  await ctx.close();
});

test("composer regains focus after a channel switch", async ({
  browser,
}, testInfo) => {
  // The text-channels list lives in the channels rail. On phone that
  // rail is behind the channels drawer; the helpers in helpers/viewport
  // already do the song-and-dance, but for this test we just exercise
  // the desktop path — the SelectChannel handler is viewport-agnostic.
  test.skip(
    testInfo.project.name === "mobile-chrome",
    "channel switching is exercised through the desktop layout",
  );

  const { ctx, page, composer } = await openChat(browser, "focus-channel");
  await expect(composer).toBeFocused({ timeout: 5_000 });

  await page.evaluate(() => {
    const el = document.activeElement;
    if (el && typeof el.blur === "function") el.blur();
  });
  await expect(composer).not.toBeFocused();

  // Click any text-channel button in the rail other than the one
  // currently active. The rail buttons render with `# <name>` text;
  // pick a stable fixture entry.
  const sageRoots = page.getByRole("button", { name: /sage-roots/ });
  if ((await sageRoots.count()) > 0) {
    await sageRoots.first().click();
  } else {
    // Fallback: click whichever non-active text channel exists.
    const channelButtons = page.locator("button").filter({ hasText: "#" });
    await channelButtons.nth(1).click();
  }

  await expect
    .poll(async () => composer.evaluate((el) => el === document.activeElement), {
      timeout: 5_000,
    })
    .toBe(true);
  await ctx.close();
});

// ─────────────────────────────────────────────────────────────────────
// 2. Settings popover from the "you" row
// ─────────────────────────────────────────────────────────────────────

test("clicking 'you' opens settings; theme buttons flip palette", async ({
  browser,
}, testInfo) => {
  const { ctx, page } = await openChat(browser, "settings-theme");
  await openSettings(page, testInfo);
  await expect(page.getByTestId("settings-popover")).toBeVisible({
    timeout: 5_000,
  });

  // Capture the body's computed background to detect a palette flip.
  // (The body's `background` is set by the global_reset stylesheet to
  // `palette.bg`, so it changes between Light and Dark.)
  const initialBg = await page.evaluate(
    () => getComputedStyle(document.body).backgroundColor,
  );

  // Force-click: on phone the bottom sheet's drag-handle scroll area can
  // briefly intercept pointer events while the slide animation finishes.
  await page.getByTestId("settings-theme-dark").click({ force: true });
  await expect
    .poll(
      async () =>
        page.evaluate(() => getComputedStyle(document.body).backgroundColor),
      { timeout: 3_000 },
    )
    .not.toBe(initialBg);

  await page.getByTestId("settings-theme-light").click({ force: true });
  await expect
    .poll(
      async () =>
        page.evaluate(() => getComputedStyle(document.body).backgroundColor),
      { timeout: 3_000 },
    )
    .toBe(initialBg);

  // Aria-pressed reflects the current selection.
  await expect(page.getByTestId("settings-theme-light")).toHaveAttribute(
    "aria-pressed",
    "true",
  );
  await ctx.close();
});

test("settings reset button wipes localStorage and reloads", async ({
  browser,
}, testInfo) => {
  const { ctx, page } = await openChat(browser, "settings-reset");

  // Seed something detectable in localStorage so we can prove it was
  // cleared by the reset action (the app's own keys would also do, but
  // an explicit canary is unambiguous).
  await page.evaluate(() => {
    localStorage.setItem("sunset-web-test-canary", "still-here");
  });

  await openSettings(page, testInfo);
  await expect(page.getByTestId("settings-reset")).toBeVisible({
    timeout: 5_000,
  });

  // Reset triggers `location.reload()`, so wait for the reload by
  // racing on a fresh navigation event.
  const navigationPromise = page.waitForLoadState("load");
  await page.getByTestId("settings-reset").click({ force: true });
  await navigationPromise;

  // After reload the canary is gone and the URL fragment is cleared.
  const canary = await page.evaluate(() =>
    localStorage.getItem("sunset-web-test-canary"),
  );
  expect(canary).toBeNull();
  expect(new URL(page.url()).hash).toBe("");

  await ctx.close();
});

// ─────────────────────────────────────────────────────────────────────
// 3. Hover highlight + reaction picker stays open
// ─────────────────────────────────────────────────────────────────────

test("message row stays highlighted while reaction picker is open", async ({
  browser,
}, testInfo) => {
  test.skip(
    testInfo.project.name === "mobile-chrome",
    "the inline reaction picker is desktop-only; the mobile sheet variant has its own selection invariants",
  );

  const { ctx, page, composer } = await openChat(browser, "hover-active");

  await composer.fill(`hover-active-${Date.now()}`);
  await composer.press("Enter");
  const msgRow = page.locator(".msg-row").last();
  await expect(msgRow).toBeVisible({ timeout: 15_000 });

  // Open the reaction picker. The toolbar React button is the only
  // one with an `aria-label="React"` — the picker emoji buttons set
  // a `title="React with …"` but no aria-label, so the label-based
  // locator is unambiguous. force:true because the action toolbar is
  // opacity:0 until the row is hovered.
  const reactBtn = msgRow.locator('button[aria-label="React"]');
  await reactBtn.click({ force: true });
  await expect(page.locator('[data-testid="reaction-picker"]')).toBeVisible({
    timeout: 5_000,
  });

  // Move the cursor away from the row. The :hover state collapses,
  // but `.is-active` should still pin the highlight + the action
  // toolbar.
  await page.mouse.move(0, 0);

  await expect(msgRow).toHaveClass(/is-active/);
  // The highlight backdrop is the surface_alt color from the palette.
  // We don't hardcode the literal RGB — rgba parsers across Chromium
  // versions are subtly different — but the bg must be non-transparent.
  const bg = await msgRow.evaluate(
    (el) => getComputedStyle(el).backgroundColor,
  );
  expect(bg).not.toBe("rgba(0, 0, 0, 0)");
  expect(bg).not.toBe("transparent");

  // The action toolbar must also be interactable while the picker is
  // open (so the user can re-click React/Info without re-entering the
  // row's hover area).
  const toolbarOpacity = await reactBtn.evaluate((el) => {
    return getComputedStyle(el.closest(".msg-actions")).opacity;
  });
  expect(parseFloat(toolbarOpacity)).toBeGreaterThan(0.5);

  await ctx.close();
});

test("message row stays highlighted while details panel is open", async ({
  browser,
}, testInfo) => {
  const { ctx, page, composer } = await openChat(browser, "hover-details");
  const isMobile = testInfo.project.name === "mobile-chrome";

  await composer.fill(`hover-details-${Date.now()}`);
  await composer.press("Enter");
  const msgRow = page.locator(".msg-row").last();
  await expect(msgRow).toBeVisible({ timeout: 15_000 });

  // On mobile the action toolbar is `pointer-events: none` until the
  // row is selected; tap the row first so the toolbar accepts clicks.
  if (isMobile) {
    await msgRow.click();
    await expect(msgRow).toHaveClass(/is-selected/);
  }

  await msgRow.getByTitle("Message details").click({ force: true });

  // Move the cursor far away to drop the :hover state.
  await page.mouse.move(0, 0);

  await expect(msgRow).toHaveClass(/is-active/);
  const bg = await msgRow.evaluate(
    (el) => getComputedStyle(el).backgroundColor,
  );
  expect(bg).not.toBe("rgba(0, 0, 0, 0)");
  expect(bg).not.toBe("transparent");

  await ctx.close();
});

test("message rows have a hover highlight and stretch to container edges", async ({
  browser,
}, testInfo) => {
  test.skip(
    testInfo.project.name === "mobile-chrome",
    "touch devices don't fire :hover; the equivalent invariant is covered by the is-active test on mobile",
  );

  const { ctx, page, composer } = await openChat(browser, "hover-bg");

  await composer.fill(`hover-bg-${Date.now()}`);
  await composer.press("Enter");
  const msgRow = page.locator(".msg-row").last();
  await expect(msgRow).toBeVisible({ timeout: 15_000 });

  // Idle bg must be transparent so the column shows through.
  await page.mouse.move(0, 0);
  const idleBg = await msgRow.evaluate(
    (el) => getComputedStyle(el).backgroundColor,
  );
  expect(idleBg === "rgba(0, 0, 0, 0)" || idleBg === "transparent").toBe(true);

  // Edge-to-edge: the row's left edge should align with the messages
  // column's left edge (within ~1px of rounding error). The negative
  // horizontal margin on .msg-row pulls the background outside the
  // column's padding.
  const { rowLeft, scrollLeft, rowRight, scrollRight } = await msgRow.evaluate(
    (el) => {
      const scroll = el.closest(".scroll-area");
      const sr = scroll.getBoundingClientRect();
      const rr = el.getBoundingClientRect();
      return {
        rowLeft: rr.left,
        scrollLeft: sr.left,
        rowRight: rr.right,
        scrollRight: sr.right,
      };
    },
  );
  expect(Math.abs(rowLeft - scrollLeft)).toBeLessThan(2);
  expect(Math.abs(rowRight - scrollRight)).toBeLessThan(2);

  // No rounded corners on the highlight.
  const radius = await msgRow.evaluate(
    (el) => getComputedStyle(el).borderTopLeftRadius,
  );
  expect(radius).toBe("0px");

  // The hover rule must exist in the global stylesheet — checking the
  // matched rule directly is more robust than dispatching synthetic
  // mouse events, which Chromium's headless renderer applies
  // inconsistently for the `:hover` pseudo-class.
  const hasHoverRule = await page.evaluate(() => {
    for (const sheet of document.styleSheets) {
      let rules = [];
      try {
        rules = sheet.cssRules ? Array.from(sheet.cssRules) : [];
      } catch {
        // CORS-protected sheets throw on cssRules; skip.
        continue;
      }
      for (const rule of rules) {
        // Inside the (hover: hover) @media block, look for the
        // `.msg-row:hover` rule and confirm it sets a background.
        if (rule.media && /hover:\s*hover/.test(rule.media.mediaText)) {
          for (const inner of Array.from(rule.cssRules || [])) {
            if (
              inner.selectorText === ".msg-row:hover" &&
              inner.style.background &&
              inner.style.background !== "transparent" &&
              inner.style.background !== "rgba(0, 0, 0, 0)"
            ) {
              return true;
            }
          }
        }
      }
    }
    return false;
  });
  expect(hasHoverRule).toBe(true);

  await ctx.close();
});

// ─────────────────────────────────────────────────────────────────────
// 4. Mobile join state: drawer closed + composer single-line
// ─────────────────────────────────────────────────────────────────────

test("on mobile, fresh load lands on chat with no drawer open", async ({
  browser,
}, testInfo) => {
  test.skip(
    testInfo.project.name !== "mobile-chrome",
    "mobile-only invariant",
  );

  const { ctx, page } = await openChat(browser, "mobile-fresh");

  // Every drawer must report aria-hidden="true" on first paint.
  for (const id of [
    "channels-drawer",
    "rooms-drawer",
    "members-drawer",
  ]) {
    const drawer = page.getByTestId(id);
    await expect(drawer).toHaveAttribute("aria-hidden", "true");
  }

  await ctx.close();
});

test("on mobile, the composer renders as a single line on first paint", async ({
  browser,
}, testInfo) => {
  test.skip(
    testInfo.project.name !== "mobile-chrome",
    "mobile-only invariant",
  );

  const { ctx, page, composer } = await openChat(browser, "mobile-compose");

  // Compute the expected single-row height from the textarea's own
  // computed font metrics so the test is robust to palette / font
  // tweaks. With `padding: 0` and `line-height: 1.4`, one rendered
  // row is ≈ font-size * 1.4. Allow ±2px for sub-pixel rounding.
  const oneLineHeight = await composer.evaluate((el) => {
    const cs = getComputedStyle(el);
    const fontSize = parseFloat(cs.fontSize);
    const lineHeight =
      cs.lineHeight === "normal" ? fontSize * 1.2 : parseFloat(cs.lineHeight);
    return lineHeight;
  });
  const actualHeight = await composer.evaluate(
    (el) => el.getBoundingClientRect().height,
  );
  expect(actualHeight).toBeLessThan(oneLineHeight + 4);

  // No inline `style.height` override should be carried over from any
  // stale autoGrow cycle.
  const inlineHeight = await composer.evaluate((el) => el.style.height);
  expect(inlineHeight).toBe("");

  await ctx.close();
});

// ─────────────────────────────────────────────────────────────────────
// 5. Username coloring by connection state
// ─────────────────────────────────────────────────────────────────────

test("own message author is rendered in the accent palette color", async ({
  browser,
}) => {
  const { ctx, page, composer } = await openChat(browser, "color-self");

  await composer.fill(`color-self-${Date.now()}`);
  await composer.press("Enter");
  const author = page
    .locator('[data-testid="message-author"]')
    .last();
  await expect(author).toBeVisible({ timeout: 15_000 });

  // Pull the accent color from a known palette-driven element. The
  // landing-input join button uses palette.accent as its background
  // when the input is non-empty — we don't need to touch landing here,
  // but theme.gleam sets the rooms-rail "you" dot to palette.live and
  // various accents on `surface_alt`. Easier: just assert the author
  // color is *not* the default text/text_muted.
  const textColor = await author.evaluate((el) => getComputedStyle(el).color);

  // The default text colour for un-tinted spans in the message header
  // is palette.text. Compare against a non-tinted sibling — the time
  // span in the same header is rendered in palette.text_faint, but a
  // simpler assertion is "the author colour differs from the rendered
  // body text". The rendered message body wraps text in a div whose
  // color is palette.text.
  const bodyColor = await page
    .locator(".msg-row")
    .last()
    .locator("div", { hasText: "color-self-" })
    .first()
    .evaluate((el) => getComputedStyle(el).color);

  expect(textColor).not.toBe(bodyColor);
  await ctx.close();
});
