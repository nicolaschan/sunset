// Verifies the consistent-emoji webfont (Noto Color Emoji, served by
// Google Fonts) is wired into the page so emoji codepoints render the
// same on every host instead of falling back to whatever OS-installed
// emoji font happens to be available (Apple Color Emoji on macOS,
// Segoe UI Emoji on Windows, a B&W glyph stub on most Linux distros).
//
// Three things have to be true for this to work end-to-end:
//
//   1. The page declares `Noto Color Emoji` somewhere in the
//      font-family stack on the chat surface — without it the browser
//      never asks for the font.
//   2. The Google Fonts CSS request actually includes the family —
//      without it the font-face declaration is missing and the
//      browser silently falls back.
//   3. The browser ends up loading a stylesheet that contains an
//      `@font-face` rule for the family — proving the request reached
//      Google Fonts and the response wired the family up.
//
// The fourth check (the chat surface actually paints an emoji using
// it) is harder to assert across engines: `document.fonts.check`
// returns true for *any* available font that claims to cover the
// codepoint, and computed `font-family` only reflects the stack, not
// which family the renderer settled on. We rely on (1)+(2)+(3); a
// regression in any of them breaks the consistent-rendering promise.

import { expect, test } from "@playwright/test";

const NOTO = "Noto Color Emoji";

test.beforeEach(async ({ page }) => {
  await page.goto("/");
  await page.evaluate(() => {
    try {
      localStorage.clear();
    } catch {}
  });
  await page.goto("/#dusk-collective");
  await expect(page.getByText("sunset", { exact: true })).toBeVisible();
});

test("the chat shell's font-family stack falls back to Noto Color Emoji", async ({
  page,
}) => {
  // `theme.font_sans` is applied to the top-level shell wrapper; every
  // descendant inherits it. Reading the computed style off the chat
  // <main> element gives us what the renderer will actually consult
  // when it encounters an emoji codepoint in a message body.
  const fontFamily = await page.evaluate(
    () => getComputedStyle(document.querySelector("main")).fontFamily,
  );
  expect(fontFamily).toContain(NOTO);
});

test("the Google Fonts <link> requests the Noto Color Emoji family", async ({
  page,
}) => {
  // The browser only fetches a font family if its name shows up in
  // the stylesheet request URL. We check the actual rendered <link>
  // rather than re-parsing gleam.toml so a refactor (e.g. swapping to
  // self-hosting) keeps this contract honest.
  const hrefs = await page.evaluate(() =>
    Array.from(document.querySelectorAll('link[rel="stylesheet"]')).map(
      (l) => l.href,
    ),
  );
  const fontHref = hrefs.find(
    (h) => h.includes("fonts.googleapis.com") && h.includes("Noto+Color+Emoji"),
  );
  expect(
    fontHref,
    `expected a Google Fonts <link> requesting Noto+Color+Emoji; got: ${JSON.stringify(hrefs)}`,
  ).toBeTruthy();
});

test("the browser registers a Noto Color Emoji face from the loaded stylesheet", async ({
  page,
}) => {
  // After the Google Fonts stylesheet loads, the browser parses its
  // `@font-face` declarations and registers them in `document.fonts`.
  // If the family ever silently dropped out of the URL — or Google
  // Fonts returned an empty response for it — this set would not
  // contain `Noto Color Emoji`, and emoji rendering would fall back
  // to whatever the OS happens to ship. We poll because the
  // `display=swap` strategy means face registration races against
  // page navigation; a couple of frames is normal.
  await expect
    .poll(
      async () =>
        await page.evaluate((family) => {
          for (const face of document.fonts) {
            if (face.family === family) return true;
          }
          return false;
        }, NOTO),
      {
        message: `expected document.fonts to contain a "${NOTO}" face`,
        timeout: 10_000,
      },
    )
    .toBe(true);
});
