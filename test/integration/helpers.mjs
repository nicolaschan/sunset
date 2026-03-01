// helpers.mjs — Puppeteer helpers for Sunset integration tests.

import puppeteer from "puppeteer";
import { execSync } from "node:child_process";
import path from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const TEST_TONE_PATH = path.join(__dirname, "test-tone.wav");

// ── Browser lifecycle ──────────────────────────────────────────────

/**
 * Find a Chrome/Chromium executable.
 * Checks PUPPETEER_EXECUTABLE_PATH env var first, then prefers
 * system-installed chromium (works on NixOS), falls back to
 * Puppeteer's bundled Chrome.
 */
function findChromePath() {
  if (process.env.PUPPETEER_EXECUTABLE_PATH) {
    return process.env.PUPPETEER_EXECUTABLE_PATH;
  }
  for (const name of ["chromium", "google-chrome", "google-chrome-stable"]) {
    try {
      const path = execSync(`which ${name}`, { encoding: "utf-8" }).trim();
      if (path) return path;
    } catch {
      // not found, try next
    }
  }
  return undefined; // let Puppeteer use its bundled Chrome
}

/**
 * Launch a headless browser with WebRTC-friendly flags.
 * Returns a Puppeteer Browser instance.
 */
export async function launchBrowser() {
  const executablePath = findChromePath();
  console.log(`[browser] Using: ${executablePath || "puppeteer bundled chrome"}`);
  return puppeteer.launch({
    headless: true,
    executablePath,
    args: [
      "--no-sandbox",
      "--disable-setuid-sandbox",
      "--disable-gpu",
      "--disable-dev-shm-usage",
      "--use-fake-ui-for-media-stream",
      "--use-fake-device-for-media-stream",
      `--use-file-for-fake-audio-capture=${TEST_TONE_PATH}`,
      "--disable-web-security",
      // Allow WebRTC on localhost
      "--allow-running-insecure-content",
    ],
  });
}

/**
 * Open the Sunset app in a new page, pointing at the local relay.
 * @param {import("puppeteer").Browser} browser
 * @param {string} appUrl   - e.g. "http://127.0.0.1:3000"
 * @param {string} relayMultiaddr - multiaddr of the local relay
 * @returns {Promise<import("puppeteer").Page>}
 */
export async function openApp(browser, appUrl, relayMultiaddr) {
  const page = await browser.newPage();
  page.on("console", (msg) => {
    const type = msg.type();
    if (type === "error" || type === "warning") {
      console.log(`[browser ${type}] ${msg.text()}`);
    }
  });
  page.on("pageerror", (err) => {
    console.log(`[browser pageerror] ${err.message}`);
  });

  const url = `${appUrl}/?relay=${encodeURIComponent(relayMultiaddr)}`;
  await page.goto(url, { waitUntil: "networkidle0", timeout: 30_000 });
  return page;
}

// ── Interaction helpers ────────────────────────────────────────────

/**
 * Join a room by typing the name and clicking Join.
 */
export async function joinRoom(page, roomName) {
  await page.waitForSelector(".room-input", { timeout: 10_000 });
  await page.type(".room-input", roomName);
  await page.click(".room-button");
  // Wait for the room view to appear
  await page.waitForSelector(".room-main", { timeout: 15_000 });
}

/**
 * Send a chat message by typing into the input and pressing Enter.
 */
export async function sendMessage(page, text) {
  await page.waitForSelector(".room-chat-input", { timeout: 10_000 });
  await page.type(".room-chat-input", text);
  await page.click(".room-send-button");
}

/**
 * Wait until a chat message containing `text` appears in the message list.
 * Returns the matching element's full text content.
 */
export async function waitForMessage(page, text, timeout = 60_000) {
  await page.waitForFunction(
    (searchText) => {
      const msgs = document.querySelectorAll(".room-msg-body");
      return [...msgs].some((el) => el.textContent.includes(searchText));
    },
    { timeout },
    text
  );
}

/**
 * Wait for peers to show up in the Connected section.
 * @param {number} count - expected number of peers
 */
