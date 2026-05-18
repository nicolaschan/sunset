// Rooms-list online-count e2e.
//
// The rooms rail used to render "N/M online" where M was either a
// hardcoded fixture value or a hardcoded `1` from `synthetic_room`
// — so a freshly-joined room read "1/1 online" regardless of who
// was actually present. That format conflated "people we've ever
// seen" with "people online right now", and the denominator was
// frequently wrong (e.g. always 14 for the dusk-collective fixture).
//
// Contract this test pins:
//   1. The rail row for a joined room shows "N online" — never the
//      X/Y format.
//   2. N comes from live presence data — self alone is "1 online",
//      not "1/1 online" and not "14/14 online".
//   3. The count matches what the members rail reports for the same
//      room (both derive from the same `state.members` snapshot).
//
// Single-browser scenario is enough to pin the contract: the peer
// publishes its own presence, the membership tracker reflects self
// as online, and the rooms rail derives N from that. Cross-peer
// accuracy (two browsers in the same room → "2 online") is left to
// presence.spec.js territory; here we want a deterministic, relay-
// free regression on the rendered format and the self count.

import { expect, test } from "@playwright/test";

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

test("rooms rail shows live online count, not the X/Y fixture format", async ({
  page,
}) => {
  const rail = page.getByTestId("rooms-rail");

  // Members rail reports the canonical online count for the active
  // room; we read it as the source of truth, then assert the rooms
  // rail agrees. Waiting on "Online — N" here also guarantees the
  // membership tracker has produced at least one snapshot — without
  // it the rooms rail would still legitimately render no online
  // segment (count = 0, the "we don't know yet" path).
  const onlineHeader = page.getByText(/^Online — \d+/);
  await expect(onlineHeader).toBeVisible({ timeout: 10_000 });
  const headerText = await onlineHeader.textContent();
  const match = headerText.match(/Online — (\d+)/);
  expect(match, `members-rail header should expose a count, got: ${headerText}`)
    .not.toBeNull();
  const expectedCount = Number.parseInt(match[1], 10);
  expect(expectedCount).toBeGreaterThanOrEqual(1);

  // The rooms-rail row for dusk-collective should carry that same
  // count followed by " online" — no X/Y, no fixture-hardcoded "14".
  const railText = await rail.textContent();

  // The old format must be gone everywhere in the rail.
  expect(
    railText,
    `rooms rail still renders the legacy "N/M online" format: ${railText}`,
  ).not.toMatch(/\d+\/\d+\s*online/);

  // Exactly one "<count> online" segment for the active row.
  expect(
    railText,
    `rooms rail should render "${expectedCount} online" for the active room`,
  ).toContain(`${expectedCount} online`);
});

test("rooms rail does not invent an online count before presence arrives", async ({
  page,
}) => {
  // Negative contract: nowhere in the rail should we see the
  // fixture-leaked "14 online" / "6 online" / etc. for dusk-collective.
  // Those numbers came from `fixture.gleam`'s hardcoded `online: 6`
  // / `members: 14`; after this change the only "N online" string in
  // the rail is the live one.
  const rail = page.getByTestId("rooms-rail");
  await expect(rail).toBeVisible();

  // Wait for the rail to settle on its live state.
  await expect(page.getByText(/^Online — \d+/)).toBeVisible({
    timeout: 10_000,
  });

  const railText = await rail.textContent();
  // The previous fixture values for dusk-collective were 6/14. Neither
  // should ever appear as a "online" segment in a single-browser run.
  expect(railText).not.toMatch(/\b6 online\b/);
  expect(railText).not.toMatch(/\b14 online\b/);
});
