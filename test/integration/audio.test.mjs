// audio.test.mjs — Integration tests for Sunset peer-to-peer audio.

import { describe, it, before, after } from "node:test";
import assert from "node:assert/strict";
import { setup, teardown } from "./setup.mjs";
import {
  launchBrowser,
  openApp,
  joinRoom,
  waitForPeers,
  waitForRelayConnected,
  joinAudio,
  leaveAudio,
  waitForAudioConnected,
  waitForReceivingAudio,
  waitForNotReceivingAudio,
  waitForActiveAudioStreams,
  waitForNonSilentAudio,
  isReceivingAudio,
  getNonSilentAudioCount,
  isAudioJoined,
  getActiveAudioStreamCount,
} from "./helpers.mjs";

let env; // { appUrl, relayMultiaddr }

// Generate unique room names to avoid collisions between test runs
function uniqueRoom(prefix = "audio") {
  return `${prefix}-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`;
}

describe("Sunset Audio Integration Tests", () => {
  before(async () => {
    env = await setup();
  });

  after(async () => {
    await teardown();
  });

  describe("Two peers — basic audio", () => {
    let browserA, browserB;
    let pageA, pageB;
    const room = uniqueRoom("two-audio");

    before(async () => {
      [browserA, browserB] = await Promise.all([
        launchBrowser(),
        launchBrowser(),
      ]);

      pageA = await openApp(browserA, env.appUrl, env.relayMultiaddr);
      pageB = await openApp(browserB, env.appUrl, env.relayMultiaddr);

      await Promise.all([joinRoom(pageA, room), joinRoom(pageB, room)]);
      await Promise.all([
        waitForRelayConnected(pageA),
        waitForRelayConnected(pageB),
      ]);
      await Promise.all([waitForPeers(pageA, 1), waitForPeers(pageB, 1)]);
    });

    after(async () => {
      await Promise.all([browserA?.close(), browserB?.close()]);
    });

    it("should establish WebRTC audio connection when both join", async () => {
      // Both peers join audio
      await joinAudio(pageA);
      await joinAudio(pageB);

      // Wait for WebRTC peer connections to reach "connected" state
      await Promise.all([
        waitForAudioConnected(pageA, 1),
        waitForAudioConnected(pageB, 1),
      ]);

      // Verify both sides show joined state
      const [joinedA, joinedB] = await Promise.all([
        isAudioJoined(pageA),
        isAudioJoined(pageB),
      ]);
      assert.ok(joinedA, "Browser A should show audio as joined");
      assert.ok(joinedB, "Browser B should show audio as joined");
    });

    it("should receive audio on both sides (bidirectional)", async () => {
      // Both should be receiving live audio from each other
      await Promise.all([
        waitForReceivingAudio(pageA),
        waitForReceivingAudio(pageB),
      ]);

      const [recvA, recvB] = await Promise.all([
        isReceivingAudio(pageA),
        isReceivingAudio(pageB),
      ]);
      assert.ok(recvA, "Browser A should be receiving audio from B");
      assert.ok(recvB, "Browser B should be receiving audio from A");

      // Verify the received audio is not silent (actual tone data is flowing)
      await Promise.all([
        waitForNonSilentAudio(pageA, 1),
        waitForNonSilentAudio(pageB, 1),
      ]);

      const [nonSilentA, nonSilentB] = await Promise.all([
        getNonSilentAudioCount(pageA),
        getNonSilentAudioCount(pageB),
      ]);
      assert.ok(nonSilentA >= 1, `Browser A should have 1 non-silent stream, got ${nonSilentA}`);
      assert.ok(nonSilentB >= 1, `Browser B should have 1 non-silent stream, got ${nonSilentB}`);
    });
  });

  describe("Leave and rejoin audio", () => {
    let browserA, browserB;
    let pageA, pageB;
    const room = uniqueRoom("rejoin-audio");

    before(async () => {
      [browserA, browserB] = await Promise.all([
        launchBrowser(),
        launchBrowser(),
      ]);

      pageA = await openApp(browserA, env.appUrl, env.relayMultiaddr);
      pageB = await openApp(browserB, env.appUrl, env.relayMultiaddr);

      await Promise.all([joinRoom(pageA, room), joinRoom(pageB, room)]);
      await Promise.all([
        waitForRelayConnected(pageA),
        waitForRelayConnected(pageB),
      ]);
      await Promise.all([waitForPeers(pageA, 1), waitForPeers(pageB, 1)]);
    });

    after(async () => {
      await Promise.all([browserA?.close(), browserB?.close()]);
    });

    it("should re-establish audio after one peer leaves and rejoins", async () => {
      // Both join audio and verify connection
      await joinAudio(pageA);
      await joinAudio(pageB);
      await Promise.all([
        waitForAudioConnected(pageA, 1),
        waitForAudioConnected(pageB, 1),
      ]);
      await Promise.all([
        waitForReceivingAudio(pageA),
        waitForReceivingAudio(pageB),
      ]);

      // Peer A leaves audio
      await leaveAudio(pageA);

      // Verify A is no longer joined
      const joinedAfterLeave = await isAudioJoined(pageA);
      assert.ok(!joinedAfterLeave, "Browser A should no longer be joined after leaving");

      // B should eventually stop receiving audio from A
      await waitForNotReceivingAudio(pageB);

      // Peer A rejoins audio
      await joinAudio(pageA);

      // Both should re-establish the connection
      await Promise.all([
        waitForAudioConnected(pageA, 1),
        waitForAudioConnected(pageB, 1),
      ]);

      // Both should be receiving audio again
      await Promise.all([
        waitForReceivingAudio(pageA),
        waitForReceivingAudio(pageB),
      ]);

      // Verify the audio is not silent after rejoin
      await Promise.all([
        waitForNonSilentAudio(pageA, 1),
        waitForNonSilentAudio(pageB, 1),
      ]);

      const [recvA, recvB] = await Promise.all([
        getNonSilentAudioCount(pageA),
        getNonSilentAudioCount(pageB),
      ]);
      assert.ok(recvA >= 1, `Browser A should have 1 non-silent stream after rejoin, got ${recvA}`);
      assert.ok(recvB >= 1, `Browser B should have 1 non-silent stream after A rejoined, got ${recvB}`);
    });
  });

  describe("Three peers — mesh audio", () => {
    let browserA, browserB, browserC;
    let pageA, pageB, pageC;
    const room = uniqueRoom("three-audio");

    before(async () => {
      [browserA, browserB, browserC] = await Promise.all([
        launchBrowser(),
        launchBrowser(),
        launchBrowser(),
      ]);

      pageA = await openApp(browserA, env.appUrl, env.relayMultiaddr);
      pageB = await openApp(browserB, env.appUrl, env.relayMultiaddr);
      pageC = await openApp(browserC, env.appUrl, env.relayMultiaddr);

      await Promise.all([
        joinRoom(pageA, room),
        joinRoom(pageB, room),
        joinRoom(pageC, room),
      ]);
      await Promise.all([
        waitForRelayConnected(pageA),
        waitForRelayConnected(pageB),
        waitForRelayConnected(pageC),
      ]);
      // Each peer should see the other 2
      await Promise.all([
        waitForPeers(pageA, 2),
        waitForPeers(pageB, 2),
        waitForPeers(pageC, 2),
      ]);
    });

    after(async () => {
      await Promise.all([
        browserA?.close(),
        browserB?.close(),
        browserC?.close(),
      ]);
    });

    it("should establish audio connections between all three peers", async () => {
      // All three join audio
      await joinAudio(pageA);
      await joinAudio(pageB);
      await joinAudio(pageC);

      // Each peer should have 2 connected audio PCs (one per other peer)
      await Promise.all([
        waitForAudioConnected(pageA, 2),
        waitForAudioConnected(pageB, 2),
        waitForAudioConnected(pageC, 2),
      ]);

      // Verify all are joined
      const [joinedA, joinedB, joinedC] = await Promise.all([
        isAudioJoined(pageA),
        isAudioJoined(pageB),
        isAudioJoined(pageC),
      ]);
      assert.ok(joinedA, "Browser A should show audio as joined");
      assert.ok(joinedB, "Browser B should show audio as joined");
      assert.ok(joinedC, "Browser C should show audio as joined");
    });

    it("all three peers should receive non-silent audio from both others", async () => {
      // Each peer should be receiving 2 live audio streams
      await Promise.all([
        waitForActiveAudioStreams(pageA, 2),
        waitForActiveAudioStreams(pageB, 2),
        waitForActiveAudioStreams(pageC, 2),
      ]);

      const [countA, countB, countC] = await Promise.all([
        getActiveAudioStreamCount(pageA),
        getActiveAudioStreamCount(pageB),
        getActiveAudioStreamCount(pageC),
      ]);
      assert.ok(
        countA >= 2,
        `Browser A should receive audio from 2 peers, got ${countA}`
      );
      assert.ok(
        countB >= 2,
        `Browser B should receive audio from 2 peers, got ${countB}`
      );
      assert.ok(
        countC >= 2,
        `Browser C should receive audio from 2 peers, got ${countC}`
      );

      // Verify ALL received audio streams are not silent
      await Promise.all([
        waitForNonSilentAudio(pageA, 2),
        waitForNonSilentAudio(pageB, 2),
        waitForNonSilentAudio(pageC, 2),
      ]);

      const [nonSilentA, nonSilentB, nonSilentC] = await Promise.all([
        getNonSilentAudioCount(pageA),
        getNonSilentAudioCount(pageB),
        getNonSilentAudioCount(pageC),
      ]);
      assert.ok(nonSilentA >= 2, `Browser A should have 2 non-silent streams, got ${nonSilentA}`);
      assert.ok(nonSilentB >= 2, `Browser B should have 2 non-silent streams, got ${nonSilentB}`);
      assert.ok(nonSilentC >= 2, `Browser C should have 2 non-silent streams, got ${nonSilentC}`);
    });
  });
});
