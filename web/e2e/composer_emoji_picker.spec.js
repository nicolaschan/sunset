// Composer emoji picker — desktop-only affordance that lives next to
// the composer's attach (upload-image) button. Clicking the button
// opens the same full emoji picker that the reactions UI uses (the
// `emoji-picker-element` web component); clicking an emoji in the
// picker inserts it into the composer's textarea at the current
// cursor position. The picker stays open across picks so the user
// can compose a multi-emoji message in one go; clicking outside the
// picker dismisses it.

import { expect, test } from "@playwright/test";

import { spawnRelay, teardownRelay } from "./helpers/voice.js";

let relayState = null;

test.beforeAll(async () => {
  relayState = await spawnRelay();
});

test.afterAll(() => {
  if (relayState) teardownRelay(relayState);
});

async function openChat(browser, { hash = "composer-emoji" } = {}) {
  const url = `/?relay=${encodeURIComponent(relayState.addr)}#${hash}`;
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

// Dispatch the picker's `emoji-click` CustomEvent on its host element.
// The picker's public contract is "emit `emoji-click` with
// detail.unicode when the user clicks an emoji"; the composer's
// handler listens for that event. Driving the picker through its real
// internal buttons would mean reaching into the web component's shadow
// DOM (an unrelated library's internals); dispatching the same event
// the user's click would dispatch exercises the same path with no
// dependency on those internals.
async function pickEmoji(page, emoji) {
  const picker = page.locator('[data-testid="full-emoji-picker"]');
  await expect(picker).toBeVisible({ timeout: 10_000 });
  await picker.evaluate((el, unicode) => {
    el.dispatchEvent(
      new CustomEvent("emoji-click", {
        detail: { unicode },
        bubbles: true,
        composed: true,
      }),
    );
  }, emoji);
}

test("desktop: emoji button is visible next to the attach button", async ({
  browser,
}, testInfo) => {
  test.skip(
    testInfo.project.name === "mobile-chrome",
    "the composer emoji button is desktop-only; mobile has native emoji input",
  );
  const { ctx, page } = await openChat(browser);

  const attach = page.locator('[data-testid="composer-attach"]');
  const emojiBtn = page.locator(
    '[data-testid="composer-emoji-picker-trigger"]',
  );
  await expect(attach).toBeVisible();
  await expect(emojiBtn).toBeVisible();

  // Both buttons should sit on roughly the same horizontal baseline
  // (same composer row). The emoji button must be adjacent to attach —
  // assert there's no large vertical separation.
  const attachBox = await attach.boundingBox();
  const emojiBox = await emojiBtn.boundingBox();
  expect(attachBox).not.toBeNull();
  expect(emojiBox).not.toBeNull();
  expect(Math.abs(attachBox.y - emojiBox.y)).toBeLessThan(8);

  await ctx.close();
});

test("mobile: emoji button is hidden (native OS emoji keyboard covers this)", async ({
  browser,
}, testInfo) => {
  test.skip(
    testInfo.project.name !== "mobile-chrome",
    "this test only matters on the mobile viewport",
  );
  const { ctx, page } = await openChat(browser);

  const emojiBtn = page.locator(
    '[data-testid="composer-emoji-picker-trigger"]',
  );
  await expect(emojiBtn).toHaveCount(0);

  await ctx.close();
});

test("desktop: clicking the emoji button opens the full picker", async ({
  browser,
}, testInfo) => {
  test.skip(
    testInfo.project.name === "mobile-chrome",
    "desktop-only feature",
  );
  const { ctx, page } = await openChat(browser);

  const overlay = page.locator('[data-testid="full-emoji-picker-overlay"]');
  await expect(overlay).toHaveCount(0);

  await page.locator('[data-testid="composer-emoji-picker-trigger"]').click();
  await expect(overlay).toBeVisible({ timeout: 10_000 });

  await ctx.close();
});

test("desktop: picking an emoji inserts it into the composer draft", async ({
  browser,
}, testInfo) => {
  test.skip(
    testInfo.project.name === "mobile-chrome",
    "desktop-only feature",
  );
  const { ctx, page, composer } = await openChat(browser);

  await composer.fill("hi ");
  await page.locator('[data-testid="composer-emoji-picker-trigger"]').click();
  await pickEmoji(page, "😀");
  await expect(composer).toHaveValue("hi 😀");

  // Submit; the message should be rendered in the stream.
  await composer.press("Enter");
  await expect(
    page.locator(".msg-row").filter({ hasText: "hi 😀" }).last(),
  ).toBeVisible({ timeout: 15_000 });

  await ctx.close();
});

test("desktop: picker stays open after a pick so multiple emojis can be inserted", async ({
  browser,
}, testInfo) => {
  test.skip(
    testInfo.project.name === "mobile-chrome",
    "desktop-only feature",
  );
  const { ctx, page, composer } = await openChat(browser);

  await page.locator('[data-testid="composer-emoji-picker-trigger"]').click();
  const overlay = page.locator('[data-testid="full-emoji-picker-overlay"]');
  await expect(overlay).toBeVisible({ timeout: 10_000 });

  await pickEmoji(page, "🌅");
  await expect(overlay).toBeVisible();
  await pickEmoji(page, "🦊");
  await expect(overlay).toBeVisible();

  await expect(composer).toHaveValue("🌅🦊");

  await ctx.close();
});

test("desktop: clicking outside the picker (backdrop) dismisses it", async ({
  browser,
}, testInfo) => {
  test.skip(
    testInfo.project.name === "mobile-chrome",
    "desktop-only feature",
  );
  const { ctx, page } = await openChat(browser);

  await page.locator('[data-testid="composer-emoji-picker-trigger"]').click();
  const overlay = page.locator('[data-testid="full-emoji-picker-overlay"]');
  await expect(overlay).toBeVisible({ timeout: 10_000 });

  await page.locator('[data-testid="full-emoji-picker-backdrop"]').click();
  await expect(overlay).toHaveCount(0);

  await ctx.close();
});

test("desktop: emoji is inserted at the cursor, not appended", async ({
  browser,
}, testInfo) => {
  test.skip(
    testInfo.project.name === "mobile-chrome",
    "desktop-only feature",
  );
  const { ctx, page, composer } = await openChat(browser);

  await composer.fill("abc");
  // Move cursor between 'a' and 'b'. Press Home → ArrowRight keeps the
  // textarea in single-caret mode and is what a user would actually do.
  await composer.press("Home");
  await composer.press("ArrowRight");

  await page.locator('[data-testid="composer-emoji-picker-trigger"]').click();
  await pickEmoji(page, "⭐");

  await expect(composer).toHaveValue("a⭐bc");

  await ctx.close();
});
