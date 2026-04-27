// Landing page + URL-hash routing + sidebar join/delete + search.
//
// These tests run with a clean localStorage; the global beforeEach
// clears it before each navigation.

import { expect, test } from "@playwright/test";

test.describe("landing + routing", () => {
  test.beforeEach(async ({ page }) => {
    // One-shot clear before each test so subsequent navigations
    // within the test retain the state we set up.
    await page.goto("/");
    await page.evaluate(() => {
      try {
        localStorage.clear();
      } catch {}
    });
  });

  test("root with no joined rooms shows the landing view", async ({ page }) => {
    await page.goto("/");
    await expect(page.getByTestId("landing-view")).toBeVisible();
    await expect(page.getByTestId("landing-input")).toBeVisible();
    await expect(page.getByTestId("landing-join")).toBeDisabled();
  });

  test("typing a room name and pressing Enter navigates to /#room", async ({
    page,
  }) => {
    await page.goto("/");
    const input = page.getByTestId("landing-input");
    await input.fill("dusk-collective");
    await input.press("Enter");

    await expect(
      page.locator('aside[data-testid="rooms-rail"]'),
    ).toBeVisible();
    await expect(page).toHaveURL(/#dusk-collective$/);
    // The sidebar's rooms list now contains the joined room.
    await expect(
      page.getByTestId("rooms-rail").getByText("dusk-collective"),
    ).toBeVisible();
  });

  test("clicking the Join button has the same effect", async ({ page }) => {
    await page.goto("/");
    await page.getByTestId("landing-input").fill("design-crit");
    await page.getByTestId("landing-join").click();
    await expect(page).toHaveURL(/#design-crit$/);
    await expect(page.getByTestId("rooms-rail")).toBeVisible();
  });

  test("returning to / with previously-used rooms goes straight to last used", async ({
    page,
  }) => {
    await page.goto("/");
    await page.getByTestId("landing-input").fill("dusk-collective");
    await page.getByTestId("landing-input").press("Enter");
    await expect(page).toHaveURL(/#dusk-collective$/);

    // Now visit / (no fragment) — should auto-redirect to the last-used room.
    await page.goto("/");
    await expect(page).toHaveURL(/#dusk-collective$/);
    await expect(page.getByTestId("rooms-rail")).toBeVisible();
  });

  test("rooms-rail search filters the visible rooms", async ({ page }) => {
    await page.goto("/#dusk-collective");
    // Add a second room from the sidebar so we have two to filter.
    await page.getByTestId("rooms-search").fill("design-crit");
    await page.getByTestId("rooms-search-join").click();
    await expect(page).toHaveURL(/#design-crit$/);

    // Filter by "dusk" — only dusk-collective should remain visible.
    await page.getByTestId("rooms-search").fill("dusk");
    await expect(
      page.getByTestId("rooms-rail").getByText("dusk-collective"),
    ).toBeVisible();
    await expect(
      page.getByTestId("rooms-rail").getByText("design-crit"),
    ).not.toBeVisible();

    // Clear filter — both visible again.
    await page.getByTestId("rooms-search").fill("");
    await expect(
      page.getByTestId("rooms-rail").getByText("design-crit"),
    ).toBeVisible();
  });

  test("Enter on a non-matching search term joins it as a new room", async ({
    page,
  }) => {
    await page.goto("/#dusk-collective");
    await page.getByTestId("rooms-search").fill("party-room");
    await page.getByTestId("rooms-search").press("Enter");

    await expect(page).toHaveURL(/#party-room$/);
    await expect(
      page.getByTestId("rooms-rail").getByText("party-room"),
    ).toBeVisible();
  });

  test("delete button removes a room; deleting all returns to landing", async ({
    page,
  }) => {
    await page.goto("/");
    // Join two rooms.
    await page.getByTestId("landing-input").fill("dusk-collective");
    await page.getByTestId("landing-input").press("Enter");
    await page.getByTestId("rooms-search").fill("design-crit");
    await page.getByTestId("rooms-search").press("Enter");
    await expect(page).toHaveURL(/#design-crit$/);

    // Delete the active room (design-crit). The rail's per-row delete
    // button is hidden until hover; force the click since the room-row
    // wrappers carry the hover-only styling.
    await page
      .getByTestId("rooms-rail")
      .locator('.room-row', { hasText: "design-crit" })
      .getByTestId("room-delete")
      .click({ force: true });

    // We should now be looking at dusk-collective (the only remaining room).
    await expect(page).toHaveURL(/#dusk-collective$/);
    await expect(
      page.getByTestId("rooms-rail").getByText("design-crit"),
    ).not.toBeVisible();

    // Delete the last remaining room → back to landing.
    await page
      .getByTestId("rooms-rail")
      .locator('.room-row', { hasText: "dusk-collective" })
      .getByTestId("room-delete")
      .click({ force: true });

    await expect(page.getByTestId("landing-view")).toBeVisible();
  });

  test("selecting a room does not reorder the list", async ({ page }) => {
    // Join three rooms in a known order. Joins prepend so the
    // resulting top-to-bottom order is gamma → beta → alpha.
    await page.goto("/");
    await page.getByTestId("landing-input").fill("alpha");
    await page.getByTestId("landing-input").press("Enter");
    await page.getByTestId("rooms-search").fill("beta");
    await page.getByTestId("rooms-search").press("Enter");
    await page.getByTestId("rooms-search").fill("gamma");
    await page.getByTestId("rooms-search").press("Enter");

    const railOrder = async () =>
      page
        .getByTestId("rooms-rail")
        .locator(".room-row")
        .evaluateAll((rows) =>
          rows.map((r) => r.getAttribute("data-room-name") || ""),
        );

    expect((await railOrder()).slice(0, 3)).toEqual([
      "gamma",
      "beta",
      "alpha",
    ]);

    // Click the alpha row at the bottom — it should NOT bubble up.
    await page
      .getByTestId("rooms-rail")
      .locator(".room-row", { hasText: "alpha" })
      .click();
    await expect(page).toHaveURL(/#alpha$/);

    expect((await railOrder()).slice(0, 3)).toEqual([
      "gamma",
      "beta",
      "alpha",
    ]);
  });

  test("drag-drop reorders rooms and persists across reloads", async ({
    page,
  }) => {
    await page.goto("/");
    await page.getByTestId("landing-input").fill("alpha");
    await page.getByTestId("landing-input").press("Enter");
    await page.getByTestId("rooms-search").fill("beta");
    await page.getByTestId("rooms-search").press("Enter");
    await page.getByTestId("rooms-search").fill("gamma");
    await page.getByTestId("rooms-search").press("Enter");

    const railOrder = async () =>
      page
        .getByTestId("rooms-rail")
        .locator(".room-row")
        .evaluateAll((rows) =>
          rows.map((r) => r.getAttribute("data-room-name") || ""),
        );

    // The third Enter dispatches JoinRoom and resets the search, but
    // Lustre's render runs in a microtask after press() resolves. Poll
    // until all three rooms are mounted before we reach in to fire the
    // raw drag events — otherwise map.alpha can be undefined.
    await expect.poll(railOrder).toEqual(["gamma", "beta", "alpha"]);

    // HTML5 drag events aren't reliably synthesised by the WebDriver
    // protocol; dispatch them manually so the test exercises the
    // app's own dragstart / dragover / drop handlers.
    await page.evaluate(() => {
      const rows = document.querySelectorAll(
        '[data-testid="rooms-rail"] .room-row',
      );
      const map = {};
      rows.forEach((row) => {
        const name = row.getAttribute("data-room-name");
        if (name) map[name] = row;
      });
      const fire = (el, type) =>
        el.dispatchEvent(new Event(type, { bubbles: true, cancelable: true }));
      // Drag alpha (bottom) onto gamma (top) → alpha lands above gamma.
      fire(map.alpha, "dragstart");
      fire(map.gamma, "dragover");
      fire(map.gamma, "drop");
      fire(map.alpha, "dragend");
    });

    await expect.poll(railOrder).toEqual(["alpha", "gamma", "beta"]);

    // Reload — the order should survive.
    await page.reload();
    await expect.poll(railOrder).toEqual(["alpha", "gamma", "beta"]);
  });
});
