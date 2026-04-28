// Touch-driven drag-and-drop for room rows. HTML5 drag events don't
// fire reliably on touch, so we build a parallel path:
//   pointerdown (touch only) → 400ms hold timer → drag mode.
//   pointermove → hit-test against [data-room-row=name] → over callback.
//   pointerup → drop callback.
//   pointercancel / scroll → cancel timer.

const HOLD_MS = 400;

export function attach(callbacks) {
  const onStart = callbacks.on_start;
  const onOver = callbacks.on_over;
  const onDrop = callbacks.on_drop;
  const onEnd = callbacks.on_end;

  let timer = null;
  let active = null;
  let lastTarget = null;

  function rowNameAt(x, y) {
    const el = document.elementFromPoint(x, y);
    if (!el) return null;
    const row = el.closest("[data-room-row]");
    return row ? row.getAttribute("data-room-row") : null;
  }

  function reset() {
    if (timer) {
      clearTimeout(timer);
      timer = null;
    }
    active = null;
    lastTarget = null;
  }

  function handleDown(e) {
    if (e.pointerType !== "touch") return;
    const startName = rowNameAt(e.clientX, e.clientY);
    if (!startName) return;
    timer = setTimeout(() => {
      timer = null;
      active = startName;
      onStart(active);
    }, HOLD_MS);
  }

  function handleMove(e) {
    if (e.pointerType !== "touch") return;
    if (!active) {
      if (timer) {
        clearTimeout(timer);
        timer = null;
      }
      return;
    }
    const target = rowNameAt(e.clientX, e.clientY);
    if (target && target !== lastTarget) {
      lastTarget = target;
      onOver(target);
    }
  }

  function handleUp(e) {
    if (e.pointerType !== "touch") return;
    if (!active) {
      reset();
      return;
    }
    const target = rowNameAt(e.clientX, e.clientY);
    if (target) onDrop(target);
    onEnd();
    reset();
  }

  function handleCancel(e) {
    if (e.pointerType !== "touch") return;
    if (active) onEnd();
    reset();
  }

  document.addEventListener("pointerdown", handleDown, { passive: true });
  document.addEventListener("pointermove", handleMove, { passive: true });
  document.addEventListener("pointerup", handleUp, { passive: true });
  document.addEventListener("pointercancel", handleCancel, { passive: true });
  window.addEventListener(
    "scroll",
    () => {
      if (timer) {
        clearTimeout(timer);
        timer = null;
      }
    },
    { passive: true, capture: true },
  );
}
