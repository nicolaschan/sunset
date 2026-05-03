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
