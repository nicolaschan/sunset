// Test helpers for viewport-aware actions. On the mobile-chrome
// project, columns live behind drawers — open them before clicking
// elements inside. On desktop the helpers are no-ops.

export function isMobile(testInfo) {
  return testInfo.project.name === "mobile-chrome";
}

// Read which drawer is currently rendered as open. Returns one of
// "channels", "rooms", "members", or null. We probe `aria-hidden`
// (set synchronously by the Lustre render based on model.drawer)
// rather than `transform` CSS, which races with the 220ms slide
// transition: mid-transition transforms like `matrix(1,0,0,1,-130,0)`
// contain a "-" and would falsely report the drawer as closed.
// aria-hidden reflects model intent and stabilises the moment the
// drawer state changes, regardless of the visual transition state.
async function activeDrawer(page) {
  for (const id of ["channels-drawer", "rooms-drawer", "members-drawer"]) {
    const hidden = await page
      .getByTestId(id)
      .getAttribute("aria-hidden")
      .catch(() => null);
    if (hidden === "false") {
      return id.replace("-drawer", "");
    }
  }
  return null;
}

export async function openChannelsDrawer(page, testInfo) {
  if (!isMobile(testInfo)) return;
  const open = await activeDrawer(page);
  if (open === "channels") return;
  // The phone-rooms-toggle lives in the header (z-index 10). When a
  // drawer is open (z-index 30) it covers the header — close it first.
  if (open !== null) {
    await page.getByTestId("drawer-backdrop").first().click({ force: true });
    await page.waitForTimeout(260);
  }
  await page.getByTestId("phone-rooms-toggle").click();
  // Wait for the drawer to finish its 220ms transition.
  await page.waitForTimeout(260);
}

export async function openRoomsDrawer(page, testInfo) {
  if (!isMobile(testInfo)) return;
  const open = await activeDrawer(page);
  if (open === "rooms") return;
  // From channels we can swap directly via the room title; from any
  // other state, open channels first.
  if (open !== "channels") {
    await openChannelsDrawer(page, testInfo);
  }
  await page.getByTestId("channels-room-title").click();
  await page.waitForTimeout(260);
}

export async function openMembersDrawer(page, testInfo) {
  if (!isMobile(testInfo)) return;
  await page.getByTestId("phone-members-toggle").click();
  await page.waitForTimeout(260);
}

export async function closeDrawer(page, testInfo) {
  if (!isMobile(testInfo)) return;
  // Multiple drawer-backdrop elements exist (one per drawer); click the first
  // with force:true because the drawer content can intercept pointer events.
  await page.getByTestId("drawer-backdrop").first().click({ force: true });
  await page.waitForTimeout(260);
}
