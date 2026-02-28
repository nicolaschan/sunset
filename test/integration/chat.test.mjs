// chat.test.mjs — Integration tests for Sunset peer-to-peer chat.

import { describe, it, before, after } from "node:test";
import assert from "node:assert/strict";
import { setup, teardown } from "./setup.mjs";
import {
  launchBrowser,
  openApp,
  joinRoom,
  sendMessage,
  waitForMessage,
  waitForPeers,
  waitForRelayConnected,
  getMessages,
  setDisplayName,
  getConnectedPeerCount,
} from "./helpers.mjs";

let env; // { appUrl, relayMultiaddr }
let browserA, browserB;
let pageA, pageB;

// Generate unique room names to avoid collisions between test runs
function uniqueRoom(prefix = "test") {
  return `${prefix}-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`;
}

describe("Sunset Integration Tests", () => {
  before(async () => {
    env = await setup();
    [browserA, browserB] = await Promise.all([
      launchBrowser(),
      launchBrowser(),
    ]);
  });

  after(async () => {
    await Promise.all([
      browserA?.close(),
      browserB?.close(),
    ]);
    await teardown();
  });

  describe("Home page", () => {
    it("should display the landing page with room join form", async () => {
      const page = await openApp(browserA, env.appUrl, env.relayMultiaddr);
      const title = await page.$eval(".landing-title", (el) => el.textContent);
      assert.equal(title, "Sunset Chat");

      const input = await page.$(".room-input");
      assert.ok(input, "Room input should be visible");

      const button = await page.$(".room-button");
      assert.ok(button, "Join button should be visible");

      await page.close();
    });
  });

  describe("Room — empty state", () => {
    it("should show empty message state when entering a room", async () => {
      const page = await openApp(browserA, env.appUrl, env.relayMultiaddr);
      const room = uniqueRoom("empty");
      await joinRoom(page, room);

      const emptyText = await page.$eval(
        ".room-messages-empty",
        (el) => el.textContent
      );
      assert.ok(
        emptyText.includes("No messages yet"),
        `Expected empty state, got: ${emptyText}`
      );

      await page.close();
    });
  });

  describe("Two-instance chat", () => {
    const room = uniqueRoom("chat");

    before(async () => {
      // Open fresh pages for both browsers
      pageA = await openApp(browserA, env.appUrl, env.relayMultiaddr);
      pageB = await openApp(browserB, env.appUrl, env.relayMultiaddr);

      // Both join the same room
      await Promise.all([joinRoom(pageA, room), joinRoom(pageB, room)]);

      // Wait for relay connection on both sides
      await Promise.all([
        waitForRelayConnected(pageA),
        waitForRelayConnected(pageB),
      ]);

      // Wait for them to discover and connect to each other
      await Promise.all([
        waitForPeers(pageA, 1),
        waitForPeers(pageB, 1),
      ]);
    });

    after(async () => {
      await pageA?.close();
      await pageB?.close();
    });

    it("should show each other as connected peers", async () => {
      const countA = await getConnectedPeerCount(pageA);
      const countB = await getConnectedPeerCount(pageB);
      assert.ok(countA >= 1, `Browser A should see at least 1 peer, got ${countA}`);
      assert.ok(countB >= 1, `Browser B should see at least 1 peer, got ${countB}`);
    });

    it("Browser A sends a message, Browser B receives it", async () => {
      await sendMessage(pageA, "Hello from A!");

      // Verify A sees its own message
      await waitForMessage(pageA, "Hello from A!");

      // Verify B receives the message
      await waitForMessage(pageB, "Hello from A!");

      const msgsA = await getMessages(pageA);
      const selfMsg = msgsA.find((m) => m.body === "Hello from A!");
      assert.ok(selfMsg, "A should see its own message");
      assert.ok(selfMsg.isSelf, "A's message should be marked as self");

      const msgsB = await getMessages(pageB);
      const receivedMsg = msgsB.find((m) => m.body === "Hello from A!");
      assert.ok(receivedMsg, "B should see A's message");
      assert.ok(!receivedMsg.isSelf, "A's message should not be marked as self on B");
    });

    it("Browser B sends a reply, Browser A receives it", async () => {
      await sendMessage(pageB, "Hello from B!");

      await waitForMessage(pageB, "Hello from B!");
      await waitForMessage(pageA, "Hello from B!");

      const msgsA = await getMessages(pageA);
      const receivedMsg = msgsA.find((m) => m.body === "Hello from B!");
      assert.ok(receivedMsg, "A should see B's reply");
      assert.ok(!receivedMsg.isSelf, "B's message should not be marked as self on A");
    });
  });

  describe("Display names", () => {
    it("should show sender display name on the receiving side", async () => {
      const room = uniqueRoom("names");
      const pA = await openApp(browserA, env.appUrl, env.relayMultiaddr);
      const pB = await openApp(browserB, env.appUrl, env.relayMultiaddr);

      await Promise.all([joinRoom(pA, room), joinRoom(pB, room)]);
      await Promise.all([
        waitForRelayConnected(pA),
        waitForRelayConnected(pB),
      ]);
      await Promise.all([waitForPeers(pA, 1), waitForPeers(pB, 1)]);

      // Set display name on A
      await setDisplayName(pA, "Alice");

      // Wait a few ticks for presence to propagate
      await new Promise((r) => setTimeout(r, 3000));

      // A sends a message
      await sendMessage(pA, "My name is Alice");
      await waitForMessage(pB, "My name is Alice");

      // Check that B sees the sender as "Alice"
      const msgsB = await getMessages(pB);
      const msg = msgsB.find((m) => m.body === "My name is Alice");
      assert.ok(msg, "B should see Alice's message");
      assert.equal(msg.sender, "Alice", `Expected sender 'Alice', got '${msg.sender}'`);

      await Promise.all([pA.close(), pB.close()]);
    });
  });

  describe("Room isolation", () => {
    it("should NOT see messages from a different room", async () => {
      const roomX = uniqueRoom("room-x");
      const roomY = uniqueRoom("room-y");

      const pA = await openApp(browserA, env.appUrl, env.relayMultiaddr);
      const pB = await openApp(browserB, env.appUrl, env.relayMultiaddr);

      // Join DIFFERENT rooms
      await joinRoom(pA, roomX);
      await joinRoom(pB, roomY);

      await Promise.all([
        waitForRelayConnected(pA),
        waitForRelayConnected(pB),
      ]);

      // A sends a message
      await sendMessage(pA, "Secret message in room X");
      await waitForMessage(pA, "Secret message in room X");

      // Wait a few seconds to confirm B does NOT receive it
      await new Promise((r) => setTimeout(r, 5000));
      const msgsB = await getMessages(pB);
      const leaked = msgsB.find((m) => m.body.includes("Secret message in room X"));
      assert.equal(leaked, undefined, "Messages should not leak across rooms");

      await Promise.all([pA.close(), pB.close()]);
    });
  });
});
