// Composer DOM helpers — auto-grow on input + selection-aware
// template insertion for keyboard shortcuts.

import { toList } from "../../prelude.mjs";

const MAX_LINES = 10;

export function autoGrow(elementId) {
  const el = document.getElementById(elementId);
  if (!el || el.tagName !== "TEXTAREA") return;
  // Reset, measure, cap.
  el.style.height = "auto";
  const lineHeight = parseFloat(getComputedStyle(el).lineHeight) || 20;
  const maxHeight = lineHeight * MAX_LINES;
  el.style.height = Math.min(el.scrollHeight, maxHeight) + "px";
  el.style.overflowY = el.scrollHeight > maxHeight ? "auto" : "hidden";
}

// Called after a submit-clears-draft cycle. We can't just call
// autoGrow here: Lustre commits the new value="" on its next render
// pass, and the effect that calls us runs *before* that — so
// scrollHeight would still report the just-sent multi-line height.
// Clear the value imperatively (Lustre will idempotently re-apply
// "" on its render) and drop the inline `style.height` so the
// textarea's CSS-declared 1-line height takes over.
export function resetTextarea(elementId) {
  const el = document.getElementById(elementId);
  if (!el || el.tagName !== "TEXTAREA") return;
  el.value = "";
  el.style.height = "";
  el.style.overflowY = "hidden";
}

export function applyTemplate(elementId, before, between, after, caretAtBetween) {
  const el = document.getElementById(elementId);
  if (!el || el.tagName !== "TEXTAREA") return el ? el.value : "";
  const start = el.selectionStart;
  const end = el.selectionEnd;
  const selected = el.value.slice(start, end);
  const middle = selected.length > 0 ? selected : between;
  const replacement = before + middle + after;
  el.value = el.value.slice(0, start) + replacement + el.value.slice(end);
  // Place caret.
  const caret =
    caretAtBetween
      ? start + before.length + middle.length
      : start + before.length;
  el.selectionStart = caret;
  el.selectionEnd = caret;
  // Single source of truth: ApplyComposerShortcut writes the returned
  // value into model.draft. We don't dispatch a synthetic input event.
  // We do trigger auto-grow ourselves since on_input won't fire.
  autoGrow(elementId);
  el.focus();
  return el.value;
}

// Move focus to the textarea and place the caret at the end of its
// current value. Used after channel / room switches so the user can
// start typing immediately. No-op if the element is missing or not a
// textarea (so the caller doesn't need to guard).
export function focusTextarea(elementId) {
  const el = document.getElementById(elementId);
  if (!el || el.tagName !== "TEXTAREA") return;
  const len = el.value.length;
  // Defer to the next frame so any pending Lustre re-render (which can
  // re-create the element) doesn't blow away the focus we just set.
  requestAnimationFrame(() => {
    try {
      el.focus({ preventScroll: true });
      el.selectionStart = len;
      el.selectionEnd = len;
    } catch {
      // ignored: focus is best-effort.
    }
  });
}

export function attachShortcutPreventDefault(elementId) {
  const el = document.getElementById(elementId);
  if (!el) return;
  el.addEventListener("keydown", (ev) => {
    const mod = ev.metaKey || ev.ctrlKey;
    if (!mod) return;
    const key = ev.key.toLowerCase();
    if (key === "b" || key === "i" || key === "k") {
      ev.preventDefault();
    }
  });
}

const PASTE_ALLOWED_IMAGE_TYPES = new Set([
  "image/jpeg",
  "image/png",
  "image/webp",
  "image/gif",
]);

let imagePasteCallback = null;
let imagePasteTargetId = null;
let imagePasteInstalled = false;

// Install a global paste listener that fires `callback` with a Gleam
// `List(#(mime, base64))` whenever the user pastes one or more image
// files into the composer textarea (id == `elementId`). Non-image
// pastes fall through to the browser default (so plain-text paste
// keeps working). The listener is global + idempotent: re-invocations
// just rebind the callback / target id, so the model can re-register
// after a room switch without leaking duplicate listeners.
export function installImagePasteHandler(elementId, callback) {
  imagePasteTargetId = elementId;
  imagePasteCallback = callback;
  if (imagePasteInstalled) return;
  imagePasteInstalled = true;
  document.addEventListener("paste", onPasteEvent, true);
}

function onPasteEvent(ev) {
  const target = ev.target;
  if (!target || target.id !== imagePasteTargetId) return;
  const cd = ev.clipboardData;
  if (!cd) return;
  // Walk every clipboard item — a screenshot copy on Chromium produces
  // exactly one `kind: "file"` item with `type: "image/png"`; other
  // tools may include an HTML representation alongside, which we
  // ignore. We only consume entries whose MIME is one of our allowed
  // raster types so a copied PDF or video can't sneak through.
  const files = [];
  const items = cd.items || [];
  for (const it of items) {
    if (it.kind !== "file") continue;
    const f = it.getAsFile();
    if (!f) continue;
    if (PASTE_ALLOWED_IMAGE_TYPES.has(f.type)) files.push(f);
  }
  if (files.length === 0) return;
  // Suppress the browser default so the image bytes don't get pasted
  // into the textarea as base64 / a stale data URI string.
  ev.preventDefault();
  const cb = imagePasteCallback;
  if (!cb) return;
  Promise.all(files.map(readPastedImage)).then((results) => {
    const valid = results.filter((p) => p !== null);
    if (valid.length > 0) cb(toList(valid));
  });
}

function readPastedImage(file) {
  return new Promise((resolve) => {
    const fr = new FileReader();
    fr.onload = () => {
      const result = fr.result;
      if (typeof result !== "string") {
        resolve(null);
        return;
      }
      const comma = result.indexOf(",");
      if (comma < 0) {
        resolve(null);
        return;
      }
      resolve([file.type, result.slice(comma + 1)]);
    };
    fr.onerror = () => resolve(null);
    fr.readAsDataURL(file);
  });
}
