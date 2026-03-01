// setup.mjs — Start servers for integration tests.
//
// Requires two environment variables pointing to pre-built artifacts:
//   SUNSET_DIST_DIR  — path to the built frontend dist directory
//   SUNSET_RELAY_BIN — path to the relay binary
//
// Build them with Nix:
//   nix build .#sunsetDist --out-link dist-result
//   nix build ./relay --out-link relay-result
// Then:
//   SUNSET_DIST_DIR=./dist-result SUNSET_RELAY_BIN=./relay-result/bin/relay npm test

import { createServer } from "node:http";
import { spawn } from "node:child_process";
import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import handler from "serve-handler";

// ── Configuration ──────────────────────────────────────────────────

function requireEnv(name, altNames = []) {
  for (const n of [name, ...altNames]) {
    if (process.env[n]) return process.env[n];
  }
  const names = [name, ...altNames].join(" or ");
  throw new Error(
    `Missing environment variable: ${names}\n` +
    `Build the artifacts with Nix first:\n` +
    `  nix build .#sunsetDist --out-link dist-result\n` +
    `  nix build ./relay --out-link relay-result\n` +
    `Then run:\n` +
    `  SUNSET_DIST_DIR=./dist-result SUNSET_RELAY_BIN=./relay-result/bin/relay npm test`
  );
}

function getDistDir() {
  return requireEnv("SUNSET_DIST_DIR");
}

function getRelayBin() {
  return requireEnv("SUNSET_RELAY_BIN", ["RELAY_BIN"]);
}

// ── Static file server ─────────────────────────────────────────────

function startStaticServer(distDir) {
  return new Promise((resolve, reject) => {
    const server = createServer((req, res) =>
      handler(req, res, { public: distDir })
    );
    server.listen(0, "127.0.0.1", () => {
      const { port } = server.address();
      console.log(`[setup] Static server on http://127.0.0.1:${port} (serving ${distDir})`);
      resolve({ server, port });
    });
    server.on("error", reject);
  });
}

// ── Relay server ───────────────────────────────────────────────────

function startRelay(relayBin) {
  return new Promise((resolve, reject) => {
    const tmpDir = mkdtempSync(join(tmpdir(), "sunset-relay-"));
    const identityPath = join(tmpDir, "identity.key");
    // Use port 0 — the relay will pick an available port
    // We parse the actual port from its log output
    const proc = spawn(relayBin, ["--port", "0", "--identity", identityPath], {
      stdio: ["ignore", "pipe", "pipe"],
      env: { ...process.env, RUST_LOG: "info" },
    });

    let relayInfo = null;
    let stderr = "";

    const onData = (chunk) => {
      const text = chunk.toString();
      stderr += text;
      process.stderr.write(text);

      // Parse lines like: Listening on /ip4/127.0.0.1/tcp/XXXXX/ws/p2p/12D3KooW...
      const match = text.match(
        /Listening on (\/ip4\/127\.0\.0\.1\/tcp\/\d+\/ws\/p2p\/\S+)/
      );
      if (match && !relayInfo) {
        clearTimeout(timeoutId);
        relayInfo = {
          proc,
          multiaddr: match[1],
          tmpDir,
        };
        console.log(`[setup] Relay multiaddr: ${relayInfo.multiaddr}`);
        resolve(relayInfo);
      }
    };

    proc.stderr.on("data", onData);
    proc.stdout.on("data", onData);

    proc.on("error", reject);
    proc.on("exit", (code) => {
      if (!relayInfo) {
        reject(
          new Error(
            `Relay exited with code ${code} before announcing address.\nstderr: ${stderr}`
          )
        );
      }
    });

    const timeoutId = setTimeout(() => {
      if (!relayInfo) {
        proc.kill();
        reject(new Error(`Relay did not announce address within 30s.\nstderr: ${stderr}`));
      }
    }, 30_000);
  });
}

// ── Public API ──────────────────────────────────────────────────────

let _state = null;

export async function setup() {
  if (_state) return _state;

  const distDir = getDistDir();
  const relayBin = getRelayBin();
  console.log(`[setup] Dist dir: ${distDir}`);
  console.log(`[setup] Relay bin: ${relayBin}`);

  const [staticServer, relay] = await Promise.all([
    startStaticServer(distDir),
    startRelay(relayBin),
  ]);

  _state = {
    appUrl: `http://127.0.0.1:${staticServer.port}`,
    relayMultiaddr: relay.multiaddr,
    _staticServer: staticServer.server,
    _relayProc: relay.proc,
    _tmpDir: relay.tmpDir,
  };

  return _state;
}

export async function teardown() {
  if (!_state) return;

  _state._relayProc.kill("SIGTERM");
  _state._staticServer.close();

  // Wait briefly for clean shutdown
  await new Promise((r) => setTimeout(r, 500));
  _state = null;
}
