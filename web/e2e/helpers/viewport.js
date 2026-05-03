// Test helpers for viewport-aware actions. On the mobile-chrome
// project, columns live behind drawers — open them before clicking
// elements inside. On desktop the helpers are no-ops.

export function isMobile(testInfo) {
  return testInfo.project.name === "mobile-chrome";
}

// Wait until every drawer's CSS transform has reached its target —
// either `matrix(1, 0, 0, 1, 0, 0)` (translateX(0), open) or fully
// off-screen (closed). The drawer's `aria-hidden` attribute carries
// the *intended* state; the transform lags by up to 220 ms while the
// `transition: transform 220ms ease` rule plays out.
//
// The previous helper hard-coded a 260 ms `waitForTimeout` after each
// open/close — fine in isolation, fragile under parallel load: under
// load the slide can run longer than 260 ms and the next click lands
// while the just-closed drawer's content is still on top of its target,
// producing a "subtree intercepts pointer events" or "element outside
// of viewport" flake.
//
// We poll the computed transform per frame and wait until every drawer
// is at its target. Lustre's render runs in a `requestAnimationFrame`
// callback, so a single pre-poll yield (await rAF) is enough to let
// any pending state-change re-render apply the new style attribute
// before we begin polling.
async function waitDrawersSettled(page) {
  await page.evaluate(async () => {
    const ids = ["channels-drawer", "rooms-drawer", "members-drawer"];
    const els = ids
      .map((id) => document.querySelector(`[data-testid="${id}"]`))
      .filter(Boolean);
    if (!els.length) return;

    const isSettled = (el) => {
      const t = getComputedStyle(el).transform;
      const open = el.getAttribute("aria-hidden") === "false";
      if (open) {
        // Identity transform = translateX(0). Some browsers report
        // "none" when no transform is set; either is acceptable.
        return t === "matrix(1, 0, 0, 1, 0, 0)" || t === "none";
      }
      // Closed: drawer is `position: fixed; transform: translateX(±100%)`.
      // Once the transition has finished, the matrix's tx component is
      // ±drawer-width (for left-side drawers it goes very negative,
      // for right-side very positive). Anything within a few pixels of
      // 0 means the slide is still in progress.
      const m = t.match(/^matrix\(1, 0, 0, 1, (-?\d+(?:\.\d+)?), 0\)$/);
      if (!m) return true; // unrecognised transform — assume settled
      return Math.abs(parseFloat(m[1])) >= 50;
    };

    // Yield once so any pending Lustre render commits its new style
    // attribute before we begin checking.
    await new Promise((r) => requestAnimationFrame(r));

    const deadline = performance.now() + 2000;
    while (performance.now() < deadline) {
      if (els.every(isSettled)) return;
      await new Promise((r) => requestAnimationFrame(r));
    }
    // Best-effort: if some drawer never settles within 2 s, return
    // anyway. The downstream Playwright actionability check will
    // surface a clearer failure than a generic timeout from here.
  });
}

// Read which drawer is currently rendered as open. Returns one of
// "channels", "rooms", "members", or null. Probes `aria-hidden`
// (set synchronously when the drawer state flips) rather than the
// transform, which is mid-transition for ~220 ms after a state change.
async function activeDrawer(page) {
  for (const id of ["channels-drawer", "rooms-drawer", "members-drawer"]) {
    const open = await page
      .getByTestId(id)
      .evaluate((el) => el.getAttribute("aria-hidden") === "false")
      .catch(() => false);
    if (open) return id.replace("-drawer", "");
  }
  return null;
}

export async function openChannelsDrawer(page, testInfo) {
  if (!isMobile(testInfo)) return;
  await waitDrawersSettled(page);
  const open = await activeDrawer(page);
  if (open === "channels") return;
  // The phone-rooms-toggle lives in the header (z-index 10). When a
  // drawer is open (z-index 30) it covers the header — close it first.
  if (open !== null) {
    await page.getByTestId("drawer-backdrop").first().click({ force: true });
    await waitDrawersSettled(page);
  }
  await page.getByTestId("phone-rooms-toggle").click();
  await waitDrawersSettled(page);
}

export async function openRoomsDrawer(page, testInfo) {
  if (!isMobile(testInfo)) return;
  await waitDrawersSettled(page);
  const open = await activeDrawer(page);
  if (open === "rooms") return;
  // From channels we can swap directly via the room title; from any
  // other state, open channels first.
  if (open !== "channels") {
    await openChannelsDrawer(page, testInfo);
  }
  await page.getByTestId("channels-room-title").click();
  await waitDrawersSettled(page);
}

export async function openMembersDrawer(page, testInfo) {
  if (!isMobile(testInfo)) return;
  await waitDrawersSettled(page);
  await page.getByTestId("phone-members-toggle").click();
  await waitDrawersSettled(page);
}

export async function closeDrawer(page, testInfo) {
  if (!isMobile(testInfo)) return;
  // Multiple drawer-backdrop elements exist (one per drawer); click the first
  // with force:true because the drawer content can intercept pointer events.
  await page.getByTestId("drawer-backdrop").first().click({ force: true });
  await waitDrawersSettled(page);
}
