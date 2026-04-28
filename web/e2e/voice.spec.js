// Voice-popover e2e: clicking an in-call member opens the popover, the
// volume slider + denoise toggle update state, and the close button
// dismisses the popover. The 'you' row hides the mute-for-me / reset
// footer, since muting yourself locally doesn't make sense.

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

test("clicking a voice member opens the popover", async ({ page }) => {
  await expect(page.getByTestId("voice-popover")).toHaveCount(0);

  await page
    .locator('[data-testid="voice-member"][data-voice-name="ravi"]')
    .click();

  const popover = page.getByTestId("voice-popover");
  await expect(popover).toBeVisible();
  await expect(popover.getByText("ravi", { exact: true })).toBeVisible();
  // Non-self peers get the mute-for-me + reset footer.
  await expect(page.getByTestId("voice-popover-deafen")).toBeVisible();
  await expect(page.getByTestId("voice-popover-reset")).toBeVisible();
});

test("close button dismisses the popover", async ({ page }) => {
  await page
    .locator('[data-testid="voice-member"][data-voice-name="ravi"]')
    .click();
  await expect(page.getByTestId("voice-popover")).toBeVisible();

  await page.getByTestId("voice-popover-close").click();
  await expect(page.getByTestId("voice-popover")).toHaveCount(0);
});

test("volume slider updates the displayed percentage", async ({ page }) => {
  await page
    .locator('[data-testid="voice-member"][data-voice-name="ravi"]')
    .click();

  const popover = page.getByTestId("voice-popover");
  // Default volume is 100% on first open.
  await expect(popover.getByText("100%")).toBeVisible();

  const slider = page.getByTestId("voice-popover-volume");
  // Setting an input[type=range] with .fill() doesn't fire 'input' on
  // every browser — drive it via evaluate + native events instead.
  await slider.evaluate((el) => {
    el.value = "150";
    el.dispatchEvent(new Event("input", { bubbles: true }));
  });

  await expect(popover.getByText("150%")).toBeVisible();
});

test("non-self volume slider goes up to 200%, self caps at 100%", async ({
  page,
}) => {
  // Other peer: 0–200 range.
  await page
    .locator('[data-testid="voice-member"][data-voice-name="ravi"]')
    .click();
  const otherSlider = page.getByTestId("voice-popover-volume");
  await expect(otherSlider).toHaveAttribute("max", "200");
  await page.getByTestId("voice-popover-close").click();

  // Self: 0–100 range.
  await page
    .locator('[data-testid="voice-member"][data-voice-name="you"]')
    .click();
  const selfSlider = page.getByTestId("voice-popover-volume");
  await expect(selfSlider).toHaveAttribute("max", "100");
});

test("denoise toggle flips aria-pressed state", async ({ page }) => {
  await page
    .locator('[data-testid="voice-member"][data-voice-name="ravi"]')
    .click();

  const toggle = page.getByTestId("voice-popover-denoise");
  // Seeded ON.
  await expect(toggle).toHaveAttribute("aria-pressed", "true");
  await toggle.click();
  await expect(toggle).toHaveAttribute("aria-pressed", "false");
  await toggle.click();
  await expect(toggle).toHaveAttribute("aria-pressed", "true");
});

test("self row hides mute-for-me + reset footer", async ({ page }) => {
  await page
    .locator('[data-testid="voice-member"][data-voice-name="you"]')
    .click();

  await expect(page.getByTestId("voice-popover")).toBeVisible();
  await expect(page.getByTestId("voice-popover-deafen")).toHaveCount(0);
  await expect(page.getByTestId("voice-popover-reset")).toHaveCount(0);
});

test("reset restores defaults after edits", async ({ page }) => {
  await page
    .locator('[data-testid="voice-member"][data-voice-name="ravi"]')
    .click();

  const popover = page.getByTestId("voice-popover");
  const slider = page.getByTestId("voice-popover-volume");
  const denoise = page.getByTestId("voice-popover-denoise");

  await slider.evaluate((el) => {
    el.value = "30";
    el.dispatchEvent(new Event("input", { bubbles: true }));
  });
  await denoise.click();
  await expect(popover.getByText("30%")).toBeVisible();
  await expect(denoise).toHaveAttribute("aria-pressed", "false");

  await page.getByTestId("voice-popover-reset").click();
  await expect(popover.getByText("100%")).toBeVisible();
  await expect(denoise).toHaveAttribute("aria-pressed", "true");
});

test.describe("phone — voice sheet", () => {
  test.beforeEach(async ({ page }, testInfo) => {
    test.skip(testInfo.project.name !== "mobile-chrome", "phone-only test");
    await page.goto("/");
    await page.evaluate(() => { try { localStorage.clear(); } catch {} });
    await page.goto("/#dusk-collective");
    await expect(page.getByTestId("phone-header")).toBeVisible();
  });

  test("tapping an in-call member opens the voice bottom sheet", async ({
    page,
  }) => {
    await page.getByTestId("phone-rooms-toggle").click();
    // The voice channel block lives in the channels drawer.
    const channelsDrawer = page.getByTestId("channels-drawer");
    await channelsDrawer
      .locator('[data-testid="voice-member"][data-voice-name="ravi"]')
      .click();

    const sheet = page.getByTestId("voice-sheet");
    await expect(sheet).toBeVisible();
    await expect(sheet.getByText("ravi", { exact: true })).toBeVisible();
    await expect(sheet.getByTestId("voice-popover-volume")).toBeVisible();
  });
});
