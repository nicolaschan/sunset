// helpers.mjs — Puppeteer helpers for Sunset integration tests.

import puppeteer from "puppeteer";
import { execSync } from "node:child_process";

// ── Browser lifecycle ──────────────────────────────────────────────

/**
 * Find a Chrome/Chromium executable.
 * Prefers system-installed chromium (works on NixOS), falls back to
 * Puppeteer's bundled Chrome.
 */
function findChromePath() {
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
      "--use-fake-ui-for-media-stream",
      "--use-fake-device-for-media-stream",
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
