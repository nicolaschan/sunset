export function set_hash(hash) {
  window.location.hash = "#" + hash;
}

export function get_hash() {
  const h = window.location.hash;
  // Strip leading "#"
  return h.startsWith("#") ? h.slice(1) : "";
}

export function clear_hash() {
  // Remove hash without triggering a scroll — use replaceState
  history.replaceState(null, "", window.location.pathname + window.location.search);
}

export function on_hash_change(callback) {
  window.addEventListener("hashchange", () => {
    callback(get_hash());
  });
}

// -- localStorage --

const DISPLAY_NAME_KEY = "sunset:displayName";

export function get_saved_display_name() {
  try {
    return localStorage.getItem(DISPLAY_NAME_KEY) || "";
  } catch {
    return "";
  }
}

export function save_display_name(name) {
  try {
    if (name) {
      localStorage.setItem(DISPLAY_NAME_KEY, name);
    } else {
      localStorage.removeItem(DISPLAY_NAME_KEY);
    }
  } catch {
    // localStorage may be unavailable (private browsing, etc.)
  }
}
