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

  const overflows = await page.evaluate(() => {
    const root = document.documentElement;
    return {
      vertical: root.scrollHeight > root.clientHeight + 1,
      horizontal: root.scrollWidth > root.clientWidth + 1,
    };
  });
  expect(overflows.vertical).toBe(false);
  expect(overflows.horizontal).toBe(false);
});

test("collapsed rail hides the logo and never overflows horizontally", async ({
  page,
}) => {
  const rail = page.getByTestId("rooms-rail");

  await page.getByRole("button", { name: /Collapse rooms/i }).click();
  await expect(
    page.getByRole("button", { name: /Expand rooms/i }),
  ).toBeVisible();

  // The CSS width transition is 220ms; wait for the rail to actually finish
  // shrinking before we measure layout-derived geometry (the "is the chevron
  // centered" check below relies on the rail being its target 54px wide).
  await expect
    .poll(async () => await rail.evaluate((el) => el.getBoundingClientRect().width))
    .toBeLessThan(56);

  // No horizontal scroll on the document during/after collapse.
  const horizontal = await page.evaluate(() => {
    const r = document.documentElement;
    return r.scrollWidth > r.clientWidth + 1;
  });
  expect(horizontal).toBe(false);

  // The 28x28 logo SVG should not be rendered inside the collapsed rail.
  const logoCount = await rail
    .locator("svg")
    .evaluateAll((els) =>
      els.filter((e) => e.getAttribute("viewBox") === "0 0 28 28").length,
    );
  expect(logoCount).toBe(0);

  // The chevron should be visually centred in the 54px rail. Compare the
  // chevron's bounding-rect centre against the rail's centre with a small
  // tolerance for sub-pixel rounding.
  const offsets = await rail.evaluate((railEl) => {
    const expand = railEl.querySelector('button[title*="Expand"]');
    const railRect = railEl.getBoundingClientRect();
    const btnRect = expand.getBoundingClientRect();
    return {
      railCenter: railRect.left + railRect.width / 2,
      btnCenter: btnRect.left + btnRect.width / 2,
    };
  });
  expect(Math.abs(offsets.btnCenter - offsets.railCenter)).toBeLessThanOrEqual(1);

  await page.screenshot({
    path: "test-results/shell-collapsed.png",
    fullPage: true,
  });
});

test("favicon link points at favicon.svg", async ({ page }) => {
  const href = await page.evaluate(
    () => document.querySelector('link[rel="icon"]')?.getAttribute("href"),
  );
  expect(href).toMatch(/favicon\.svg$/);
});

test("channels and main column bottom borders line up", async ({ page }) => {
  // Channels rail is the second <aside>; main column is <main>.
  const offsets = await page.evaluate(() => {
    const channelsHeader = document.querySelectorAll("aside")[1].firstElementChild;
    const mainHeader = document.querySelector("main").firstElementChild;
    return {
      channels:
        channelsHeader.getBoundingClientRect().bottom -
        document.documentElement.getBoundingClientRect().top,
      main:
        mainHeader.getBoundingClientRect().bottom -
        document.documentElement.getBoundingClientRect().top,
    };
  });
  // Allow 1px of sub-pixel slop.
  expect(Math.abs(offsets.channels - offsets.main)).toBeLessThanOrEqual(1);
});

test("column-bottom rows share a top y-coordinate", async ({ page }) => {
  // Rooms rail's pinned 'you' row, channels rail's self-control bar, and
  // main panel's composer all sit at the bottom of their column. Their
  // top borders must align horizontally so the layout reads as a single
  // bottom seam across the screen.
  const tops = await page.evaluate(() => {
    const rooms = document.querySelectorAll("aside")[0];
    const channels = document.querySelectorAll("aside")[1];
    const main = document.querySelector("main");
    const lastChild = (parent) => parent.children[parent.children.length - 1];
    return {
      rooms: lastChild(rooms).getBoundingClientRect().top,
      channels: lastChild(channels).getBoundingClientRect().top,
      main: lastChild(main).getBoundingClientRect().top,
    };
  });
  expect(Math.abs(tops.rooms - tops.channels)).toBeLessThanOrEqual(1);
  expect(Math.abs(tops.channels - tops.main)).toBeLessThanOrEqual(1);
});

test("channels header has no online subtitle", async ({ page }) => {
  // The 'X online' subtitle moved out of the channels-rail header in
  // round-2 polish; the count still surfaces in the members rail. The
  // header should only contain the room title + connection dot.
  const headerText = await page.evaluate(() => {
    const channels = document.querySelectorAll("aside")[1];
    return channels.firstElementChild.textContent.trim();
  });
  expect(headerText).not.toMatch(/online/i);
});

test("rooms list does not render timestamps", async ({ page }) => {
  // 'Last active' timestamps ('2m', 'now', '12m', etc.) were dropped
  // from the rooms-list rows in round-2 polish.
  const railText = await page
    .getByTestId("rooms-rail")
    .evaluate((el) => el.textContent);
  // 'now' / '2m' / '12m' / '3d' / 'just now' would all match this.
  expect(railText).not.toMatch(/\b\d+\s*(?:m|h|d)\b/);
  expect(railText).not.toMatch(/\bjust now\b/i);
  expect(railText).not.toMatch(/(?<!\w)now(?!\w)/);
});
