// setup.mjs — Build app + relay, start servers for integration tests.

import { createServer } from "node:http";
import { execSync, spawn } from "node:child_process";
import { mkdtempSync, readFileSync, existsSync, readdirSync, statSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import handler from "serve-handler";

const ROOT = resolve(import.meta.dirname, "..", "..");

// ── Build ──────────────────────────────────────────────────────────

function buildApp() {
  console.log("[setup] Building Gleam app...");
  execSync("gleam run -m lustre/dev build sunset --minify", {
    cwd: ROOT,
    stdio: "inherit",
  });
  console.log("[setup] App built.");
}

function buildRelay() {
  console.log("[setup] Building relay via nix...");
  const out = execSync("nix build ./relay#default --no-link --print-out-paths", {
    cwd: ROOT,
    encoding: "utf-8",
  }).trim();
  const relayBin = join(out, "bin", "relay");
  console.log(`[setup] Relay binary: ${relayBin}`);
  return relayBin;
}

// ── Static file server ─────────────────────────────────────────────

function startStaticServer() {
  return new Promise((resolve, reject) => {
    const distDir = join(ROOT, "dist");
    const server = createServer((req, res) =>
      handler(req, res, { public: distDir })
    );
    server.listen(0, "127.0.0.1", () => {
      const { port } = server.address();
      console.log(`[setup] Static server on http://127.0.0.1:${port}`);
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

    // Timeout after 30s
    setTimeout(() => {
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

  buildApp();
  const relayBin = buildRelay();

  const [staticServer, relay] = await Promise.all([
    startStaticServer(),
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
