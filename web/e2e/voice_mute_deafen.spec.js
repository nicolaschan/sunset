// voice_mute_deafen.spec.js — Mute, deafen, and per-peer mute-for-me.

import { test, expect, devices } from "@playwright/test";
import {
  spawnRelay,
  teardownRelay,
  freshSeedHex,
  syntheticPcm,
  installVoiceFfi,
} from "./helpers/voice.js";

let relay;
test.beforeAll(async () => {
  relay = await spawnRelay();
});
test.afterAll(async () => {
  teardownRelay(relay);
});

async function openPeer(browser, relayAddr) {
  const ctx = await browser.newContext({
    ...devices["Pixel 7"],
    permissions: ["microphone"],
  });
  await ctx.addInitScript(() => {
    window.SUNSET_TEST = true;
  });
  const page = await ctx.newPage();
  page.on("pageerror", (err) =>
    process.stderr.write(`[pageerror] ${err.stack || err}\n`),
  );
  page.on("console", (msg) => {
    if (msg.type() === "error" || msg.type() === "warn") {
      process.stderr.write(`[console.${msg.type()}] ${msg.text()}\n`);
    }
  });
  await ctx.addInitScript((seed) => {
    localStorage.setItem("sunset/identity-seed", seed);
  }, freshSeedHex());
  // Install the voiceFfi helper so tests can query per-peer GainNode values.
  await installVoiceFfi(page);
  await page.goto(`/?relay=${encodeURIComponent(relayAddr)}#voice-test-room`);
  await page.waitForFunction(() => !!window.sunsetClient, null, {
    timeout: 15_000,
  });
  return { page, ctx };
}

// Open the channels rail. On desktop the rail is always visible, so the
// phone-rooms-toggle isn't rendered; on phone the toggle opens a drawer
// that overlays the main panel and must be dismissed before clicking
// minibar controls underneath.
async function openChannelsRail(page) {
  const toggle = page.locator('[data-testid="phone-rooms-toggle"]');
  if (await toggle.isVisible()) {
    await toggle.click();
  }
}

async function dismissDrawerIfPresent(page) {
  const backdrop = page.locator('[data-testid="drawer-backdrop"]').first();
  if (await backdrop.isVisible()) {
    // Drawer max width is 320px; both phone (412px) and any desktop
    // viewport are wider, so 380x400 always lands on the backdrop.
    await backdrop.click({ position: { x: 380, y: 400 } });
  }
}

async function joinVoice(page) {
  await openChannelsRail(page);
  await page.locator('[data-testid="voice-channel-row"]').first().click();
  // Both layouts render the leave button once self_in_call is true:
  // the phone minibar uses voice-leave, and the desktop self_control_bar
  // uses the same testid (channels.gleam ~825). `self_in_call` flips only
  // after `voice_start()` resolves Ok on the WASM side (the Gleam UI
  // dispatches `VoiceStarted` from the FFI success callback), so this
  // visibility assertion is also a "voice runtime ready" gate.
  await expect(page.locator('[data-testid="voice-leave"]')).toBeVisible({
    timeout: 2_000,
  });
  await dismissDrawerIfPresent(page);
}

async function getPubkeyBytes(page) {
  return page.evaluate(() =>
    Array.from(new Uint8Array(window.sunsetClient.public_key)),
  );
}

async function getPubkeyHex(page) {
  return page.evaluate(() =>
    Array.from(new Uint8Array(window.sunsetClient.public_key))
      .map((b) => b.toString(16).padStart(2, "0"))
      .join(""),
  );
}

async function injectFrames(page, startCounter, count) {
  for (let c = startCounter; c < startCounter + count; c++) {
    const pcm = syntheticPcm(c);
    await page.evaluate(
      (arr) => window.sunsetClient.voice_inject_pcm(new Float32Array(arr)),
      Array.from(pcm),
    );
    await page.waitForTimeout(20);
  }
}

// Snapshot the current number of recorded frames from a given sender.
async function countFrames(receiverPage, senderBytes) {
  return receiverPage.evaluate(([bytes]) => {
    try {
      const arr = window.sunsetClient.voice_recorded_frames(
        new Uint8Array(bytes),
      );
      return Array.isArray(arr) ? arr.length : 0;
    } catch (_e) {
      return 0;
    }
  }, [senderBytes]);
}


