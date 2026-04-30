// Gleam ↔ sunset-web-wasm bridge. Loads the wasm bundle once on first
// call, caches the module exports, and exposes typed JS functions that
// Gleam externals call.

import { BitArray, Ok, Error as GError, toList } from "../../prelude.mjs";
import init, { Client } from "../../sunset_web_wasm.js";

let initPromise = null;
function ensureLoaded() {
  if (!initPromise) {
    // Explicitly pass the .wasm URL relative to the HTML page; the
    // default import.meta.url-based lookup breaks once the
    // wasm-bindgen JS gets bundled into sunset_web.js.
    initPromise = init("./sunset_web_wasm_bg.wasm");
  }
  return initPromise;
}

const IDENTITY_KEY = "sunset/identity-seed";

function bitsToBytes(bits) {
  // Gleam BitArray exposes its raw buffer in different ways depending on the
  // runtime version; try each in order.
  if (bits.rawBuffer) return bits.rawBuffer;
  if (bits.buffer) return bits.buffer;
  if (bits instanceof Uint8Array) return bits;
  return new Uint8Array(bits);
}

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
  callback(new BitArray(bytes));
}

export async function createClient(seed, roomName, callback) {
  await ensureLoaded();
  const seedBytes = bitsToBytes(seed);
  const client = new Client(seedBytes, roomName);
  // Test-only hook: expose the client to Playwright when SUNSET_TEST is
  // set on `window` before the bundle loads. No-op in production.
  if (typeof window !== "undefined" && window.SUNSET_TEST) {
    window.sunsetClient = client;
  }
  callback(client);
}

export async function clientConnectDirect(client, peerPubkey, callback) {
  try {
    const bytes = bitsToBytes(peerPubkey);
    await client.connect_direct(bytes);
    callback(new Ok(undefined));
  } catch (e) {
    callback(new GError(String(e)));
  }
}

export function clientPeerConnectionMode(client, peerPubkey) {
  const bytes = bitsToBytes(peerPubkey);
  return client.peer_connection_mode(bytes);
}

export async function addRelay(client, url, callback) {
  try {
    await client.add_relay(url);
    callback(new Ok(undefined));
  } catch (e) {
    callback(new GError(String(e)));
  }
}

export async function publishRoomSubscription(client, callback) {
  try {
    await client.publish_room_subscription();
    callback(new Ok(undefined));
  } catch (e) {
    callback(new GError(String(e)));
  }
}

export async function sendMessage(client, body, sentAtMs, callback) {
  try {
    const nonce = window.crypto.getRandomValues(new Uint8Array(32));
    const valueHashHex = await client.send_message(body, sentAtMs, nonce);
    callback(new Ok(valueHashHex));
  } catch (e) {
    callback(new GError(String(e)));
  }
}

export function onMessage(client, callback) {
  client.on_message((incoming) => {
    // Copy fields into a plain JS object so we can free the wasm-bindgen
    // wrapper immediately and avoid GC-delayed memory accumulation.
    const plain = {
      author_pubkey: incoming.author_pubkey,
      epoch_id: incoming.epoch_id,
      sent_at_ms: incoming.sent_at_ms,
      body: incoming.body,
      value_hash_hex: incoming.value_hash_hex,
      is_self: incoming.is_self,
    };
    incoming.free();
    callback(plain);
  });
}

export function relayStatus(client) {
  return client.relay_status;
}

export function relayUrlParam() {
  const params = new URLSearchParams(window.location.search);
  const v = params.get("relay");
  if (v === null) return new GError(undefined);
  return new Ok(v);
}

// Helpers used by sunset_web.gleam for rendering IncomingMessage fields.

export function currentTimeMs() {
  return Date.now();
}

export function shortPubkey(bits) {
  const bytes = bitsToBytes(bits);
  return Array.from(bytes.slice(0, 4), (b) => b.toString(16).padStart(2, "0"))
    .join("");
}

export function shortInitials(bits) {
  const bytes = bitsToBytes(bits);
  return Array.from(bytes.slice(0, 1), (b) => b.toString(16).padStart(2, "0"))
    .join("")
    .toUpperCase();
}

export function formatTimeMs(ms) {
  const d = new Date(ms);
  const hh = String(d.getHours()).padStart(2, "0");
  const mm = String(d.getMinutes()).padStart(2, "0");
  return `${hh}:${mm}`;
}

// IncomingMessage accessors. The wasm-bindgen-generated class exposes
// fields directly via getters.
export function incAuthorPubkey(msg) { return new BitArray(msg.author_pubkey); }
export function incEpochId(msg) { return msg.epoch_id; }
export function incSentAtMs(msg) { return msg.sent_at_ms; }
export function incBody(msg) { return msg.body; }
export function incValueHashHex(msg) { return msg.value_hash_hex; }
export function incIsSelf(msg) { return msg.is_self; }

// Presence + membership FFI shims.

export async function startPresence(client, intervalMs, ttlMs, refreshMs) {
  try {
    await client.start_presence(intervalMs, ttlMs, refreshMs);
  } catch (e) {
    console.warn("startPresence failed", e);
  }
}

export function onMembersChanged(client, callback) {
  client.on_members_changed((members) => {
    try {
      // Copy fields into plain JS objects so we don't hold raw
      // wasm-bindgen pointers across the JS/Gleam boundary —
      // FinalizationRegistry would eventually GC the wrappers, but
      // explicit copy + free is safer and more predictable.
      const copied = Array.from(members, (m) => ({
        pubkey: m.pubkey,                // Vec<u8> getter -> Uint8Array
        presence: m.presence,            // String getter -> string
        connection_mode: m.connection_mode,
        is_self: m.is_self,
      }));
      Array.from(members).forEach((m) => m.free());
      callback(toList(copied));
    } catch (e) {
      console.warn("onMembersChanged callback threw", e);
    }
  });
}

export function onRelayStatusChanged(client, callback) {
  client.on_relay_status_changed((s) => {
    try {
      callback(String(s));
    } catch (e) {
      console.warn("onRelayStatusChanged callback threw", e);
    }
  });
}

export function memPubkey(m) {
  return new BitArray(m.pubkey);
}
export function memPresence(m) {
  return m.presence;
}
export function memConnectionMode(m) {
  return m.connection_mode;
}
export function memIsSelf(m) {
  return m.is_self;
}

export function presenceParamsFromUrl() {
  const params = new URLSearchParams(window.location.search);
  const parseOr = (key, dflt) => {
    const raw = params.get(key);
    if (raw === null) return dflt;
    const n = Number(raw);  // strict — "30000abc" -> NaN, unlike parseInt
    return Number.isFinite(n) && n > 0 ? n : dflt;
  };
  const interval = parseOr("presence_interval", 30000);
  const ttl = parseOr("presence_ttl", 60000);
  const refresh = parseOr("presence_refresh", 5000);
  // Gleam tuple #(Int, Int, Int) is a 3-element JS array.
  return [interval, ttl, refresh];
}

// Delivery-receipt FFI.

export function onReceipt(client, callback) {
  client.on_receipt((incoming) => {
    // Copy fields into a plain JS object so we can free the wasm-bindgen
    // wrapper immediately and avoid GC-delayed memory accumulation.
    const plain = {
      for_value_hash_hex: incoming.for_value_hash_hex,
      from_pubkey: incoming.from_pubkey,
    };
    incoming.free();
    callback(plain);
  });
}

export function recForValueHashHex(rec) {
  return rec.for_value_hash_hex;
}

export function recFromPubkey(rec) {
  return new BitArray(rec.from_pubkey);
}
