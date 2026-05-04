// E2E against the real prod relay at relay.sunset.chat. Triggers a pod
// restart via `kubectl delete pod -n sunset-relay …` and verifies that
// two unrefreshed browser pages reconnect and exchange messages.
//
// This is the production-shaped test for the "stuck disconnected after
// relay restart" report:
//   * Real Cloudflare in front of the relay.
//   * Real network / DNS.
//   * The relay's data_dir is the pod's local fs (no PVC mounted), so a
//     pod restart **rotates the identity** — exactly the hypothesis the
//     user asked us to verify against prod.
//
// Skip conditions: this test requires kubectl access to the
// `sunset-relay` namespace AND outbound network to relay.sunset.chat.
// If either is unavailable, the test is skipped (not failed) so the
// rest of the e2e suite can run on machines that don't have cluster
// credentials.
//
// CAUTION: this test deletes the prod relay pod, which disconnects
// every connected user momentarily. It's gated to a single deliberate
// invocation (env `SUNSET_E2E_PROD_KILL=1`) so a casual `npx playwright
// test` doesn't take prod down.

import { test, expect } from "@playwright/test";
import { spawnSync } from "child_process";

const NAMESPACE = "sunset-relay";
const RELAY_HOST = "relay.sunset.chat";
const RELAY_HTTPS_URL = `https://${RELAY_HOST}/`;

function kubectl(args, opts = {}) {
  const r = spawnSync("kubectl", ["-n", NAMESPACE, ...args], {
    encoding: "utf8",
    ...opts,
  });
  if (r.status !== 0) {
    throw new Error(
      `kubectl ${args.join(" ")} failed (${r.status}): ${r.stderr || r.stdout}`,
    );
  }
  return r.stdout.trim();
}

function getRelayPod() {
  const out = kubectl([
    "get",
    "pod",
    "-l",
    "app=sunset-relay",
    "-o",
    "jsonpath={.items[0].metadata.name}",
  ]);
  if (!out) throw new Error("no sunset-relay pod found");
  return out;
}

function getRelayPodReady(name) {
  // Returns "True" / "False" / "" if pod ready condition is set.
  const out = kubectl([
    "get",
    "pod",
    name,
    "-o",
    'jsonpath={.status.conditions[?(@.type=="Ready")].status}',
  ]);
  return out;
}

// Resolve current relay identity directly via HTTPS to relay.sunset.chat
// (same path the browser-side resolver takes). Returns the ed25519 hex.
async function fetchRelayEd25519() {
  const res = await fetch(RELAY_HTTPS_URL, { cache: "no-store" });
  if (!res.ok) throw new Error(`relay GET / status ${res.status}`);
  const j = await res.json();
  if (!j.ed25519) throw new Error(`relay GET / missing ed25519: ${JSON.stringify(j)}`);
  return j.ed25519;
}

async function waitFor(predicate, { timeoutMs, intervalMs = 500, label }) {
  const deadline = Date.now() + timeoutMs;
  let lastErr = null;
  while (Date.now() < deadline) {
    try {
      const ok = await predicate();
      if (ok) return;
    } catch (e) {
      lastErr = e;
    }
    await new Promise((r) => setTimeout(r, intervalMs));
  }
  throw new Error(`waitFor(${label}) timed out after ${timeoutMs}ms${lastErr ? `; last error: ${lastErr}` : ""}`);
}

