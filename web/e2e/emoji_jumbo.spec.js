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

// `getComputedStyle` so a stylesheet override that broke the inline style
// would surface — reading the `style` attribute alone would not.
async function computedFontSizePx(locator) {
  const raw = await locator.evaluate((el) => getComputedStyle(el).fontSize);
  return parseFloat(raw);
}

async function assertJumbo(page, { count, fontPx, renderedText }) {
  const jumbo = page.locator('[data-testid="emoji-jumbo"]').last();
  await expect(jumbo).toBeVisible({ timeout: 15_000 });
  await expect(jumbo).toHaveAttribute("data-emoji-count", String(count));
  expect(await computedFontSizePx(jumbo)).toBe(fontPx);
  if (renderedText !== undefined) {
    await expect(jumbo).toHaveText(renderedText);
  }
}

// Negative cases need a row-rendered anchor — `toHaveCount(0)` alone would
// pass vacuously if the row never appeared.
async function assertNotJumbo(page, { rowText }) {
  const msgRow = page.locator(".msg-row", { hasText: rowText }).last();
  await expect(msgRow).toBeVisible({ timeout: 15_000 });
  await expect(msgRow).toContainText(rowText);
  await expect(msgRow.locator('[data-testid="emoji-jumbo"]')).toHaveCount(0);
}

const jumboCases = [
  { name: "one emoji", hash: "jumbo-one", body: "🌅", count: 1, fontPx: 54 },
  { name: "two emoji", hash: "jumbo-two", body: "🌅🌙", count: 2, fontPx: 44 },
  {
    name: "three emoji",
    hash: "jumbo-three",
    body: "🌅🌙🔥",
    count: 3,
    fontPx: 36,
  },
  {
    name: "ZWJ family as one cluster",
    hash: "jumbo-zwj-family",
    body: "👨\u{200D}👩\u{200D}👧",
    count: 1,
    fontPx: 54,
  },
];

for (const { name, hash, body, count, fontPx } of jumboCases) {
  test(`${name}: data-emoji-count=${count}, font-size=${fontPx}px`, async ({
    browser,
  }) => {
    const { ctx, page, composer } = await openChat(browser, hash);
    await send(composer, body);
    await assertJumbo(page, { count, fontPx });
    await ctx.close();
  });
}

test("surrounding whitespace doesn't disqualify jumbo and is stripped on render", async ({
  browser,
}) => {
  const { ctx, page, composer } = await openChat(browser, "jumbo-whitespace");
  await send(composer, "  🌅 🌙  ");
  await assertJumbo(page, { count: 2, fontPx: 44, renderedText: "🌅🌙" });
  await ctx.close();
});

const notJumboCases = [
  {
    name: "four emoji",
    hash: "jumbo-four",
    body: "🌅🌙🔥👀",
    rowText: "🌅🌙🔥👀",
  },
  {
    name: "mixed text and emoji",
    hash: "jumbo-mixed",
    body: "hi 🌅",
    rowText: "hi 🌅",
  },
  {
    name: "keycap-base codepoints",
    hash: "jumbo-keycap",
    body: "123",
    rowText: "123",
  },
];

for (const { name, hash, body, rowText } of notJumboCases) {
  test(`${name} renders as a normal paragraph, no jumbo`, async ({
    browser,
  }) => {
    const { ctx, page, composer } = await openChat(browser, hash);
    await send(composer, body);
    await assertNotJumbo(page, { rowText });
    await ctx.close();
  });
}