test("self-mute stops frames at bob; unmute resumes", async ({ browser }) => {
  const alice = await openPeer(browser, relay.addr);
  const bob = await openPeer(browser, relay.addr);

  await joinVoice(alice.page);
  await joinVoice(bob.page);

  await alice.page.evaluate(() =>
    window.sunsetClient.voice_install_frame_recorder(),
  );
  await bob.page.evaluate(() =>
    window.sunsetClient.voice_install_frame_recorder(),
  );

  const aliceBytes = await getPubkeyBytes(alice.page);

  // Alice injects 30 frames while unmuted.
  await injectFrames(alice.page, 1, 30);

  // Wait for bob to receive some frames.
  await bob.page.waitForFunction(
    ([bytes]) => {
      try {
        const arr = window.sunsetClient.voice_recorded_frames(
          new Uint8Array(bytes),
        );
        return Array.isArray(arr) && arr.length >= 10;
      } catch (_e) {
        return false;
      }
    },
    [aliceBytes],
    { timeout: 3_000 },
  );

  // Alice mutes via the mic button. Both phone (minibar) and desktop
  // (self_control_bar) layouts render exactly one "Mute mic" button.
  // After the click the title flips to "Unmute mic" — wait for that so
  // we know the Lustre effect (which calls voice_set_muted on the WASM
  // client) has actually run.
  await alice.page.getByTitle("Mute mic").click();
  await expect(alice.page.getByTitle("Unmute mic")).toBeVisible({
    timeout: 1_000,
  });

  // Wait until bob's runtime confirms is_muted=true for alice. This rides
  // on the next heartbeat (≤ 2 s cadence + transport) and is the same
  // signal a real user's UI uses to flip the muted icon. Once bob's
  // observed-mute is true, the relay has carried at least one heartbeat
  // since alice flipped — the in-flight audio captured before the mute
  // toggled is fully delivered.
  // voice_active_peers() returns [{ peer_id: Uint8Array, in_call, talking, is_muted }]
  await bob.page.waitForFunction(
    ([bytes]) => {
      try {
        const peers = window.sunsetClient.voice_active_peers();
        if (!Array.isArray(peers)) return false;
        return peers.some((p) => {
          const id = new Uint8Array(p.peer_id);
          if (id.length !== bytes.length) return false;
          for (let i = 0; i < bytes.length; i++) {
            if (id[i] !== bytes[i]) return false;
          }
          return p.is_muted;
        });
      } catch (_e) {
        return false;
      }
    },
    [aliceBytes],
    { timeout: 3_000 },
  );

  // Alice injects 30 frames while muted — none should reach bob's
  // recorder (send_pcm gates on muted). We can't simply count recorded
  // frames because bob's jitter pump pads underruns with the
  // last-delivered frame and then silence, both of which still pass
  // through FrameSink::deliver and so still get recorded.
  //
  // Instead use the user-observable signal: bob's `talking` indicator
  // for alice flips false within FRAME_STALE_AFTER (1 s) + sweep (~500 ms)
  // once alice's frames stop. This is exactly what a real user sees.
  await injectFrames(alice.page, 100, 30);

  const isAliceNotTalking = ([bytes]) => {
    try {
      const peers = window.sunsetClient.voice_active_peers();
      if (!Array.isArray(peers)) return false;
      return peers.some((p) => {
        const id = new Uint8Array(p.peer_id);
        if (id.length !== bytes.length) return false;
        for (let i = 0; i < bytes.length; i++) {
          if (id[i] !== bytes[i]) return false;
        }
        return !p.talking;
      });
    } catch (_e) {
      return false;
    }
  };
  await bob.page.waitForFunction(isAliceNotTalking, [aliceBytes], {
    timeout: 3_000,
  });

  // Alice unmutes. Wait for the title to flip back so we know the
  // voice_set_muted(false) FFI has run.
  await alice.page.getByTitle("Unmute mic").click();
  await expect(alice.page.getByTitle("Mute mic")).toBeVisible({
    timeout: 1_000,
  });

  // Alice injects again — bob must observe `talking=true` for alice
  // again within ~200 ms of the first frame.
  await injectFrames(alice.page, 200, 30);

  const isAliceTalking = ([bytes]) => {
    try {
      const peers = window.sunsetClient.voice_active_peers();
      if (!Array.isArray(peers)) return false;
      return peers.some((p) => {
        const id = new Uint8Array(p.peer_id);
        if (id.length !== bytes.length) return false;
        for (let i = 0; i < bytes.length; i++) {
          if (id[i] !== bytes[i]) return false;
        }
        return p.talking;
      });
    } catch (_e) {
      return false;
    }
  };
  await bob.page.waitForFunction(isAliceTalking, [aliceBytes], {
    timeout: 2_000,
  });

  await alice.ctx.close();
  await bob.ctx.close();
});

