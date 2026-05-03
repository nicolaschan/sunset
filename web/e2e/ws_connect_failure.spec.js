// Regression test for the wasm-bindgen "closure invoked recursively
// or after being dropped" panic that wedged the page when the WS
// dial against a broken upstream failed.
//
// Repro:
//   * The browser opens a TCP connection to a TLS endpoint.
//   * The endpoint accepts but never completes the WS upgrade
//     (mimicking a misconfigured reverse proxy that swallows
//     `Upgrade: websocket`). After ~few seconds the server drops
//     the TCP connection.
//   * The browser fires onclose; our wasm code returns Err from
//     `WebSocketRawTransport::connect`.
//   * **Without the Drop impl on `WebSocketRawConnection`**, the
//     local `Closure` instances are dropped while the JS WebSocket
//     still has live `.on*` handlers pointing at them. The next
//     event from the dying socket invokes a freed closure →
//     wasm-bindgen throws "closure invoked recursively or after
//     being dropped" → wasm is poisoned → no further connection
//     attempt can succeed.
//   * **With the Drop impl**, the JS handlers are detached before
//     the closures drop, so any in-flight events have nowhere to
//     fire and no panic is raised.
//
// We verify the fix by:
//   1. Pointing the page at a TCP listener that accepts and then
//      closes without responding.
//   2. Letting the supervisor's first dial fail.
//   3. Asserting no `pageerror` or wasm panic appears in the page's
//      console during a generous wait.

import { test, expect } from "@playwright/test";
import { createServer } from "net";

let blackholeServer = null;
let blackholePort = null;

test.beforeAll(async () => {
  // TCP listener: accept, send nothing, close after a short delay.
  // The browser sees TCP connect succeed → sends WS upgrade request
  // → server drops without responding → onclose fires → connect()
  // returns Err. This is the exact shape that triggered the panic
  // in production.
  blackholeServer = createServer((sock) => {
    sock.on("error", () => {});
    setTimeout(() => sock.destroy(), 200);
  });
  await new Promise((resolve) => {
    blackholeServer.listen(0, "127.0.0.1", resolve);
  });
  blackholePort = blackholeServer.address().port;
});

test.afterAll(async () => {
  if (blackholeServer) {
    await new Promise((resolve) => blackholeServer.close(resolve));
  }
});

test.setTimeout(30_000);
test("WS connect failure does not panic the wasm with closure-after-drop", async ({
  browser,
}) => {
  const ctx = await browser.newContext();
  const page = await ctx.newPage();

  const pageErrors = [];
  const consoleErrors = [];
  page.on("pageerror", (err) => {
    process.stderr.write(`[pageerror] ${err.stack || err}\n`);
    pageErrors.push(String(err));
  });
  page.on("console", (msg) => {
    if (msg.type() === "error") {
      const text = msg.text();
      process.stderr.write(`[console error] ${text}\n`);
      consoleErrors.push(text);
    }
  });

  // Use ?relay=ws://127.0.0.1:port (loopback → plain ws, no
  // resolver fetch; the canonical-form fragment is required by
  // PeerAddr but the noise key never gets exchanged because the
  // server hangs up). The fragment is a placeholder x25519 key —
  // its value doesn't matter because the dial will fail before
  // Noise IK starts.
  const fakeKey = "00".repeat(32);
  const url = `/?relay=${encodeURIComponent(`ws://127.0.0.1:${blackholePort}#x25519=${fakeKey}`)}#sunset-blackhole`;

  await page.goto(url);
  await expect(page.getByText("sunset", { exact: true })).toBeVisible({
    timeout: 15_000,
  });

  // Give the supervisor a moment to attempt the dial, see it fail,
  // and surface the error. With the fix this completes cleanly;
  // without the fix the wasm panics inside the WS event handler
  // shortly after `connect` returns Err.
  await page.waitForTimeout(8_000);

  const panics = [...pageErrors, ...consoleErrors].filter((m) =>
    /closure invoked recursively or after being dropped/i.test(m),
  );
  expect(panics, `expected no closure-drop panics, got:\n${panics.join("\n")}`).toEqual([]);

  await ctx.close();
});
