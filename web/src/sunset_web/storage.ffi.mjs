// localStorage persistence + URL-hash routing.
//
// Two localStorage keys:
//   - `sunset-web/joined-rooms`: JSON array of room names. The order
//     is user-controlled (drag-drop in the rooms rail). The first
//     entry is treated as the default room when the page loads at
//     `/` with no URL fragment.
//   - `sunset-web/theme`: "light" or "dark". Set once the user
//     explicitly toggles the theme; until then we follow the OS via
//     prefers-color-scheme.

import { toList } from "../../prelude.mjs";

const ROOMS_KEY = "sunset-web/joined-rooms";
const THEME_KEY = "sunset-web/theme";

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

// Theme preference: "" when no explicit user choice has been made.
export function readSavedTheme() {
  try {
    const v = localStorage.getItem(THEME_KEY);
    return v === "light" || v === "dark" ? v : "";
  } catch {
    return "";
  }
}

export function writeSavedTheme(value) {
  try {
    if (value === "light" || value === "dark") {
      localStorage.setItem(THEME_KEY, value);
    } else {
      localStorage.removeItem(THEME_KEY);
    }
  } catch {
    // ignored.
  }
}

// True if the OS / browser is currently advertising a dark colour
// scheme via the prefers-color-scheme media query. Used as the
// fallback when the user hasn't picked a theme yet.
export function prefersDark() {
  try {
    return (
      typeof window.matchMedia === "function" &&
      window.matchMedia("(prefers-color-scheme: dark)").matches
    );
  } catch {
    return false;
  }
}

// Phone vs desktop is gated on a single CSS-media-query equivalent.
// Returns a fresh boolean each call so the caller doesn't need to
// hold a reference to the MediaQueryList.
export function isPhoneViewport() {
  try {
    return (
      typeof window.matchMedia === "function" &&
      window.matchMedia("(max-width: 767px)").matches
    );
  } catch {
    return false;
  }
}

// Subscribes `callback(isPhone: bool)` to viewport changes via
// MediaQueryList.addEventListener. Fires once for each crossing of
// the 768px boundary; not on every resize.
export function onViewportChange(callback) {
  try {
    if (typeof window.matchMedia !== "function") return;
    const mql = window.matchMedia("(max-width: 767px)");
    const handler = (e) => callback(e.matches);
    // addEventListener is the modern API; older Safari needs addListener.
    if (typeof mql.addEventListener === "function") {
      mql.addEventListener("change", handler);
    } else if (typeof mql.addListener === "function") {
      mql.addListener(handler);
    }
  } catch {
    // best-effort: viewport tracking is non-critical.
  }
}

// Override the default viewport meta tag with one that:
//   * cover: enables env(safe-area-inset-*) under iOS notch / dynamic island.
//   * interactive-widget=resizes-content: tells iOS/Android to resize the
//     layout viewport (not just the visual viewport) when the keyboard
//     opens, so position:fixed footers/composers don't get covered.
export function installMobileViewportMeta() {
  try {
    const existing = document.querySelectorAll('meta[name="viewport"]');
    existing.forEach((el) => el.remove());
    const meta = document.createElement("meta");
    meta.setAttribute("name", "viewport");
    meta.setAttribute(
      "content",
      "width=device-width, initial-scale=1, viewport-fit=cover, interactive-widget=resizes-content",
    );
    document.head.appendChild(meta);
  } catch {
    // ignored: best-effort.
  }
}
