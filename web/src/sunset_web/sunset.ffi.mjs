// Gleam ↔ sunset-web-wasm bridge. Loads the wasm bundle once on first
// call, caches the module exports, and exposes typed JS functions that
// Gleam externals call.

import { BitArray, Ok, Error as GError, toList } from "../../prelude.mjs";
import { Some, None } from "../../gleam_stdlib/gleam/option.mjs";
import { IntentSnapshot } from "./sunset.mjs";

// The wasm-bindgen bundle is loaded lazily via dynamic `import()` so
// that gleam unit tests (which run on Node and never touch the wasm
// runtime) don't fail at module-resolution time when the bundle isn't
// present in the build directory.
let wasmModulePromise = null;
function loadWasmModule() {
  if (!wasmModulePromise) {
    wasmModulePromise = import("../../sunset_web_wasm.js");
  }
  return wasmModulePromise;
}

let initPromise = null;
function ensureLoaded() {
  if (!initPromise) {
    initPromise = loadWasmModule().then((mod) =>
      // Explicitly pass the .wasm URL relative to the HTML page; the
      // default import.meta.url-based lookup breaks once the
      // wasm-bindgen JS gets bundled into sunset_web.js.
      mod.default("./sunset_web_wasm_bg.wasm"),
    );
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

export async function createClient(seed, heartbeatIntervalMs, callback) {
  await ensureLoaded();
  const { Client } = await loadWasmModule();
  const seedBytes = bitsToBytes(seed);
  const hb =
    Number.isFinite(heartbeatIntervalMs) && heartbeatIntervalMs > 0
      ? heartbeatIntervalMs
      : 0;
  const client = new Client(seedBytes, hb);
  // Test-only hook: expose the client to Playwright when SUNSET_TEST is
  // set on `window` before the bundle loads. No-op in production.
  if (typeof window !== "undefined" && window.SUNSET_TEST) {
    window.sunsetClient = client;
  }
  callback(client);
}

export async function clientOpenRoom(client, name, callback) {
  const handle = await client.open_room(name);
  // Test-only hook: expose the most-recently-opened room handle to
  // Playwright when SUNSET_TEST is set. No-op in production.
  if (typeof window !== "undefined" && window.SUNSET_TEST) {
    window.sunsetRoom = handle;
  }
  callback(handle);
}

export async function clientConnectDirect(room, peerPubkey, callback) {
  try {
    const bytes = bitsToBytes(peerPubkey);
    await room.connect_direct(bytes);
    callback(new Ok(undefined));
  } catch (e) {
    callback(new GError(String(e)));
  }
}

export function clientPeerConnectionMode(room, peerPubkey) {
  const bytes = bitsToBytes(peerPubkey);
  return room.peer_connection_mode(bytes);
}

export async function addRelay(client, url, callback) {
  try {
    const id = await client.add_relay(url);
    callback(new Ok(id));
  } catch (e) {
    callback(new GError(String(e)));
  }
}

export function onIntentChanged(client, callback) {
  client.on_intent_changed((snap) => {
    // Copy the wasm-bindgen object's fields into a real Gleam
    // IntentSnapshot record so pattern-matching + field access both
    // work. `peer_pubkey` and `kind` arrive as `undefined` when None
    // on the Rust side; wrap into Some/None.
    const peerPubkey = snap.peer_pubkey;
    const kind = snap.kind;
    const lastPongMs = snap.last_pong_at_unix_ms;
    const lastRttMs = snap.last_rtt_ms;
    const record = new IntentSnapshot(
      snap.id,
      snap.state,
      snap.label,
      peerPubkey === undefined || peerPubkey === null
        ? new None()
        : new Some(new BitArray(peerPubkey)),
      kind === undefined || kind === null ? new None() : new Some(kind),
      snap.attempt,
      lastPongMs === undefined || lastPongMs === null
        ? new None()
        : new Some(lastPongMs),
      lastRttMs === undefined || lastRttMs === null
        ? new None()
        : new Some(lastRttMs),
    );
    callback(record);
  });
}

export async function sendMessage(room, body, sentAtMs, callback) {
  try {
    const valueHashHex = await room.send_message(body, sentAtMs);
    callback(new Ok(valueHashHex));
  } catch (e) {
    callback(new GError(String(e)));
  }
}

export function onMessage(room, callback) {
  room.on_message((incoming) => {
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

/// Encode a BitArray (Uint8Array internally) as lowercase hex.
/// Used by the relays popover to render the relay's peer_id.
export function bitsToHex(bits) {
  const bytes = bitsToBytes(bits);
  return Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join("");
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

export async function startPresence(room, intervalMs, ttlMs, refreshMs) {
  try {
    await room.start_presence(intervalMs, ttlMs, refreshMs);
  } catch (e) {
    console.warn("startPresence failed", e);
  }
}

export function onMembersChanged(room, callback) {
  room.on_members_changed((members) => {
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
        last_heartbeat_ms: m.last_heartbeat_ms,  // f64; -1 sentinel for "no heartbeat"
      }));
      Array.from(members).forEach((m) => m.free());
      callback(toList(copied));
    } catch (e) {
      console.warn("onMembersChanged callback threw", e);
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
export function memLastHeartbeatMs(m) {
  // The wasm-bindgen getter returns `f64`, with `-1` sentinel for
  // "no heartbeat observed" (self or never-heard-from peer).
  // See `MemberJs::last_heartbeat_ms` on the Rust side for why this
  // shape (Option<u64> serializes to `bigint | undefined`, which
  // doesn't play well with Number arithmetic on the Gleam side).
  // Gleam pattern matches on `Some/None` instances — return the
  // actual constructors, not bare values.
  const v = m.last_heartbeat_ms;
  if (v < 0) return new None();
  return new Some(v);
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

export function onReceipt(room, callback) {
  room.on_receipt((incoming) => {
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

/// Schedule a recurring callback every `ms` milliseconds. Returns
/// nothing — there is no cancel handle in v1; the ticker runs for the
/// page lifetime. Use only for cheap, idempotent dispatches.
export function setIntervalMs(ms, callback) {
  setInterval(callback, ms);
}

/// Wall-clock unix-ms snapshot. Used by the popover ticker to update
/// the "heard from N seconds ago" readout between membership-tracker
/// emits.
export function nowMs() {
  return Date.now();
}

/// One-shot setTimeout wrapper. Used to stagger room-open calls at
/// startup so the Argon2id KDF cost doesn't block the page.
export function setTimeoutMs(ms, callback) {
  setTimeout(callback, ms);
}

// Per-room reactions FFI. `RoomHandle::send_reaction` generates its
// own nonce + sent_at_ms internally (was Client-level on master with
// caller-supplied entropy; multi-room moved it to RoomHandle).
export function onReactionsChanged(roomHandle, callback) {
  roomHandle.on_reactions_changed((payload) => {
    callback(payload);
  });
}

export function reactionsSnapshotTargetHex(snapshot) {
  return snapshot.target_hex;
}

export function reactionsSnapshotEntries(snapshot) {
  const out = [];
  for (const [emoji, set] of snapshot.reactions.entries()) {
    out.push([emoji, toList([...set])]);
  }
  return toList(out);
}

export function sendReaction(roomHandle, targetHex, emoji, action, callback) {
  roomHandle
    .send_reaction(targetHex, emoji, action)
    .then(() => callback(new Ok(undefined)))
    .catch((e) => callback(new GError(String(e?.message ?? e))));
}

export function clientPublicKeyHex(client) {
  const bytes = client.public_key;
  return [...bytes].map((b) => b.toString(16).padStart(2, "0")).join("");
}

let emojiPickerLoaded = null;
export function registerEmojiPicker() {
  if (!emojiPickerLoaded) {
    emojiPickerLoaded = import("emoji-picker-element");
  }
  return emojiPickerLoaded;
}

/// Read `?heartbeat_interval_ms=NNN` from the current URL. Returns 0
/// when absent or unparseable, signalling Client::new to use the
/// SyncConfig default (15 s). e2e-only knob.
export function heartbeatIntervalMsFromUrl() {
  const params = new URLSearchParams(window.location.search);
  const raw = params.get("heartbeat_interval_ms");
  if (raw === null) return 0;
  const n = Number(raw);
  return Number.isFinite(n) && n > 0 ? n : 0;
}
