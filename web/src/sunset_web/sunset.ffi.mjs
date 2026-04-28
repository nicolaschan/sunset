// Gleam ↔ sunset-web-wasm bridge. Loads the wasm bundle once on first
// call, caches the module exports, and exposes typed JS functions that
// Gleam externals call.

import init, { Client } from "../../sunset_web_wasm.js";

let initPromise = null;
function ensureLoaded() {
  if (!initPromise) {
    initPromise = init();
  }
  return initPromise;
}

const IDENTITY_KEY = "sunset/identity-seed";

export function loadOrCreateIdentity(callback) {
  let bytes;
  const stored = window.localStorage.getItem(IDENTITY_KEY);
  if (stored && /^[0-9a-fA-F]{64}$/.test(stored)) {
    bytes = new Uint8Array(32);
    for (let i = 0; i < 32; i++) {
      bytes[i] = parseInt(stored.substr(i * 2, 2), 16);
    }
  } else {
    bytes = window.crypto.getRandomValues(new Uint8Array(32));
    const hex = Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join("");
    window.localStorage.setItem(IDENTITY_KEY, hex);
  }
  // Convert Uint8Array to a Gleam BitArray. Gleam's BitArray on JS is the
  // {@link BitArray} class from the Gleam runtime; constructing it from a
  // Uint8Array works directly (the runtime accepts a buffer).
  callback(bytes);
}

export async function createClient(seed, roomName, callback) {
  await ensureLoaded();
  // `seed` arrives from Gleam as a BitArray. Its `.buffer` (or `.rawBuffer`)
  // is the underlying Uint8Array. Try both depending on the runtime version.
  const seedBytes = seed.buffer || seed.rawBuffer || seed;
  const client = new Client(seedBytes, roomName);
  callback(client);
}

export async function addRelay(client, url, callback) {
  try {
    await client.add_relay(url);
    callback({ Ok: undefined });
  } catch (e) {
    callback({ Error: String(e) });
  }
}

export async function publishRoomSubscription(client, callback) {
  try {
    await client.publish_room_subscription();
    callback({ Ok: undefined });
  } catch (e) {
    callback({ Error: String(e) });
  }
}

export async function sendMessage(client, body, sentAtMs, callback) {
  try {
    const nonce = window.crypto.getRandomValues(new Uint8Array(32));
    const valueHashHex = await client.send_message(body, sentAtMs, nonce);
    callback({ Ok: valueHashHex });
  } catch (e) {
    callback({ Error: String(e) });
  }
}

export function onMessage(client, callback) {
  client.on_message((incoming) => {
    callback(incoming);
  });
}

export function relayStatus(client) {
  return client.relay_status;
}

export function relayUrlParam() {
  const params = new URLSearchParams(window.location.search);
  const v = params.get("relay");
  if (v === null) return { 0: undefined };  // Gleam Error(Nil)
  return { 0: v };                            // Gleam Ok(string)
}

// IncomingMessage accessors. The wasm-bindgen-generated class exposes
// fields directly via getters.
export function incAuthorPubkey(msg) { return msg.author_pubkey; }
export function incEpochId(msg) { return msg.epoch_id; }
export function incSentAtMs(msg) { return msg.sent_at_ms; }
export function incBody(msg) { return msg.body; }
export function incValueHashHex(msg) { return msg.value_hash_hex; }
export function incIsSelf(msg) { return msg.is_self; }
