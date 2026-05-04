// Landing page + URL-hash routing + sidebar join/delete + search.
//
// These tests run with a clean localStorage; the global beforeEach
// clears it before each navigation.

import { expect, test } from "@playwright/test";
import { openRoomsDrawer } from "./helpers/viewport.js";

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

  test("rooms-rail search filters the visible rooms", async ({ page }, testInfo) => {
    await page.goto("/#dusk-collective");
    // Add a second room from the sidebar so we have two to filter.
    await openRoomsDrawer(page, testInfo);
    await page.getByTestId("rooms-search").fill("design-crit");
    await page.getByTestId("rooms-search-join").click();
    await expect(page).toHaveURL(/#design-crit$/);

    // Filter by "dusk" — only dusk-collective should remain visible.
    await openRoomsDrawer(page, testInfo);
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
  }, testInfo) => {
    await page.goto("/");
    // Join two rooms.
    await page.getByTestId("landing-input").fill("dusk-collective");
    await page.getByTestId("landing-input").press("Enter");
    await openRoomsDrawer(page, testInfo);
    await page.getByTestId("rooms-search").fill("design-crit");
    await page.getByTestId("rooms-search").press("Enter");
    await expect(page).toHaveURL(/#design-crit$/);

    // Delete the active room (design-crit). On desktop the per-row
    // delete button is `opacity: 0` until the row is `:hover`'d, so we
    // hover the row first to reveal the button; that's what a real user
    // does and it lets Playwright's normal actionability check work.
    // On touch (mobile-chrome) `@media (hover: none)` keeps the button
    // visible always, so the explicit hover is a no-op there.
    await openRoomsDrawer(page, testInfo);
    const designCritRow = page
      .getByTestId("rooms-rail")
      .locator(".room-row", { hasText: "design-crit" });
    await designCritRow.hover();
    await designCritRow.getByTestId("room-delete").click();

    // We should now be looking at dusk-collective (the only remaining room).
    await expect(page).toHaveURL(/#dusk-collective$/);
    await expect(
      page.getByTestId("rooms-rail").getByText("design-crit"),
    ).not.toBeVisible();

    // On mobile the rooms drawer stays open after deletion. Reload the page to
    // reset all in-memory state (drawer open/closed), then re-open the drawer.
    await page.reload();
    if (testInfo.project.name === "mobile-chrome") {
      await expect(page.getByTestId("phone-header")).toBeVisible();
    } else {
      await expect(page.getByText("sunset", { exact: true })).toBeVisible();
    }
    await openRoomsDrawer(page, testInfo);

    // Delete the last remaining room → back to landing.
    const duskRow = page
      .getByTestId("rooms-rail")
      .locator(".room-row", { hasText: "dusk-collective" });
    await duskRow.hover();
    await duskRow.getByTestId("room-delete").click();

    await expect(page.getByTestId("landing-view")).toBeVisible();
  });

  test("selecting a room does not reorder the list", async ({ page }, testInfo) => {
    // Join three rooms in a known order. Joins prepend so the
    // resulting top-to-bottom order is gamma → beta → alpha.
    await page.goto("/");
    await page.getByTestId("landing-input").fill("alpha");
    await page.getByTestId("landing-input").press("Enter");
    await openRoomsDrawer(page, testInfo);
    await page.getByTestId("rooms-search").fill("beta");
    await page.getByTestId("rooms-search").press("Enter");
    await openRoomsDrawer(page, testInfo);
    await page.getByTestId("rooms-search").fill("gamma");
    await page.getByTestId("rooms-search").press("Enter");

    const railOrder = async () =>
      page
        .getByTestId("rooms-rail")
        .locator(".room-row")
        .evaluateAll((rows) =>
          rows.map((r) => r.getAttribute("data-room-name") || ""),
        );

    await openRoomsDrawer(page, testInfo);
    await expect.poll(async () => (await railOrder()).slice(0, 3)).toEqual([
      "gamma",
      "beta",
      "alpha",
    ]);

    // Click the alpha row at the bottom — it should NOT bubble up.
    // Playwright's actionability check fails on the Pixel 7 viewport
    // for this drawer-internal row, so we dispatch via JS. The click
    // handler lives on the inner <button>, not the .room-row wrapper.
    await page
      .getByTestId("rooms-rail")
      .locator('.room-row[data-room-name="alpha"] button')
      .first()
      .evaluate((el) => el.click());
    await expect(page).toHaveURL(/#alpha$/);

    await openRoomsDrawer(page, testInfo);
    await expect.poll(async () => (await railOrder()).slice(0, 3)).toEqual([
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

  test.describe("phone — landing", () => {
    test.beforeEach(async ({ page }, testInfo) => {
      test.skip(testInfo.project.name !== "mobile-chrome", "phone-only test");
    });

    test("landing fills the viewport edge-to-edge", async ({ page }) => {
      await page.goto("/");
      await expect(page.getByTestId("landing-view")).toBeVisible();
      const input = page.getByTestId("landing-input");
      await expect(input).toBeVisible();
      const inputBox = await input.boundingBox();
      const viewport = page.viewportSize();
      // Input should be near full-width minus our 24px gutters.
      expect(inputBox.width).toBeGreaterThan(viewport.width - 60);
    });

    // On iOS PWA standalone with `viewport-fit=cover` the page extends
    // under the status bar / home indicator. The landing wrapper paints
    // its own bg via `position: fixed; inset: 0`, but rubber-banding
    // and the safe-area bands can expose whatever's behind it. Without
    // an html/body bg in landing's reset_style, those regions show the
    // OS default (white in light mode) — a visible band stuck to the
    // screen edges. Even at zero inset, a missing body bg shows up as
    // a computed colour mismatch between the landing wrapper and html.
    test("landing html bg matches the landing wrapper bg", async ({ page }) => {
      await page.goto("/");
      await expect(page.getByTestId("landing-view")).toBeVisible();
      const colours = await page.evaluate(() => {
        const wrapper = document.querySelector('[data-testid="landing-view"]');
        return {
          wrapper: getComputedStyle(wrapper).backgroundColor,
          html: getComputedStyle(document.documentElement).backgroundColor,
          body: getComputedStyle(document.body).backgroundColor,
        };
      });
      expect(
        colours.html,
        "html bg should match landing wrapper bg so iOS safe-area bands don't show OS default",
      ).toBe(colours.wrapper);
      expect(
        colours.body,
        "body bg should match landing wrapper bg",
      ).toBe(colours.wrapper);
    });
  });

  test.describe("phone — touch drag-drop", () => {
    test.beforeEach(async ({ page }, testInfo) => {
      test.skip(testInfo.project.name !== "mobile-chrome", "phone-only test");
    });

    test("long-press + drag reorders rooms", async ({ page }) => {
      // Add three rooms in known order via landing + sidebar search.
      // Rooms-drawer navigation: toggle opens channels-drawer; tapping
      // the room title inside opens rooms-drawer. Wait for each drawer
      // to be visible before interacting (220ms CSS slide-in transition).
      await page.goto("/");
      await page.evaluate(() => { try { localStorage.clear(); } catch {} });
      await page.goto("/");
      await page.getByTestId("landing-input").fill("alpha");
      await page.getByTestId("landing-input").press("Enter");

      // Helper: wait for a drawer to finish sliding in (translateX(0)).
      // Playwright's toBeVisible / toBeInViewport don't wait for the 220ms
      // CSS transition to complete, so we poll the transform directly.
      const waitForDrawerOpen = (testId) =>
        page.waitForFunction((tid) => {
          const el = document.querySelector(`[data-testid="${tid}"]`);
          if (!el) return false;
          const rect = el.getBoundingClientRect();
          return rect.x >= -1; // fully in-viewport (or essentially so)
        }, testId, { timeout: 3000 });

      // Open channels → rooms, add beta. After JoinRoom on phone the
      // drawer transitions to channels-drawer for the new room, so on
      // subsequent iterations we only need to swap to rooms-drawer.
      await page.getByTestId("phone-rooms-toggle").click();
      await waitForDrawerOpen("channels-drawer");
      await page.getByTestId("channels-room-title").click();
      await waitForDrawerOpen("rooms-drawer");
      await page.getByTestId("rooms-drawer").getByTestId("rooms-search").fill("beta");
      await page.getByTestId("rooms-drawer").getByTestId("rooms-search").press("Enter");

      // Channels-drawer is now open; swap to rooms-drawer and add gamma.
      await waitForDrawerOpen("channels-drawer");
      await page.getByTestId("channels-room-title").click();
      await waitForDrawerOpen("rooms-drawer");
      await page.getByTestId("rooms-drawer").getByTestId("rooms-search").fill("gamma");
      await page.getByTestId("rooms-drawer").getByTestId("rooms-search").press("Enter");

      // Re-open the rooms drawer to read bounding boxes.
      await waitForDrawerOpen("channels-drawer");
      await page.getByTestId("channels-room-title").click();
      await waitForDrawerOpen("rooms-drawer");

      const drawer = page.getByTestId("rooms-drawer");

      await expect.poll(async () =>
        drawer.locator("[data-room-row]").evaluateAll((rows) =>
          rows.map((r) => r.getAttribute("data-room-row")),
        ),
      ).toEqual(["gamma", "beta", "alpha"]);

      // Simulate touch long-press on alpha then drag onto gamma.
      const alpha = drawer.locator('[data-room-row="alpha"]');
      const gamma = drawer.locator('[data-room-row="gamma"]');
      const aBox = await alpha.boundingBox();
      const gBox = await gamma.boundingBox();

      await page.evaluate(
        ({ ax, ay, gx, gy }) => {
          const fire = (type, x, y) => {
            const ev = new PointerEvent(type, {
              pointerType: "touch",
              clientX: x,
              clientY: y,
              bubbles: true,
              cancelable: true,
            });
            document.dispatchEvent(ev);
          };
          fire("pointerdown", ax, ay);
          return new Promise((res) => setTimeout(() => {
            fire("pointermove", gx, gy);
            fire("pointerup", gx, gy);
            res();
          }, 450));
        },
        {
          ax: aBox.x + aBox.width / 2,
          ay: aBox.y + aBox.height / 2,
          gx: gBox.x + gBox.width / 2,
          gy: gBox.y + gBox.height / 2,
        },
      );

      await expect.poll(async () =>
        drawer.locator("[data-room-row]").evaluateAll((rows) =>
          rows.map((r) => r.getAttribute("data-room-row")),
        ),
      ).toEqual(["alpha", "gamma", "beta"]);
    });
  });
});