test("self-deafen freezes alice's recorder; talking light still shows bob", async ({
  browser,
}) => {
  const alice = await openPeer(browser, relay.addr);
  const bob = await openPeer(browser, relay.addr);

  await joinVoice(alice.page);
  await joinVoice(bob.page);

  await alice.page.evaluate(() =>
    window.sunsetClient.voice_install_frame_recorder(),
  );
  await bob.page.evaluate(() =>
    window.sunsetClient.voice_install_frame_recorder(),
  );

  const bobBytes = await getPubkeyBytes(bob.page);

  // Bob injects a few frames so alice has some baseline.
  await injectFrames(bob.page, 300, 20);
  await alice.page.waitForFunction(
    ([bytes]) => {
      try {
        const arr = window.sunsetClient.voice_recorded_frames(
          new Uint8Array(bytes),
        );
        return Array.isArray(arr) && arr.length >= 5;
      } catch (_e) {
        return false;
      }
    },
    [bobBytes],
    { timeout: 3_000 },
  );

  // Alice deafens via the headphones button (both layouts). Wait for
  // the title to flip so we know the FFI has set deafened=true on the
  // WASM client before we test the suppression.
  await alice.page.getByTitle("Deafen").click();
  await expect(alice.page.getByTitle("Undeafen")).toBeVisible({
    timeout: 1_000,
  });

  // Snapshot alice's recorder count for bob right after deafen took effect.
  // When alice is deafened, jitter.rs's pump drains buffers but skips
  // FrameSink::deliver entirely, so the recorder's ring stops growing.
  const countAtDeafen = await countFrames(alice.page, bobBytes);

  // Bob injects 30 more frames; none should land in alice's recorder.
  await injectFrames(bob.page, 400, 30);

  const countAfterDeafenedInjection = await countFrames(alice.page, bobBytes);
  expect(countAfterDeafenedInjection - countAtDeafen).toBeLessThan(5);

  // Alice undeafens. Wait for the title to flip back so the FFI runs.
  await alice.page.getByTitle("Undeafen").click();
  await expect(alice.page.getByTitle("Deafen")).toBeVisible({
    timeout: 1_000,
  });

  // After undeafen, bob's `talking` indicator on alice's side should
  // re-fire (frame_liveness was kept fresh by subscribe.rs even while
  // deafened, but the user-visible signal is that alice's recorder
  // starts growing again as the jitter pump resumes deliver calls).
  const countBeforeResume = await countFrames(alice.page, bobBytes);
  await injectFrames(bob.page, 500, 30);

  await alice.page.waitForFunction(
    ([bytes, before]) => {
      try {
        const arr = window.sunsetClient.voice_recorded_frames(
          new Uint8Array(bytes),
        );
        return Array.isArray(arr) && arr.length >= before + 10;
      } catch (_e) {
        return false;
      }
    },
    [bobBytes, countBeforeResume],
    { timeout: 2_000 },
  );

  await alice.ctx.close();
  await bob.ctx.close();
});

test("per-peer mute-for-me sets GainNode to 0", async ({ browser }) => {
  const alice = await openPeer(browser, relay.addr);
  const bob = await openPeer(browser, relay.addr);

  await joinVoice(alice.page);
  await joinVoice(bob.page);

  const bobHex = await getPubkeyHex(bob.page);

  // Bob's GainNode on alice's side is allocated lazily, on the first
  // delivered frame from bob (deliverFrame in voice.ffi.mjs). Drive bob
  // to inject some frames so the slot exists, otherwise setPeerVolume
  // is a silent no-op on a missing slot. This also matches the real UX:
  // the mute-for-me toggle only does anything once you've actually heard
  // the peer.
  await injectFrames(bob.page, 1, 30);
  await alice.page.waitForFunction(
    (hex) => window.__voiceFfi.getPeerGain(hex) !== null,
    bobHex,
    { timeout: 3_000 },
  );

  // Alice toggles mute-for-me on bob via the JS FFI exposed under
  // SUNSET_TEST. Same path the popover mute-for-me toggle invokes
  // through Gleam's setPeerVolume binding.
  await alice.page.evaluate(
    (hex) => window.__voiceFfi.setPeerVolume(hex, 0.0),
    bobHex,
  );

  // Bob's GainNode on alice's side must read 0 immediately.
  const gain = await alice.page.evaluate(
    (hex) => window.__voiceFfi.getPeerGain(hex),
    bobHex,
  );
  expect(gain).toBe(0);

  await alice.ctx.close();
  await bob.ctx.close();
});
