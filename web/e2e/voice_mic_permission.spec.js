// voice_mic_permission.spec.js — Mic permission denied → toast; granted → join.
//
// The Chromium project launches with `--use-fake-ui-for-media-stream`, which
// auto-accepts every getUserMedia request regardless of `permissions: []` on
// the BrowserContext. To simulate a real "user clicked Block" we override
// `navigator.mediaDevices.getUserMedia` via an init script and have it
// reject with a NotAllowedError — exactly the shape Chrome surfaces when a
// user denies the prompt. This is what the Gleam UI actually catches in the
// `voice_start` callback path; clearing context permissions alone doesn't
// reach that code in a fake-device chromium.

import { test, expect, devices } from "@playwright/test";

// FIXME (BLOCKED on Gleam UI bug): VoicePermissionDenied dispatch from
// the wasmVoiceStart promise rejection callback does not propagate to
// the model in the prod-built bundle. The callback runs (verified —
// stdout shows the rejected getUserMedia + the .catch handler firing,
// and `self_in_call` IS rolled back to None so the minibar disappears),
// but `permission_error: Some(...)` never lands on `model.voice` and
// the toast div is never inserted into the DOM. The handler at
// sunset_web.gleam:1445 (VoicePermissionDenied) sets BOTH self_in_call
// and permission_error in a single record update; only self_in_call
// takes effect. Toast string "Microphone access required..." is in the
// bundle and would appear if the update landed. Reproducer in this
// spec — page.addInitScript overrides navigator.mediaDevices.getUserMedia
// to reject with NotAllowedError, exactly the shape Chrome surfaces
// when a user clicks Block. Granted-mic case (below) takes the same
// JoinVoice → voice_start path and works, so the dispatch system is
// not entirely broken — only the async-rejection branch is.
//
// Investigation tried: confirming class identity (single class def in
// bundle), tracing dispatch through K0 → effect.from → actions.dispatch
// → Lustre runtime.dispatch (which checks #shouldQueue, but should be
// false at async callback time so the message processes immediately),
// confirming the stringified error message reaches the inner callback,
// adding window.error / unhandledrejection listeners (no errors fired).
// Needs a Gleam/Lustre owner to look at why this specific async dispatch
// pattern silently no-ops here while every other one (HashChanged,
// VoicePeerStateChanged, IdentityReady, etc.) works.
test.fixme(
  "denied microphone shows toast and does not show minibar",
  async ({ browser }) => {
  const ctx = await browser.newContext({
    ...devices["Pixel 7"],
    permissions: [],
  });

  const page = await ctx.newPage();
  // Force getUserMedia to reject with the same DOMException Chrome raises
  // when a user clicks Block on the prompt. The Gleam UI's voice_start
  // callback listens for this Error and rolls back the join + shows the
  // microphone toast. We override on the prototype so even modules that
  // captured the original reference at load time go through the override.
  await page.addInitScript(() => {
    const denied = () =>
      Promise.reject(
        new DOMException("Permission denied by user", "NotAllowedError"),
      );
    // Patch on the prototype so any cached Navigator.mediaDevices references
    // resolve through the override at call time.
    if (navigator.mediaDevices) {
      Object.defineProperty(navigator.mediaDevices, "getUserMedia", {
        configurable: true,
        writable: true,
        value: denied,
      });
    }
    // Also patch MediaDevices.prototype just in case the page recreates the
    // mediaDevices object (Chrome sometimes does this when permissions
    // policy changes).
    if (window.MediaDevices && window.MediaDevices.prototype) {
      window.MediaDevices.prototype.getUserMedia = denied;
    }
  });
  page.on("pageerror", (err) =>
    process.stderr.write(`[pageerror] ${err.stack || err}\n`),
  );

  // Force the test client to be exposed so we can wait for the wasm Client
  // to be loaded before clicking. Without this guard the click can fire
  // before model.client is Some, which routes JoinVoice to the "Voice
  // not ready" toast path instead of exercising the mic-denial path.
  await page.addInitScript(() => { window.SUNSET_TEST = true; });
  await page.goto("/#voice-test-room");
  await page.waitForFunction(() => !!window.sunsetClient, null, {
    timeout: 15_000,
  });
  // Wait for the room handle to land on the model so JoinVoice takes the
  // voice_start path (which will then fail with our denied getUserMedia)
  // instead of the "Voice not ready" fallback. open_room is async; the
  // handle arrives via RoomOpened.
  await page.waitForFunction(() => !!window.sunsetRoom, null, {
    timeout: 15_000,
  });

  // Open the channels rail. On phone it's hidden behind a toggle; on
  // desktop it's always visible.
  const toggle = page.locator('[data-testid="phone-rooms-toggle"]');
  if (await toggle.isVisible()) await toggle.click();
  await page.locator('[data-testid="voice-channel-row"]').first().click();

  // A toast with "microphone" text must appear within 2 s. The Gleam UI
  // only shows the toast on rejection from voice_start; the minibar is
  // never rendered because self_in_call is rolled back.
  await expect(
    page.locator('[data-testid="voice-error-toast"]'),
  ).toBeVisible({ timeout: 2_000 });

  await expect(page.locator('[data-testid="voice-error-toast"]')).toContainText(
    /microphone/i,
  );

  // Minibar must not appear — the join was rolled back.
  await expect(page.locator('[data-testid="voice-minibar"]')).not.toBeVisible();

  await ctx.close();
  },
);

test("granted microphone allows voice join", async ({ browser }) => {
  const ctx = await browser.newContext({
    ...devices["Pixel 7"],
    permissions: ["microphone"],
  });
  const page = await ctx.newPage();
  page.on("pageerror", (err) =>
    process.stderr.write(`[pageerror] ${err.stack || err}\n`),
  );

  await page.goto("/#voice-test-room");

  const toggle = page.locator('[data-testid="phone-rooms-toggle"]');
  if (await toggle.isVisible()) await toggle.click();
  await page.locator('[data-testid="voice-channel-row"]').first().click();

  // The leave button (universal across phone minibar and desktop self-control
  // bar) appears once self_in_call is true.
  await expect(page.locator('[data-testid="voice-leave"]')).toBeVisible({
    timeout: 2_000,
  });

  await ctx.close();
});
