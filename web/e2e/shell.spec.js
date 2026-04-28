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
//
// Five tests below are `test.skip`'d. They asserted on `fixture.messages()`
// rendered into the DOM (e.g., `.msg-row` containing "routing thru ravi"),
// but Plan E swapped the message source from fixtures to the live
// sunset-sync engine, so on a fresh page load the messages list is empty.
// Refactor each to first send a message via the engine + wait for it to
// land before exercising the hover/react/details-panel interactions.

import { expect, test } from "@playwright/test";

test.beforeEach(async ({ page }) => {
  // The chat shell now lives behind a hash-based route. Clear any
  // persisted joined-rooms state ONCE, then navigate directly to a
  // known fixture room so the existing layout tests render the chat
  // shell and not the landing page.
  await page.goto("/");
  await page.evaluate(() => {
    try {
      localStorage.clear();
    } catch {}
  });
  await page.goto("/#dusk-collective");
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

  // The icon-only button advertises its target mode via title.
  await expect(toggle).toHaveAttribute("title", /dark/i);

  // Capture the body bg before — light palette has #f7f5f1 (cream)
  const bgLight = await page.evaluate(
    () =>
      getComputedStyle(
        document.querySelector("#app > div"),
      ).backgroundColor,
  );

  await toggle.click();
  await expect(toggle).toHaveAttribute("title", /light/i);

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

test("theme choice persists across reloads", async ({ page }) => {
  const toggle = page.getByTestId("theme-toggle");
  // Start in light mode (the default for this beforeEach setup).
  await expect(toggle).toHaveAttribute("title", /dark/i);

  await toggle.click();
  await expect(toggle).toHaveAttribute("title", /light/i);

  // Reload — saved theme should be restored.
  await page.reload();
  await expect(page.getByTestId("theme-toggle")).toHaveAttribute(
    "title",
    /light/i,
  );
});

test.describe("system theme default", () => {
  test.use({ colorScheme: "dark" });

  test("with no saved choice, the OS dark preference wins", async ({
    page,
  }) => {
    // Use a dedicated emulated colorScheme + an isolated localStorage.
    await page.goto("/");
    await page.evaluate(() => {
      try {
        localStorage.clear();
      } catch {}
    });
    await page.goto("/#dusk-collective");
    await expect(page.getByText("sunset", { exact: true })).toBeVisible();
    await expect(page.getByTestId("theme-toggle")).toHaveAttribute(
      "title",
      /light/i,
    );
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

test.skip("hover on a message reveals the action toolbar", async ({ page }) => {
  // The actions toolbar exists in the DOM but is invisible (opacity: 0)
  // until the parent .msg-row is hovered.
  const row = page.locator(".msg-row", { hasText: "routing thru ravi" });
  await expect(row).toBeVisible();

  const actions = row.locator(".msg-actions");

  const initial = await actions.evaluate((el) => getComputedStyle(el).opacity);
  expect(parseFloat(initial)).toBeLessThan(0.5);

  // Hover the row itself — hovering the inner body text isn't a reliable
  // way to activate :hover on the parent across all engines.
  await row.hover();
  // Opacity transitions over 120ms; poll until the hover-state CSS kicks in.
  await expect
    .poll(async () =>
      parseFloat(await actions.evaluate((el) => getComputedStyle(el).opacity)),
    )
    .toBeGreaterThan(0.5);
});

test.skip("react button opens the emoji picker; clicking an emoji reacts", async ({
  page,
}) => {
  const row = page.locator(".msg-row", { hasText: "routing thru ravi" });
  await row.hover();
  await row.getByRole("button", { name: /^React$/ }).click();

  const picker = page.getByTestId("reaction-picker");
  await expect(picker).toBeVisible();

  // Click a fresh emoji that the message doesn't already have.
  await picker.getByRole("button", { name: /🔥/ }).click();
  await expect(picker).not.toBeVisible();

  // The new reaction pill should now appear under the message body.
  await expect(row.getByText("🔥")).toBeVisible();
});

test.skip("info button opens the details side panel with sender + receipts", async ({
  page,
}) => {
  const row = page.locator(".msg-row", { hasText: "routing thru ravi" });
  await row.hover();
  await row.getByRole("button", { name: /Message details/i }).click();

  const panel = page.getByTestId("details-panel");
  await expect(panel).toBeVisible();
  await expect(panel.getByText("Message details")).toBeVisible();
  await expect(panel.getByText(/8f3c…a2/)).toBeVisible();

  // Receipts include the four people from the fixture.
  for (const name of ["ravi", "elena", "tomo", "june"]) {
    await expect(panel.getByText(name, { exact: false }).first()).toBeVisible();
  }

  await page.screenshot({
    path: "test-results/shell-details-open.png",
    fullPage: true,
  });

  // Closing the panel returns the right column to the members rail.
  await page.getByTestId("details-close").click();
  await expect(panel).not.toBeVisible();
  await expect(page.getByText(/^Online — /)).toBeVisible();
});

test.skip("info button is disabled while a message is still sending", async ({
  page,
}) => {
  // The pending message (m7) hasn't been delivered yet, so its
  // crypto chain + receipts aren't populated. The info button should
  // render but be disabled until the send completes.
  const row = page.locator(".msg-row", { hasText: "noted. pushing a fix" });
  await row.hover();
  const info = row.getByRole("button", { name: /Message details/i });
  await expect(info).toBeDisabled();
});

test.skip("info button on a delivered incoming message also opens details", async ({
  page,
}) => {
  // Round-3: every fixture row except the pending one carries mocked
  // crypto + receipts. Sanity-check that an incoming message (m1, sent
  // by noor) opens a populated panel with the sender hash and at
  // least one receipt row.
  const row = page.locator(".msg-row", { hasText: "shipping the relay path" });
  await row.hover();
  await row.getByRole("button", { name: /Message details/i }).click();

  const panel = page.getByTestId("details-panel");
  await expect(panel).toBeVisible();
  await expect(panel.getByText(/9b1d…74/)).toBeVisible();
  await expect(panel.getByTestId("receipt-row").first()).toBeVisible();
  await page.getByTestId("details-close").click();
});
