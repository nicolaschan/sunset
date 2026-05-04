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
// A handful of round-3 tests below assert on UI behavior tied to
// `.msg-row` content, which used to come from a static fixture. Plan E
// switched the message source to the live sunset-sync engine, so on a
// fresh page load the messages list is empty until the user (or test)
// sends one. The hover-toolbar test below now sends a real message
// through the composer and runs its assertions against that. The
// remaining ones — info-panel + details panel + receipts + pending
// state — depend on cross-peer crypto chains and receipts that aren't
// available without a relay + second browser; those scenarios are
// covered end-to-end by reactions.spec.js, receipts.spec.js, and the
// two_browser_chat.spec.js suite, so the duplicated single-browser
// versions stay skipped here with a pointer to their counterpart.

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

// Theme palette flip (light↔dark) and persistence-across-reloads are
// covered by the settings-popover tests in `ui_tweaks.spec.js` ("clicking
// 'you' opens settings; theme buttons flip palette"). The legacy desktop-
// only fixed pill at the bottom-right was removed; settings popover is
// the only entry point now.

test.describe("system theme default", () => {
  test.use({ colorScheme: "dark" });

  test("with no saved choice, the OS dark preference wins", async ({
    page,
  }, testInfo) => {
    test.skip(testInfo.project.name === "mobile-chrome", "desktop-only test");
    // Use a dedicated emulated colorScheme + an isolated localStorage.
    await page.goto("/");
    await page.evaluate(() => {
      try {
        localStorage.clear();
      } catch {}
    });
    await page.goto("/#dusk-collective");
    await expect(page.getByText("sunset", { exact: true })).toBeVisible();
    // Settings popover's System button is the live reflection of the
    // saved preference; with no saved choice the default is System and
    // the dark colorScheme should be honoured. The body's bg paints
    // `palette.bg` (set by global_reset) — the dark palette's bg is
    // distinctly darker than the light palette's cream, so reading the
    // computed colour and asserting it's not the light cream is a
    // robust check that doesn't lock us to a specific hex.
    const bodyBg = await page.evaluate(
      () => getComputedStyle(document.body).backgroundColor,
    );
    // Light palette uses a cream (#f7f5f1 / rgb(247, 245, 241) family);
    // dark palette is a deeply tinted near-black. Compute the average
    // channel value — under 128 means the bg is dark.
    const m = bodyBg.match(/rgb\((\d+),\s*(\d+),\s*(\d+)\)/);
    expect(m, `unexpected bg shape: ${bodyBg}`).not.toBeNull();
    const avg = (Number(m[1]) + Number(m[2]) + Number(m[3])) / 3;
    expect(avg, `bg should be dark, got ${bodyBg}`).toBeLessThan(128);

    await page.getByTestId("you-row").click();
    await expect(page.getByTestId("settings-theme-system")).toHaveAttribute(
      "aria-pressed",
      "true",
    );
  });
});

