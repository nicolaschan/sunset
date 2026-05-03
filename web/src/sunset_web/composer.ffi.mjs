// Composer DOM helpers — auto-grow on input + selection-aware
// template insertion for keyboard shortcuts.

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
