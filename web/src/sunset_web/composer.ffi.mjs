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
