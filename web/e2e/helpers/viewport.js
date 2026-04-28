// Test helpers for viewport-aware actions. On the mobile-chrome
// project, columns live behind drawers — open them before clicking
// elements inside. On desktop the helpers are no-ops.

export function isMobile(testInfo) {
  return testInfo.project.name === "mobile-chrome";
}

export async function openChannelsDrawer(page, testInfo) {
  if (!isMobile(testInfo)) return;
  await page.getByTestId("phone-rooms-toggle").click();
  // Wait for the drawer to finish its 220ms transition.
  await page.waitForTimeout(260);
}

export async function openRoomsDrawer(page, testInfo) {
  if (!isMobile(testInfo)) return;
  await openChannelsDrawer(page, testInfo);
  // Tap the room title inside the channels drawer to swap to rooms.
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
  await page.getByTestId("drawer-backdrop").click();
  await page.waitForTimeout(260);
}