export async function waitForPeers(page, count, timeout = 60_000) {
  await page.waitForFunction(
    (expectedCount) => {
      const header = document.querySelector(".room-peers-title");
      if (!header) return false;
      const match = header.textContent.match(/Connected \((\d+)\)/);
      return match && parseInt(match[1], 10) >= expectedCount;
    },
    { timeout },
    count
  );
}

/**
 * Wait for the relay to be connected (status text shows "Connected").
 */
export async function waitForRelayConnected(page, timeout = 60_000) {
  await page.waitForFunction(
    () => {
      const el = document.querySelector(".room-status-text");
      return el && el.textContent.includes("Connected");
    },
    { timeout }
  );
}

/**
 * Get all chat messages as an array of { sender, body } objects.
 */
export async function getMessages(page) {
  return page.evaluate(() => {
    const msgs = document.querySelectorAll(".room-msg");
    return [...msgs].map((msg) => ({
      sender:
        msg.querySelector(".room-msg-sender")?.textContent || "You",
      body: msg.querySelector(".room-msg-body")?.textContent || "",
      isSelf: msg.classList.contains("room-msg-self"),
      isSystem: msg.classList.contains("room-msg-system"),
    }));
  });
}

/**
 * Set the display name via the name editor UI.
 */
export async function setDisplayName(page, name) {
  // Click the edit name button
  await page.click(".room-name-edit-btn");
  await page.waitForSelector(".room-name-input", { timeout: 5_000 });
  // Clear and type
  await page.click(".room-name-input", { clickCount: 3 });
  await page.type(".room-name-input", name);
  await page.click(".room-name-save-btn");
}

/**
 * Get the count of connected peers from the sidebar header.
 */
export async function getConnectedPeerCount(page) {
  return page.evaluate(() => {
    const header = document.querySelector(".room-peers-title");
    if (!header) return 0;
    const match = header.textContent.match(/Connected \((\d+)\)/);
    return match ? parseInt(match[1], 10) : 0;
  });
}

// ── Audio helpers ──────────────────────────────────────────────────

/**
 * Click the "Join audio" button to join the audio session.
 */
export async function joinAudio(page) {
  await page.waitForSelector(".room-voice-btn", { timeout: 10_000 });
  await page.click(".room-voice-btn");
  // Wait for the button to change to "Leave audio" (active state)
  await page.waitForSelector(".room-voice-btn-active", { timeout: 15_000 });
}

/**
 * Click the "Leave audio" button to leave the audio session.
 */
export async function leaveAudio(page) {
  await page.waitForSelector(".room-voice-btn-active", { timeout: 10_000 });
  await page.click(".room-voice-btn-active");
  // Wait for the button to revert to non-active (Join audio)
  await page.waitForFunction(
    () => {
      const btn = document.querySelector(".room-voice-btn");
      return btn && !btn.classList.contains("room-voice-btn-active");
    },
    { timeout: 15_000 }
  );
}

/**
 * Wait until a peer's audio RTC peer connection dot shows "connected"
 * (green dot with class room-peer-dot-rtc-connected) for at least
 * `count` peers.
 */
export async function waitForAudioConnected(page, count, timeout = 60_000) {
  await page.waitForFunction(
    (expectedCount) => {
      const dots = document.querySelectorAll(".room-peer-dot-rtc-connected");
      return dots.length >= expectedCount;
    },
    { timeout },
    count
  );
}

/**
 * Check whether the page is receiving live remote audio.
 * Evaluates in the browser context using the hidden <audio> elements.
 */
export async function isReceivingAudio(page) {
  return page.evaluate(() => {
    const audios = document.querySelectorAll("audio");
    for (const audio of audios) {
      if (!audio.srcObject) continue;
      const tracks = audio.srcObject.getAudioTracks();
      if (tracks.some((t) => t.readyState === "live")) return true;
    }
    return false;
  });
}

/**
 * Wait until the page is receiving live remote audio from at least one peer.
 */
export async function waitForReceivingAudio(page, timeout = 60_000) {
  await page.waitForFunction(
    () => {
      const audios = document.querySelectorAll("audio");
      for (const audio of audios) {
        if (!audio.srcObject) continue;
        const tracks = audio.srcObject.getAudioTracks();
        if (tracks.some((t) => t.readyState === "live")) return true;
      }
      return false;
    },
    { timeout }
  );
}