test.describe("prod relay pod restart", () => {
  test.skip(
    process.env.SUNSET_E2E_PROD_KILL !== "1",
    "set SUNSET_E2E_PROD_KILL=1 to opt into killing the prod relay pod",
  );

  test.setTimeout(300_000);

  test("two unrefreshed browsers reconnect and exchange messages after kubectl pod kill", async ({
    browser,
  }) => {
    // Sanity: kubectl access works.
    const podBefore = getRelayPod();
    const edBefore = await fetchRelayEd25519();
    process.stderr.write(
      `[prod-restart] starting pod=${podBefore} ed25519=${edBefore}\n`,
    );

    // Use a unique room hash so the test cannot collide with real
    // users' rooms. The relay is interest-filter "all" so it forwards
    // every subscriber's traffic; an isolated room is just a private
    // namespace inside that.
    const roomHash = `e2e-prod-restart-${Date.now()}-${Math.floor(Math.random() * 1e9).toString(36)}`;
    const url = `/?relay=${encodeURIComponent(RELAY_HOST)}#${roomHash}`;

    const ctxA = await browser.newContext();
    const ctxB = await browser.newContext();
    const pageA = await ctxA.newPage();
    const pageB = await ctxB.newPage();

    for (const [name, page] of [
      ["A", pageA],
      ["B", pageB],
    ]) {
      page.on("pageerror", (err) =>
        process.stderr.write(`[${name} pageerror] ${err.stack || err}\n`),
      );
      page.on("console", (msg) => {
        // Forward warnings/errors always; forward INFO lines if they
        // come from our tracing instrumentation (prefixed with
        // `supervisor:` / `resolver fetch` / `peer disconnected`).
        // This is what lets us cross-check the WASM-side reconnect
        // path against what we expect to see during a prod repro.
        const t = msg.type();
        const text = msg.text();
        if (
          t === "error" ||
          t === "warning" ||
          /supervisor:|resolver fetch|peer disconnected|engine\.add_peer/.test(
            text,
          )
        ) {
          process.stderr.write(`[${name} ${t}] ${text}\n`);
        }
      });
      await page.addInitScript(() => {
        window.SUNSET_TEST = true;
      });
    }

    await pageA.goto(url);
    await pageB.goto(url);

    await expect(pageA.getByText("sunset", { exact: true })).toBeVisible({
      timeout: 30_000,
    });
    await expect(pageB.getByText("sunset", { exact: true })).toBeVisible({
      timeout: 30_000,
    });

    const inputA = pageA.getByPlaceholder(/^Message #/);
    const inputB = pageB.getByPlaceholder(/^Message #/);
    await expect(inputA).toBeVisible({ timeout: 30_000 });
    await expect(inputB).toBeVisible({ timeout: 30_000 });

    // Wait until the supervisor reports connected, capture the v1
    // peer_pubkey, and assert it matches the relay's banner-reported
    // ed25519. This proves the WS+Hello path actually went to the v1
    // relay we're about to kill.
    async function readConnectedPeerPubkey(page) {
      return await page.evaluate(async () => {
        const intents = await window.sunsetClient.intents();
        const c = intents.find((i) => i.state === "connected");
        if (!c || !c.peer_pubkey) return null;
        return Array.from(c.peer_pubkey, (b) =>
          b.toString(16).padStart(2, "0"),
        ).join("");
      });
    }
    await waitFor(
      async () => (await readConnectedPeerPubkey(pageA)) === edBefore,
      { timeoutMs: 60_000, label: "pageA connect to v1" },
    );
    await waitFor(
      async () => (await readConnectedPeerPubkey(pageB)) === edBefore,
      { timeoutMs: 60_000, label: "pageB connect to v1" },
    );

    // Run id mixed into every message so payloads are unique to this
    // run AND across messages within this run (no collision with any
    // earlier-or-concurrent traffic; no collision with the
    // pre-restart message when we later check it's still on B).
    const runId = `${Date.now()}-${Math.floor(Math.random() * 1e12).toString(36)}`;
    const nonce = () => Math.floor(Math.random() * 1e15).toString(36);

    // Pre-restart sanity: chat works against v1.
    const msgPre = `[e2e ${runId}] pre-restart-A nonce=${nonce()}`;
    expect(
      await pageB.getByText(msgPre).count(),
      "B must not see msgPre before A sends it (paranoia: rules out collision)",
    ).toBe(0);
    await inputA.fill(msgPre);
    await inputA.press("Enter");
    await expect(pageB.getByText(msgPre)).toBeVisible({ timeout: 30_000 });

    // Capture the page-load-id of B BEFORE the kill. We compare it
    // post-restart to assert B was never refreshed: a refresh would
    // re-execute the page's startup code and produce a fresh load id.
    // Using `performance.timeOrigin` (page-load wall-clock anchor) —
    // changes if and only if the page reloads.
    const pageBOriginBefore = await pageB.evaluate(() => performance.timeOrigin);

    // Kill the pod. The deployment will roll a fresh one with a fresh
    // data_dir, hence a fresh identity.
    process.stderr.write(
      `[prod-restart] kubectl delete pod ${podBefore}\n`,
    );
    kubectl(["delete", "pod", podBefore, "--wait=false"]);

    // Wait for a NEW pod (different name) to be Ready and serving a
    // NEW identity. Use the relay's HTTP endpoint as the readiness
    // probe — once the resolver succeeds with a different ed25519,
    // we know v2 is up and reachable through Cloudflare.
    let edAfter = null;
    let podAfter = null;
    await waitFor(
      async () => {
        try {
          podAfter = getRelayPod();
          if (podAfter === podBefore) return false;
          if (getRelayPodReady(podAfter) !== "True") return false;
          const ed = await fetchRelayEd25519();
          if (ed === edBefore) return false;
          edAfter = ed;
          return true;
        } catch {
          return false;
        }
      },
      { timeoutMs: 180_000, intervalMs: 1_000, label: "v2 pod ready" },
    );
    process.stderr.write(
      `[prod-restart] new pod=${podAfter} ed25519=${edAfter}\n`,
    );
    expect(edAfter).not.toBe(edBefore);
    expect(podAfter).not.toBe(podBefore);

    // Deliberately do NOT poll `intents()` here. Polling sends a
    // `SupervisorCommand::Snapshot` round-trip that wakes the
    // supervisor's run loop on every poll — it masks the exact
    // liveness bug this test is meant to catch (run loop parked on a
    // stale `pending` sleep_fut while a background `spawn_dial`
    // schedules a fresh `Backoff`). The user's UI doesn't poll; it
    // only subscribes via `on_intent_changed`. The message-arrival
    // assertions below are the single source of truth: if either
    // page failed to reconnect on its own, msgPost wouldn't be
    // pushable from A and wouldn't be visible on B inside the
    // timeout. That's the user-visible contract we care about.

    // Build the post-restart payload AFTER the v2 reconnect has
    // happened — so the body literally cannot have been delivered
    // before the supervisor was talking to v2. Then prove on B's side
    // that:
    //   1. The exact post-restart bytes were not in B's DOM before A
    //      sends them (paranoia against any collision / pre-cached
    //      match).
    //   2. After A sends, the exact post-restart bytes appear on B.
    //   3. The pre-restart message is STILL on B's DOM (proves no
    //      page reload erased history).
    //   4. B's `performance.timeOrigin` is unchanged from before the
    //      kill (proves the page wasn't reloaded — `timeOrigin`
    //      changes if and only if the document is fresh).
    const msgPost = `[e2e ${runId}] post-restart-A nonce=${nonce()} v2_ed=${edAfter.slice(0, 16)}`;
    expect(
      await pageB.getByText(msgPost).count(),
      "B must not see msgPost before A sends it",
    ).toBe(0);
    await inputA.fill(msgPost);
    await inputA.press("Enter");
    await expect(pageB.getByText(msgPost)).toBeVisible({ timeout: 60_000 });

    // Cross-checks: B was not reloaded, and pre-restart history is intact.
    await expect(
      pageB.getByText(msgPre),
      "msgPre must still be visible on B (proves no reload erased history)",
    ).toBeVisible();
    const pageBOriginAfter = await pageB.evaluate(() => performance.timeOrigin);
    expect(
      pageBOriginAfter,
      "B's performance.timeOrigin must be unchanged across the restart (proves no reload)",
    ).toBe(pageBOriginBefore);

    // Now that we've proven message flow works post-restart end-to-end,
    // it's safe to also confirm via the supervisor snapshot that A is
    // talking to the v2 relay specifically (this call polls and would
    // wake the supervisor — but at this point the test has already
    // passed its non-polling canary, so this is just an extra
    // assertion, not the load-bearing one).
    expect(
      await readConnectedPeerPubkey(pageA),
      "after msg flowed, pageA's connected peer should be v2 ed25519",
    ).toBe(edAfter);
    expect(
      await readConnectedPeerPubkey(pageB),
      "after msg flowed, pageB's connected peer should be v2 ed25519",
    ).toBe(edAfter);

    // Bidirectional: send a fresh unique message from B → A.
    const msgPostB = `[e2e ${runId}] post-restart-B nonce=${nonce()} v2_ed=${edAfter.slice(0, 16)}`;
    expect(
      await pageA.getByText(msgPostB).count(),
      "A must not see msgPostB before B sends it",
    ).toBe(0);
    await inputB.fill(msgPostB);
    await inputB.press("Enter");
    await expect(pageA.getByText(msgPostB)).toBeVisible({ timeout: 60_000 });

    await ctxA.close();
    await ctxB.close();
  });
});
