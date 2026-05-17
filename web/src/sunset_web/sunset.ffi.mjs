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

export function setSelfName(client, name, callback) {
  // Empty string ⇒ clear the name (Rust normalizes whitespace + empty
  // to None; passing "" through is fine).
  client.set_self_name(name);
  callback();
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

export async function sendMessage(
  room,
  channel,
  body,
  images,
  sentAtMs,
  callback,
) {
  try {
    // `images` arrives as a Gleam List of #(mime_type, data_base64) tuples
    // packaged as 2-elem Gleam dynamic arrays. Convert to plain JS objects
    // before crossing the wasm boundary — see `room_handle.rs::images_from_js`.
    const imagesArray = gleamListToImageArray(images);
    const valueHashHex = await room.send_message(
      channel,
      body,
      imagesArray,
      sentAtMs,
    );
    callback(new Ok(valueHashHex));
  } catch (e) {
    callback(new GError(String(e)));
  }
}

/// Convert a Gleam `List(#(String, String))` of (mime, base64) pairs
/// into the `Array<{ mime_type, data_base64 }>` shape the wasm side
/// expects. Gleam's stdlib List exposes a `toArray()` view; each tuple
/// is materialized as an array-like with `[0]` = mime, `[1]` = base64.
function gleamListToImageArray(list) {
  return list.toArray().map((pair) => ({
    mime_type: pair[0],
    data_base64: pair[1],
  }));
}

export function onMessage(room, callback) {
  room.on_message((incoming) => {
    // Copy fields into a plain JS object so we can free the wasm-bindgen
    // wrapper immediately and avoid GC-delayed memory accumulation.
    // `images` is already a plain JS Array of `{ mime_type, data_base64 }`
    // objects (built in `messages.rs::images_to_js`), so we can keep
    // the reference directly.
    const plain = {
      author_pubkey: incoming.author_pubkey,
      epoch_id: incoming.epoch_id,
      sent_at_ms: incoming.sent_at_ms,
      channel: incoming.channel,
      body: incoming.body,
      value_hash_hex: incoming.value_hash_hex,
      is_self: incoming.is_self,
      images: incoming.images,
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

export function hexEncode(bits) {
  const bytes = bitsToBytes(bits);
  return Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join("");
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

/// HH:MM:SS — used by the message-details panel where second-level
/// precision matters for diffing delivery / reaction timestamps.
export function formatTimeMsExact(ms) {
  const d = new Date(ms);
  const hh = String(d.getHours()).padStart(2, "0");
  const mm = String(d.getMinutes()).padStart(2, "0");
  const ss = String(d.getSeconds()).padStart(2, "0");
  return `${hh}:${mm}:${ss}`;
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
export function incChannel(msg) { return msg.channel; }

/// Convert the message's `images` JS Array into a Gleam
/// `List(#(String, String))` of `(mime_type, data_base64)` pairs.
/// Each `<img>` rendered by the timeline picks its `src` as
/// `"data:" + mime_type + ";base64," + data_base64`.
export function incImages(msg) {
  const pairs = (msg.images ?? []).map((e) => [e.mime_type, e.data_base64]);
  return toList(pairs);
}

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
        name: m.name,                    // String | undefined; None when unset
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
export function memName(member) {
  const v = member.name;
  if (v === undefined || v === null || v === "") {
    return new None();
  }
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
      channel: incoming.channel,
      sent_at_ms: incoming.sent_at_ms,
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

export function recSentAtMs(rec) {
  return rec.sent_at_ms;
}

export function recChannel(rec) {
  return rec.channel;
}

// Sorted snapshot of channel-name strings observed in this room. Always
// includes "general". Returned as a Gleam List(String).
export function observedChannels(room) {
  return toList(Array.from(room.observed_channels()));
}

// Register a callback fired (immediately with the current snapshot,
// then on every change) with a Gleam List(String) of channel names.
export function onChannelsChanged(room, callback) {
  room.on_channels_changed((arr) => callback(toList(Array.from(arr))));
}

// Sorted snapshot of every Text message in the room, ordered by the
// sender-claimed `sent_at_ms` ascending (tie-broken on value-hash for
// stability). The wasm side hands back an `Array<IncomingMessage>`;
// `incomingToPlain` copies the fields out and frees each wasm wrapper
// so we don't hold cross-boundary pointers. Returned as a Gleam
// List(IncomingMessage).
export function orderedMessages(room) {
  const arr = Array.from(room.ordered_messages());
  return toList(arr.map(incomingToPlain));
}

// Register a callback fired (immediately with the current snapshot,
// then on every change) with a Gleam List(IncomingMessage) sorted by
// sender-claimed `sent_at_ms`. The bridge owns the sort so all client
// surfaces — Gleam UI, future TUI, etc. — render identical order.
export function onMessagesChanged(room, callback) {
  room.on_messages_changed((arr) => {
    const items = Array.from(arr).map(incomingToPlain);
    callback(toList(items));
  });
}

/// Shared helper: copy fields off a wasm-bindgen `IncomingMessage`
/// into a plain JS object and free the wrapper. Used by both
/// `onMessage` (single-message fire) and `onMessagesChanged` /
/// `orderedMessages` (snapshot fires) so the conversion stays in one
/// place.
function incomingToPlain(incoming) {
  const plain = {
    author_pubkey: incoming.author_pubkey,
    epoch_id: incoming.epoch_id,
    sent_at_ms: incoming.sent_at_ms,
    channel: incoming.channel,
    body: incoming.body,
    value_hash_hex: incoming.value_hash_hex,
    is_self: incoming.is_self,
    images: incoming.images,
  };
  incoming.free();
  return plain;
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

export function reactionsSnapshotChannel(snapshot) {
  return snapshot.channel;
}

export function reactionsSnapshotEntries(snapshot) {
  // Inner shape on the wasm side is `Map<author_pubkey_hex, sent_at_ms>`.
  // Flatten each emoji's Map to a Gleam-friendly `List(#(author_hex, sent_at_ms))`.
  const out = [];
  for (const [emoji, authors] of snapshot.reactions.entries()) {
    const pairs = [];
    for (const [authorHex, sentAtMs] of authors.entries()) {
      pairs.push([authorHex, sentAtMs]);
    }
    out.push([emoji, toList(pairs)]);
  }
  return toList(out);
}

export function sendReaction(roomHandle, channel, targetHex, emoji, action, callback) {
  roomHandle
    .send_reaction(channel, targetHex, emoji, action)
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

export function truncFloat(f) { return Math.trunc(f); }

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

/// Image attachment picker. The first time it's called we lazily create
/// a hidden `<input type="file" multiple accept="image/*">` and reuse
/// it for every subsequent open — Safari and Firefox both insist the
/// click come from a user-gesture handler, so the input has to live in
/// the DOM, not be created on demand inside a Promise.
let imagePickerInput = null;
function getImagePickerInput() {
  if (imagePickerInput && document.body.contains(imagePickerInput)) {
    return imagePickerInput;
  }
  const el = document.createElement("input");
  el.type = "file";
  el.multiple = true;
  // Browser-renderable raster formats only: jpeg, png, webp, gif.
  // HEIC is intentionally absent — the wasm preprocessor recognises
  // and rejects HEIC with a tailored error as a defence-in-depth
  // measure, but until we have a permissively-licensed HEIC decoder
  // wired in (see docs/superpowers/specs/2026-05-13-image-
  // preprocessing-design.md) we don't surface it in the picker.
  el.accept = "image/jpeg,image/png,image/webp,image/gif";
  el.setAttribute("data-testid", "composer-image-input");
  el.style.position = "fixed";
  el.style.left = "-9999px";
  el.style.top = "-9999px";
  el.style.width = "1px";
  el.style.height = "1px";
  el.style.opacity = "0";
  el.style.pointerEvents = "none";
  document.body.appendChild(el);
  imagePickerInput = el;
  return el;
}

/// Open the OS image picker. `callback` is invoked exactly once per
/// open: with a Gleam `List(#(String, String))` of `(mime_type,
/// data_base64)` pairs, possibly empty (user cancelled or picked
/// nothing). Each image is read via FileReader as a data URI and the
/// `data:.../;base64,` prefix is stripped.
export function pickImages(callback) {
  const input = getImagePickerInput();
  // Reset value so picking the same file twice in a row still fires
  // `change`. (Without this, the browser silently dedupes.)
  input.value = "";
  let done = false;
  const finish = (pairs) => {
    if (done) return;
    done = true;
    input.removeEventListener("change", onChange);
    input.removeEventListener("cancel", onCancel);
    callback(toList(pairs));
  };
  const onChange = async () => {
    const files = Array.from(input.files ?? []);
    console.info(`pickImages: change fired, files=${files.length}`);
    try {
      const pairs = await Promise.all(files.map(readImage));
      const valid = pairs.filter((p) => p !== null);
      finish(valid);
    } catch (e) {
      console.warn("pickImages: readImage failed", e);
      finish([]);
    }
  };
  const onCancel = () => {
    // iOS Safari has been observed firing `cancel` even after a
    // successful Photo Library selection. If files are actually
    // present, ignore the spurious cancel and let `change` handle it.
    const count = input.files ? input.files.length : 0;
    console.info(`pickImages: cancel fired, files=${count}`);
    if (count > 0) return;
    finish([]);
  };
  input.addEventListener("change", onChange);
  // `cancel` is the modern event when the user dismisses the picker
  // without selecting; older browsers just never fire `change`.
  input.addEventListener("cancel", onCancel);
  input.click();
}

const ALLOWED_IMAGE_TYPES = new Set([
  "image/jpeg",
  "image/png",
  "image/webp",
  "image/gif",
]);

/// Resolve an allowed MIME type for `file`, falling back to the
/// filename extension when `file.type` is empty or unrecognized. iOS
/// WebKit sometimes hands Photo Library picks back with `file.type =
/// ""` (Live Photos, edited photos, certain iOS versions), so trusting
/// `file.type` alone silently drops valid images on iPhone.
function inferImageType(file) {
  if (ALLOWED_IMAGE_TYPES.has(file.type)) return file.type;
  const name = (file.name || "").toLowerCase();
  const dot = name.lastIndexOf(".");
  if (dot < 0) return null;
  const ext = name.slice(dot + 1);
  if (ext === "jpg" || ext === "jpeg") return "image/jpeg";
  if (ext === "png") return "image/png";
  if (ext === "webp") return "image/webp";
  if (ext === "gif") return "image/gif";
  return null;
}

function readImage(file) {
  const type = inferImageType(file);
  if (type === null) {
    console.warn(
      `pickImages: skipping ${file.name} (type=${file.type}, size=${file.size})`,
    );
    return Promise.resolve(null);
  }
  return new Promise((resolve, reject) => {
    const fr = new FileReader();
    fr.onload = () => {
      const result = fr.result;
      if (typeof result !== "string") {
        resolve(null);
        return;
      }
      // result is `"data:<mime>;base64,<payload>"` — strip the prefix.
      const comma = result.indexOf(",");
      if (comma < 0) {
        resolve(null);
        return;
      }
      resolve([type, result.slice(comma + 1)]);
    };
    fr.onerror = () => reject(fr.error);
    fr.readAsDataURL(file);
  });
}

/// Programmatically build a data URL from a `(mime_type, data_base64)`
/// Gleam tuple. Used by the timeline renderer so the Gleam side doesn't
/// have to concatenate strings byte-by-byte at render time.
export function imageDataUrl(mimeType, dataBase64) {
  return `data:${mimeType};base64,${dataBase64}`;
}