/**
 * Wait until the page is NOT receiving any live remote audio.
 */
export async function waitForNotReceivingAudio(page, timeout = 30_000) {
  await page.waitForFunction(
    () => {
      const audios = document.querySelectorAll("audio");
      for (const audio of audios) {
        if (!audio.srcObject) continue;
        const tracks = audio.srcObject.getAudioTracks();
        if (tracks.some((t) => t.readyState === "live")) return false;
      }
      return true;
    },
    { timeout }
  );
}

/**
 * Get the number of active audio peer connections (hidden <audio>
 * elements with a live srcObject).
 */
export async function getActiveAudioStreamCount(page) {
  return page.evaluate(() => {
    let count = 0;
    const audios = document.querySelectorAll("audio");
    for (const audio of audios) {
      if (!audio.srcObject) continue;
      const tracks = audio.srcObject.getAudioTracks();
      if (tracks.some((t) => t.readyState === "live")) count++;
    }
    return count;
  });
}

/**
 * Wait until the page has exactly `count` active audio streams
 * (hidden <audio> elements with live tracks).
 */
export async function waitForActiveAudioStreams(page, count, timeout = 60_000) {
  await page.waitForFunction(
    (expectedCount) => {
      let actual = 0;
      const audios = document.querySelectorAll("audio");
      for (const audio of audios) {
        if (!audio.srcObject) continue;
        const tracks = audio.srcObject.getAudioTracks();
        if (tracks.some((t) => t.readyState === "live")) actual++;
      }
      return actual >= expectedCount;
    },
    { timeout },
    count
  );
}

/**
 * Check if the "Join audio" / "Leave audio" button shows joined state.
 */
export async function isAudioJoined(page) {
  return page.evaluate(() => {
    return !!document.querySelector(".room-voice-btn-active");
  });
}

// ── Non-silent audio detection ─────────────────────────────────────

/**
 * Count how many active audio streams have non-silent content.
 * Uses the Web Audio API AnalyserNode to measure RMS energy.
 */
export async function getNonSilentAudioCount(page) {
  return page.evaluate(async () => {
    const audios = document.querySelectorAll("audio");
    let count = 0;
    for (const audio of audios) {
      if (!audio.srcObject) continue;
      const tracks = audio.srcObject.getAudioTracks();
      if (!tracks.some((t) => t.readyState === "live")) continue;

      const ctx = new AudioContext();
      const source = ctx.createMediaStreamSource(audio.srcObject);
      const analyser = ctx.createAnalyser();
      analyser.fftSize = 2048;
      source.connect(analyser);

      // Wait for the analyser to process some audio frames
      await new Promise((r) => setTimeout(r, 200));

      const data = new Float32Array(analyser.fftSize);
      analyser.getFloatTimeDomainData(data);

      let sumSq = 0;
      for (let i = 0; i < data.length; i++) {
        sumSq += data[i] * data[i];
      }
      const rms = Math.sqrt(sumSq / data.length);

      source.disconnect();
      ctx.close();

      // Threshold: anything above -60 dBFS (~0.001 RMS) is non-silent
      if (rms > 0.001) count++;
    }
    return count;
  });
}

/**
 * Check whether any received audio stream has non-silent content.
 */
export async function isReceivingNonSilentAudio(page) {
  return (await getNonSilentAudioCount(page)) > 0;
}

/**
 * Wait until the page has at least `count` non-silent audio streams.
 * Polls periodically because the AnalyserNode needs time to accumulate data.
 * @param {number} count - minimum number of non-silent streams (default 1)
 */
export async function waitForNonSilentAudio(page, count = 1, timeout = 60_000) {
  const start = Date.now();
  while (Date.now() - start < timeout) {
    const nonSilent = await getNonSilentAudioCount(page);
    if (nonSilent >= count) return;
    await new Promise((r) => setTimeout(r, 500));
  }
  throw new Error(
    `Timed out after ${timeout}ms waiting for ${count} non-silent audio stream(s)`
  );
}
