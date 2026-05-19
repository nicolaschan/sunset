// iMessage-style "jumbo" emoji rendering — a message body whose trimmed
// content is exactly 1-3 emoji grapheme clusters (with only whitespace
// between them) renders at a much larger font size than normal text.
// The classification lives in the Rust parser as a `Block::Jumbo`
// variant; this spec verifies the full WASM parse → Gleam decode →
// Lustre render → DOM pipeline.
//
// Pinning notes:
//   - Font sizes are asserted by exact value (54 / 44 / 36 px), not by
//     range. Loose bands would let an adjacent-count swap pass.
//   - `data-emoji-count` is asserted by exact value ("1" / "2" / "3").
//   - Negative cases (4+ emoji, mixed text + emoji, plain text, keycap-
//     base codepoints) assert the jumbo testid is *absent* AND that the
//     row still rendered something — `toHaveCount(0)` alone could pass
//     vacuously if the row never appeared.

import { expect, test } from "@playwright/test";

import { spawnRelay, teardownRelay } from "./helpers/voice.js";

let relay = null;

test.beforeAll(async () => {
  relay = await spawnRelay();
});

test.afterAll(() => {
  teardownRelay(relay);
});

test.setTimeout(60_000);

async function openChat(browser, hash) {
  const url = `/?relay=${encodeURIComponent(relay.addr)}#${hash}`;
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

async function send(composer, body) {
  await composer.fill(body);
  await composer.press("Enter");
}

// Returns the resolved font-size in px as a float — the value the
// renderer will actually use. Inspecting only the inline `style` attr
// would miss a CSS regression that overrode it.
async function fontSizePx(locator) {
  const raw = await locator.evaluate((el) => getComputedStyle(el).fontSize);
  return parseFloat(raw);
}

test("one emoji body renders jumbo at 54px with data-emoji-count=1", async ({
  browser,
}) => {
  const { ctx, page, composer } = await openChat(browser, "jumbo-one");
  await send(composer, "🌅");
  const jumbo = page.locator('[data-testid="emoji-jumbo"]').last();
  await expect(jumbo).toBeVisible({ timeout: 15_000 });
  await expect(jumbo).toHaveAttribute("data-emoji-count", "1");
  expect(await fontSizePx(jumbo)).toBe(54);
  await ctx.close();
});

test("two emoji body renders jumbo at 44px with data-emoji-count=2", async ({
  browser,
}) => {
  const { ctx, page, composer } = await openChat(browser, "jumbo-two");
  await send(composer, "🌅🌙");
  const jumbo = page.locator('[data-testid="emoji-jumbo"]').last();
  await expect(jumbo).toBeVisible({ timeout: 15_000 });
  await expect(jumbo).toHaveAttribute("data-emoji-count", "2");
  expect(await fontSizePx(jumbo)).toBe(44);
  await ctx.close();
});

test("three emoji body renders jumbo at 36px with data-emoji-count=3", async ({
  browser,
}) => {
  const { ctx, page, composer } = await openChat(browser, "jumbo-three");
  await send(composer, "🌅🌙🔥");
  const jumbo = page.locator('[data-testid="emoji-jumbo"]').last();
  await expect(jumbo).toBeVisible({ timeout: 15_000 });
  await expect(jumbo).toHaveAttribute("data-emoji-count", "3");
  expect(await fontSizePx(jumbo)).toBe(36);
  await ctx.close();
});

test("surrounding whitespace doesn't disqualify jumbo", async ({ browser }) => {
  // A user typing "  🌅 🌙  " is still emoji-only — two clusters.
  const { ctx, page, composer } = await openChat(browser, "jumbo-whitespace");
  await send(composer, "  🌅 🌙  ");
  const jumbo = page.locator('[data-testid="emoji-jumbo"]').last();
  await expect(jumbo).toBeVisible({ timeout: 15_000 });
  await expect(jumbo).toHaveAttribute("data-emoji-count", "2");
  expect(await fontSizePx(jumbo)).toBe(44);
  await ctx.close();
});

test("ZWJ family is one cluster (jumbo-1)", async ({ browser }) => {
  // 👨‍👩‍👧 is a single grapheme: three people codepoints joined by ZWJs.
  // The parser folds it into one cluster; the rendered body is jumbo-1.
  const { ctx, page, composer } = await openChat(browser, "jumbo-zwj-family");
  await send(composer, "👨‍👩‍👧");
  const jumbo = page.locator('[data-testid="emoji-jumbo"]').last();
  await expect(jumbo).toBeVisible({ timeout: 15_000 });
  await expect(jumbo).toHaveAttribute("data-emoji-count", "1");
  expect(await fontSizePx(jumbo)).toBe(54);
  await ctx.close();
});

test("four emoji body renders as a normal paragraph, no jumbo", async ({
  browser,
}) => {
  const { ctx, page, composer } = await openChat(browser, "jumbo-four");
  await send(composer, "🌅🌙🔥👀");
  // The row must render — otherwise the absence of the jumbo testid is
  // vacuous. Anchor on the rendered message text via the .msg-row
  // selector; the last() picks the just-sent message.
  const msgRow = page.locator(".msg-row").last();
  await expect(msgRow).toBeVisible({ timeout: 15_000 });
  // And the row must contain the emoji content (proving render happened).
  await expect(msgRow).toContainText("🌅");
  // The body should be a normal paragraph, not the jumbo wrapper.
  await expect(msgRow.locator('[data-testid="emoji-jumbo"]')).toHaveCount(0);
  await ctx.close();
});

test("mixed text and emoji renders as a normal paragraph, no jumbo", async ({
  browser,
}) => {
  const { ctx, page, composer } = await openChat(browser, "jumbo-mixed");
  await send(composer, "hi 🌅");
  const msgRow = page
    .locator(".msg-row", { hasText: "hi" })
    .last();
  await expect(msgRow).toBeVisible({ timeout: 15_000 });
  await expect(msgRow.locator('[data-testid="emoji-jumbo"]')).toHaveCount(0);
  await ctx.close();
});

test("keycap-base codepoints render as plain text, no jumbo", async ({
  browser,
}) => {
  // Digits / `#` / `*` all carry Emoji=YES but render as text by default;
  // the parser's `EmojiStatus` blacklist must reject them.
  const { ctx, page, composer } = await openChat(browser, "jumbo-keycap");
  await send(composer, "123");
  const msgRow = page.locator(".msg-row", { hasText: "123" }).last();
  await expect(msgRow).toBeVisible({ timeout: 15_000 });
  await expect(msgRow.locator('[data-testid="emoji-jumbo"]')).toHaveCount(0);
  await ctx.close();
});
