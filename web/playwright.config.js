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

// Per-project `testIgnore` overrides the top-level `testIgnore` (it
// does not merge). That means any project that wants to add its own
// ignore patterns must also re-include the top-level ones, or the
// non-voice runner (SUNSET_TEST_HOOKS=0) will start picking up
// voice_*.spec.js. We build per-project ignore lists by prepending
// the top-level pattern.
function ignoreFor(...extra) {
  const base = testHooks ? [] : [/voice_.*\.spec\.js$/];
  // The sample-rate regression reproduces only on Firefox (where the audio
  // device runs at a non-48 kHz rate); it runs solely under the `firefox`
  // project, so every Chromium project ignores it.
  return [...base, /voice_samplerate_firefox\.spec\.js$/, ...extra];
}

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
      // *_real_mic.spec.js depends on the deterministic sweep.wav
      // reference signal that only the chromium-real-mic project
      // wires up (see below). Running it under the default
      // fake-audio device would assert tone purity on a signal that
      // wasn't designed to satisfy it — a spurious failure shape
      // that says nothing about the code under test.
      testIgnore: ignoreFor(/_real_mic\.spec\.js$/),
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
      testIgnore: ignoreFor(/_real_mic\.spec\.js$/),
    },
    // Chromium project with a fake WAV file piped as the mic input.
    // Used exclusively by voice_real_mic.spec.js (testMatch below).
    //
    // sweep.wav lives inside the Nix-built dist (`webVoiceUiTestDist`
    // copies it into `$out/audio/test-fixtures/sweep.wav` via the flake
    // — see flake.nix). The previous resolve to `web/audio/test-fixtures`
    // (the source tree) never existed on disk, so Chromium silently
    // fell back to its built-in fake audio generator and tests that
    // depended on a known 440 Hz reference signal didn't actually
    // observe one.
    {
      name: "chromium-real-mic",
      use: {
        ...devices["Desktop Chrome"],
        permissions: ["microphone"],
        launchOptions: {
          args: [
            "--use-fake-device-for-media-stream",
            "--use-fake-ui-for-media-stream",
            `--use-file-for-fake-audio-capture=${resolve(dist, "audio/test-fixtures/sweep.wav")}`,
          ],
        },
      },
      // The real-mic project pipes a deterministic 440 Hz sine into
      // getUserMedia, so any spec that needs a clean reference signal
      // for tone-purity / audio-quality assertions lives here. Match on
      // the `_real_mic` suffix so new specs slot in without touching
      // the config.
      testMatch: /voice_.*_real_mic\.spec\.js|voice_real_mic\.spec\.js/,
    },
    // Firefox project, scoped to the sample-rate regression spec. Firefox
    // delivers the mic at the audio device rate (44.1 kHz here) and refuses
    // createMediaStreamSource into a 48 kHz AudioContext — the exact bug
    // this guards. `firefoxUserPrefs` supply a fake mic and auto-grant
    // permission (the Chromium `--use-fake-*` flags don't apply to Firefox).
    {
      name: "firefox",
      use: {
        ...devices["Desktop Firefox"],
        launchOptions: {
          firefoxUserPrefs: {
            "media.navigator.streams.fake": true,
            "media.navigator.permission.disabled": true,
          },
        },
      },
      testMatch: /voice_samplerate_firefox\.spec\.js$/,
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
