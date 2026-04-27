// localStorage persistence + URL-hash routing.
//
// Two localStorage keys:
//   - `sunset-web/joined-rooms`: JSON array of room names (insertion
//     order; the rooms-rail renders them in this order).
//   - `sunset-web/last-used`: the room name the user last navigated
//     to. When the page loads with no URL fragment we use this to
//     redirect to the previous session's active room.

import { toList } from "../../prelude.mjs";

const ROOMS_KEY = "sunset-web/joined-rooms";
const LAST_USED_KEY = "sunset-web/last-used";

// Convert an iterable Gleam list to a JS array.
function listToArray(list) {
  return [...list];
}

function safeParseRooms(raw) {
  if (!raw) return [];
  try {
    const parsed = JSON.parse(raw);
    if (!Array.isArray(parsed)) return [];
    return parsed.filter((s) => typeof s === "string");
  } catch {
    return [];
  }
}

export function readJoinedRooms() {
  try {
    return toList(safeParseRooms(localStorage.getItem(ROOMS_KEY)));
  } catch {
    // localStorage can throw in private mode / disabled storage; fall
    // back to an empty list and don't propagate.
    return toList([]);
  }
}

export function writeJoinedRooms(rooms) {
  try {
    localStorage.setItem(ROOMS_KEY, JSON.stringify(listToArray(rooms)));
  } catch {
    // ignored: storage is best-effort.
  }
}

export function readLastUsed() {
  try {
    return localStorage.getItem(LAST_USED_KEY) || "";
  } catch {
    return "";
  }
}

export function writeLastUsed(name) {
  try {
    if (typeof name === "string" && name.length > 0) {
      localStorage.setItem(LAST_USED_KEY, name);
    } else {
      localStorage.removeItem(LAST_USED_KEY);
    }
  } catch {
    // ignored.
  }
}

export function readHash() {
  try {
    return decodeURIComponent((location.hash || "").replace(/^#/, ""));
  } catch {
    return "";
  }
}

export function setHash(name) {
  if (typeof name !== "string" || name.length === 0) {
    // Clear the fragment without a full navigation.
    history.replaceState(
      "",
      document.title,
      location.pathname + location.search,
    );
    return;
  }
  const encoded = "#" + encodeURIComponent(name);
  if (location.hash !== encoded) {
    location.hash = encoded;
  }
}

export function onHashChange(callback) {
  const handler = () => callback(readHash());
  window.addEventListener("hashchange", handler);
}
