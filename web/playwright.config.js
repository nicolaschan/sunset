// Playwright config — runs the e2e suite against the prod-built dist.
//
// `nix run .#web-test` (or in-CI equivalents) sets `SUNSET_WEB_DIST` to
// the Nix-built artefact path. We serve it on a fixed local port via
// `static-web-server` (provided by the dev shell / app wrapper) and
// point Playwright's `baseURL` at it.

import { defineConfig, devices } from "@playwright/test";
import { resolve, dirname } from "path";
import { fileURLToPath } from "url";

const __dirname = dirname(fileURLToPath(import.meta.url));

const dist = process.env.SUNSET_WEB_DIST;
if (!dist) {
  throw new Error(
    "SUNSET_WEB_DIST is unset. Run via `nix run .#web-test` or set it manually to the build output (e.g. `SUNSET_WEB_DIST=$(nix build .#web --no-link --print-out-paths)`).",
  );
}

const port = Number(process.env.SUNSET_WEB_PORT ?? 4173);
const testHooks = process.env.SUNSET_TEST_HOOKS === "1";

export default defineConfig({
  testDir: "e2e",
  // Voice runner only runs voice_*.spec.js; chat runner skips voice.
  // The voice runner uses webVoiceUiTestDist which lacks PWA assets (apple-touch-icon,
  // manifest) that shell.spec.js asserts on, so non-voice specs would fail spuriously.
  testMatch: testHooks ? [/voice_.*\.spec\.js$/] : undefined,
  testIgnore: testHooks ? [] : [/voice_.*\.spec\.js$/],
  fullyParallel: true,
  forbidOnly: !!process.env.CI,
  // No retries — flakes are bugs, not noise. A test that doesn't pass on
  // its first run is a regression we want surfaced, not papered over.
  retries: 0,
  workers: process.env.CI ? 1 : undefined,
  reporter: process.env.CI ? "github" : "list",
  outputDir: "test-results",
  use: {
    baseURL: `http://127.0.0.1:${port}`,
    // We never retry, so capture trace + screenshot whenever a test fails
    // — that's the only forensic data we'll have.
    trace: "retain-on-failure",
    screenshot: "only-on-failure",
  },
  projects: [
    {
      name: "chromium",
      use: {
        ...devices["Desktop Chrome"],
        // Voice tests require microphone access.
        permissions: ["microphone"],
        // Provide a fake audio device so getUserMedia succeeds in headless
        // environments without a real microphone.
        launchOptions: {
          args: [
            "--use-fake-device-for-media-stream",
            "--use-fake-ui-for-media-stream",
          ],
        },
      },
    },
    {
      name: "mobile-chrome",
      use: {
        ...devices["Pixel 7"],
        permissions: ["microphone"],
        launchOptions: {
          args: [
            "--use-fake-device-for-media-stream",
            "--use-fake-ui-for-media-stream",
          ],
        },
      },
    },
    // Chromium project with a fake WAV file piped as the mic input.
    // Used exclusively by voice_real_mic.spec.js (testMatch below).
    {
      name: "chromium-real-mic",
      use: {
        ...devices["Desktop Chrome"],
        permissions: ["microphone"],
        launchOptions: {
          args: [
            "--use-fake-device-for-media-stream",
            "--use-fake-ui-for-media-stream",
            `--use-file-for-fake-audio-capture=${resolve(__dirname, "audio/test-fixtures/sweep.wav")}`,
          ],
        },
      },
      testMatch: /voice_real_mic\.spec\.js/,
    },
  ],
  webServer: {
    // static-web-server serves the dist directory; --port is bound on
    // 127.0.0.1 only so playwright's connectivity probe matches.
    command: `static-web-server --root "${dist}" --port ${port} --host 127.0.0.1`,
    port,
    reuseExistingServer: !process.env.CI,
    timeout: 30_000,
  },
});