test("rooms rail collapse button changes the rail width", async ({ page }, testInfo) => {
  test.skip(testInfo.project.name === "mobile-chrome", "desktop-only test");
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
}, testInfo) => {
  test.skip(testInfo.project.name === "mobile-chrome", "desktop-only test");
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

test("apple-touch-icon link points at a 180x180 PNG that exists", async ({
  page,
}) => {
  // Apple home-screen icons must be a full-bleed opaque PNG; iOS
  // applies its own corner mask, so any rounding in the source PNG
  // shows up as a doubled edge. The PNG is baked from
  // priv/apple-touch-icon.svg by the flake's webDist installPhase.
  const link = await page.evaluate(() => {
    const el = document.querySelector('link[rel="apple-touch-icon"]');
    return el
      ? { href: el.getAttribute("href"), sizes: el.getAttribute("sizes") }
      : null;
  });
  expect(link).not.toBeNull();
  expect(link.href).toMatch(/apple-touch-icon\.png$/);
  expect(link.sizes).toBe("180x180");

  // The icon must actually be served alongside the build artefact.
  const resolved = new URL(link.href, page.url()).toString();
  const response = await page.request.get(resolved);
  expect(response.ok()).toBe(true);
  expect(response.headers()["content-type"]).toContain("image/png");

  // Decode the PNG to verify the dimensions match the link's sizes
  // attribute. We use createImageBitmap rather than Buffer-parsing so
  // the test stays portable across Playwright environments.
  const dims = await page.evaluate(async (url) => {
    const r = await fetch(url);
    const blob = await r.blob();
    const bm = await createImageBitmap(blob);
    return { width: bm.width, height: bm.height };
  }, resolved);
  expect(dims).toEqual({ width: 180, height: 180 });
});

test("manifest link points at a valid web app manifest", async ({ page }) => {
  // Add-to-Home-Screen on Chrome/Android reads `<link rel="manifest">`
  // for the icon set, name, and display mode.
  const href = await page.evaluate(
    () => document.querySelector('link[rel="manifest"]')?.getAttribute("href"),
  );
  expect(href).toMatch(/manifest\.webmanifest$/);

  const resolved = new URL(href, page.url()).toString();
  const response = await page.request.get(resolved);
  expect(response.ok()).toBe(true);
  // Some servers serve .webmanifest as text/plain; accept either as
  // long as the body is valid JSON with the required fields.
  const manifest = await response.json();
  expect(manifest.name).toBe("sunset.chat");
  expect(manifest.short_name).toBeTruthy();
  expect(manifest.display).toBe("standalone");
  // At minimum the iOS-sized icon and one square PWA icon must be
  // declared, and each must reference a fetchable PNG.
  const sizes = manifest.icons.map((i) => i.sizes);
  expect(sizes).toContain("180x180");
  expect(sizes.some((s) => s === "192x192" || s === "512x512")).toBe(true);

  for (const icon of manifest.icons) {
    if (icon.type !== "image/png") continue;
    const iconUrl = new URL(icon.src, resolved).toString();
    const iconResp = await page.request.get(iconUrl);
    expect(iconResp.ok()).toBe(true);
    expect(iconResp.headers()["content-type"]).toContain("image/png");
  }
});

test("PWA / Apple home-screen meta tags are present", async ({ page }) => {
  // theme-color paints the browser chrome; the `apple-mobile-web-app-*`
  // metas are what iOS reads when the page is launched as a standalone
  // home-screen app — without them the title bar shows the truncated
  // page title and the status bar reverts to the system default.
  const metas = await page.evaluate(() => {
    const get = (name) =>
      document
        .querySelector(`meta[name="${name}"]`)
        ?.getAttribute("content") ?? null;
    return {
      themeColor: get("theme-color"),
      appleCapable: get("apple-mobile-web-app-capable"),
      mobileCapable: get("mobile-web-app-capable"),
      statusBar: get("apple-mobile-web-app-status-bar-style"),
      title: get("apple-mobile-web-app-title"),
    };
  });
  expect(metas.themeColor).toMatch(/^#[0-9a-fA-F]{3,8}$/);
  expect(metas.appleCapable).toBe("yes");
  expect(metas.mobileCapable).toBe("yes");
  expect(metas.statusBar).toBe("black-translucent");
  expect(metas.title).toBeTruthy();
});

test("viewport meta is mobile-friendly (safe-area + keyboard resize)", async ({
  page,
}) => {
  const content = await page.evaluate(
    () => document.querySelector('meta[name="viewport"]').getAttribute("content"),
  );
  expect(content).toContain("viewport-fit=cover");
  expect(content).toContain("interactive-widget=resizes-content");
});

test("composer input font-size is at least 16px (iOS no-zoom)", async ({
  page,
}) => {
  // The composer input or textarea must render at >= 16px on phone so iOS
  // doesn't auto-zoom on focus. We assert via computed style; this passes
  // on desktop too since the inherited size is already >= 16px.
  const fontSize = await page.evaluate(() => {
    const el = document.querySelector("main input, main textarea");
    return el ? parseFloat(getComputedStyle(el).fontSize) : 0;
  });
  expect(fontSize).toBeGreaterThanOrEqual(16);
});

test("channels and main column bottom borders line up", async ({ page }, testInfo) => {
  test.skip(testInfo.project.name === "mobile-chrome", "desktop-only test");
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

test("column-bottom rows share a top y-coordinate", async ({ page }, testInfo) => {
  test.skip(testInfo.project.name === "mobile-chrome", "desktop-only test");
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

test("hover on a message reveals the action toolbar", async ({ page }, testInfo) => {
  // The actions toolbar is part of every message row but starts at
  // opacity 0; CSS makes it visible on `.msg-row:hover`. This is a
  // pure pointer-on-DOM behavior so a single browser, no relay, is
  // enough — we just need a real message in the column.
  test.skip(
    testInfo.project.name === "mobile-chrome",
    "hover-reveal toolbar is desktop-only; phone renders actions inline (covered elsewhere)",
  );

  const input = page.getByPlaceholder(/^Message #/);
  await expect(input).toBeVisible({ timeout: 15_000 });

  const text = `hover toolbar — ${Date.now()}`;
  await input.fill(text);
  await input.press("Enter");

  const row = page.locator(".msg-row", { hasText: text }).first();
  await expect(row).toBeVisible({ timeout: 15_000 });

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

// Skipped: the live e2e for "open picker + pick + chip lands" is
// reactions.spec.js (single browser, real relay). Keeping a near-
// identical fixture-based version here doesn't add coverage and
// would just need the same send-message setup.
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

test.describe("phone shell smoke", () => {
  test.beforeEach(async ({ page }, testInfo) => {
    test.skip(testInfo.project.name !== "mobile-chrome", "phone-only test");
    await page.goto("/");
    await page.evaluate(() => { try { localStorage.clear(); } catch {} });
    await page.goto("/#dusk-collective");
    await expect(page.getByTestId("phone-header")).toBeVisible();
  });

  test("phone header is visible on phone viewport", async ({ page }) => {
    await expect(page.getByTestId("phone-rooms-toggle")).toBeVisible();
    await expect(page.getByTestId("phone-members-toggle")).toBeVisible();
    await expect(page.getByTestId("phone-header").getByText("dusk-collective")).toBeVisible();
  });

  test("composer input is reachable within the visible viewport", async ({
    page,
  }) => {
    // Regression for the iOS URL bar issue: nested 100dvh containers
    // were forcing the composer below the visible viewport, where the
    // bottom URL bar covered it on iOS Safari. The composer's bottom
    // edge must sit within the viewport's bottom edge.
    const result = await page.evaluate(() => {
      const input = document.querySelector("main input, main textarea");
      if (!input) return { found: false };
      const rect = input.getBoundingClientRect();
      return {
        found: true,
        bottom: rect.bottom,
        viewportHeight: window.innerHeight,
      };
    });
    expect(result.found).toBe(true);
    expect(result.bottom).toBeLessThanOrEqual(result.viewportHeight);
  });

  test("tapping room title in channels drawer opens rooms drawer", async ({
    page,
  }) => {
    await page.getByTestId("phone-rooms-toggle").click();
    await expect(page.getByTestId("channels-drawer")).toBeVisible();
    await expect(page.getByTestId("channels-room-title")).toBeVisible();

    await page.getByTestId("channels-room-title").click();
    await expect(page.getByTestId("rooms-drawer")).toBeVisible();
  });

  test("rooms drawer transitions to channels drawer after selecting a room", async ({ page }) => {
    // On phone, picking a room from the rooms drawer should land the
    // user in the channels drawer for the new room — not close all
    // drawers — so they can pick a channel without reopening the nav.
    await page.getByTestId("phone-rooms-toggle").click();
    await page.getByTestId("channels-room-title").click();
    const roomsDrawer = page.getByTestId("rooms-drawer");
    await roomsDrawer.getByTestId("rooms-search").fill("design-crit");
    await roomsDrawer.getByTestId("rooms-search").press("Enter");

    await expect(page).toHaveURL(/#design-crit$/);
    // Rooms drawer slides off-left, channels drawer slides on for the new room.
    await expect(roomsDrawer).toHaveCSS("transform", /matrix.*-/);
    await expect(page.getByTestId("channels-drawer")).toHaveCSS(
      "transform",
      /matrix\(1, 0, 0, 1, 0, 0\)/,
    );
  });

  test("phone exposes theme controls via the settings sheet (and not as a fixed pill)", async ({
    page,
  }) => {
    // Theme controls live in the settings sheet, opened from the
    // rooms-rail "you" row. The standalone phone-theme-toggle row
    // (under the rooms list) was removed once the settings sheet
    // landed — it was a redundant second entry point to the same
    // preference.
    await page.getByTestId("phone-rooms-toggle").click();
    await page.getByTestId("channels-room-title").click();
    await page.getByTestId("you-row").click();
    await expect(page.getByTestId("settings-sheet")).toBeVisible();
    await expect(page.getByTestId("settings-theme-system")).toBeVisible();
    await expect(page.getByTestId("settings-theme-light")).toBeVisible();
    await expect(page.getByTestId("settings-theme-dark")).toBeVisible();
    // Desktop fixed-pill toggle isn't rendered on phone.
    expect(await page.getByTestId("theme-toggle").count()).toBe(0);
    // The deprecated standalone phone-theme-toggle row is gone.
    expect(await page.getByTestId("phone-theme-toggle").count()).toBe(0);
  });
});

test.describe("phone — details sheet", () => {
  test.beforeEach(async ({ page }, testInfo) => {
    test.skip(testInfo.project.name !== "mobile-chrome", "phone-only test");
    await page.goto("/");
    await page.evaluate(() => { try { localStorage.clear(); } catch {} });
    await page.goto("/#dusk-collective");
    await expect(page.getByTestId("phone-header")).toBeVisible();
  });

  // Skipped: depends on fixture messages being rendered into the chat column.
  // Since Plan E, messages come from the live engine only, so the msg-row
  // with "routing thru ravi" does not exist on a fresh page load. Unblock
  // once fixtures are merged back or messages carry HasDetails from the engine.
  test.skip("info button on a delivered message opens the details bottom sheet", async ({
    page,
  }) => {
    const row = page.locator(".msg-row", { hasText: "routing thru ravi" });
    await row.getByRole("button", { name: /Message details/i }).click();
    const sheet = page.getByTestId("details-sheet");
    await expect(sheet).toBeVisible();
    await expect(sheet.getByText(/8f3c…a2/)).toBeVisible();
  });
});

test.describe("phone — reaction picker", () => {
  test.beforeEach(async ({ page }, testInfo) => {
    test.skip(testInfo.project.name !== "mobile-chrome", "phone-only test");
    await page.goto("/");
    await page.evaluate(() => { try { localStorage.clear(); } catch {} });
    await page.goto("/#dusk-collective");
    await expect(page.getByTestId("phone-header")).toBeVisible();
  });

  // Skipped: depends on msg-row elements being present in the chat column.
  // Since Plan E, messages come from the live engine only, so no .msg-row
  // exists on a fresh page load (the engine isn't connected in tests).
  // Unblock once fixtures are merged back or the test seeds a message first.
  test.skip("react button opens the picker as a bottom sheet", async ({ page }) => {
    const row = page.locator(".msg-row").first();
    // Tap the React action — actions are always-visible on touch (Task 18).
    await row.getByRole("button", { name: /^React$/ }).click();
    const sheet = page.getByTestId("reaction-sheet");
    await expect(sheet).toBeVisible();
    // Click an emoji and confirm sheet closes.
    await sheet.getByRole("button", { name: /🔥/ }).click();
    await expect(sheet).not.toBeVisible();
  });
});
