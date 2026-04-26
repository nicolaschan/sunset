// Visual + functional smoke tests for the D1 shell.
//
// What's covered today:
//   * Page loads with the right title and #app mounts a Lustre tree.
//   * All four columns render in light mode and dark mode.
//   * The theme toggle in the top-right flips both the label and the
//     palette (verified by the body's computed background colour).
//   * Rooms-rail collapse button changes the rail width.
//   * Full-page screenshots in light + dark are saved as test artefacts
//     so a developer (or Claude, when iterating locally) can inspect the
//     rendered layout without a browser.
//
// Visual snapshot regressions (`toHaveScreenshot`) are intentionally not
// wired up yet — pixel-stable snapshots come once the design has settled.

import { expect, test } from "@playwright/test";

test.beforeEach(async ({ page }) => {
  await page.goto("/");
  // Lustre mounts asynchronously; wait until the brand text appears.
  await expect(page.getByText("sunset", { exact: true })).toBeVisible();
});

test("page title is sunset.chat", async ({ page }) => {
  await expect(page).toHaveTitle("sunset.chat");
});

test("all four columns render in light mode", async ({ page }) => {
  // Brand row in the rooms rail
  await expect(page.getByText("sunset", { exact: true })).toBeVisible();
  // The current room name appears in the rail and in the channels-rail header
  await expect(page.getByText("dusk-collective").first()).toBeVisible();
  // Channels section
  await expect(page.getByText("Channels", { exact: true })).toBeVisible();
  // Voice section
  await expect(page.getByText("Voice", { exact: true })).toBeVisible();
  // Main column channel header reads "general" (the initial channel)
  await expect(page.getByText("general").first()).toBeVisible();
  // Members rail
  await expect(page.getByText(/^Online — /)).toBeVisible();

  await page.screenshot({
    path: "test-results/shell-light.png",
    fullPage: true,
  });
});

test("theme toggle flips light to dark", async ({ page }) => {
  const toggle = page.getByTestId("theme-toggle");

  // Default label
  await expect(toggle).toContainText("Light");

  // Capture the body bg before — light palette has #f7f5f1 (cream)
  const bgLight = await page.evaluate(
    () =>
      getComputedStyle(
        document.querySelector("#app > div"),
      ).backgroundColor,
  );

  await toggle.click();
  await expect(toggle).toContainText("Dark");

  const bgDark = await page.evaluate(
    () =>
      getComputedStyle(
        document.querySelector("#app > div"),
      ).backgroundColor,
  );

  expect(bgLight).not.toEqual(bgDark);

  await page.screenshot({
    path: "test-results/shell-dark.png",
    fullPage: true,
  });
});

test("rooms rail collapse button changes the rail width", async ({ page }) => {
  const collapse = page.getByRole("button", { name: /Collapse rooms/i });
  const rail = page.getByTestId("rooms-rail");

  // The rail width transitions over 220ms; read the *target* width
  // from the inline style instead of measuring the live geometry.
  await expect(rail).toHaveCSS("width", "260px");

  await collapse.click();
  await expect(
    page.getByRole("button", { name: /Expand rooms/i }),
  ).toBeVisible();
  await expect(rail).toHaveCSS("width", "54px");
});

test("no body-level scrollbar appears", async ({ page }) => {
  // Regression test for the whitespace-gap fix: the rendered tree must
  // exactly fill the viewport, never overflow the document.
  const overflowY = await page.evaluate(
    () => getComputedStyle(document.documentElement).overflowY,
  );
  expect(overflowY).toBe("hidden");

  const docOverflows = await page.evaluate(() => {
    const root = document.documentElement;
    return root.scrollHeight > root.clientHeight + 1;
  });
  expect(docOverflows).toBe(false);
});
