// Auto-scroll the chat messages list to the bottom when new content
// arrives, but only if the user hasn't scrolled up to read history.
//
// `wasAtBottom` is updated only on USER-driven events (wheel,
// touchmove, scroll-control keys). Scroll events from programmatic
// scrolls and content reflow are ignored. This avoids a subtle race
// during message bursts where content can grow between our
// programmatic `scrollTop = scrollHeight` and the resulting scroll
// event firing — a passive scroll listener would see the new larger
// scrollHeight against the now-stale scrollTop and conclude the user
// had moved away from the bottom, latching wasAtBottom to false for
// the rest of the burst.
//
// The trade-off: dragging the scrollbar (no wheel/touch event) won't
// update wasAtBottom. Scrollbar drags on a chat scroll area are
// uncommon; acceptable for v1.
//
// The chat shell mounts/unmounts as the user navigates between
// landing and rooms, so a body-level MutationObserver re-attaches
// each time a fresh `.scroll-area` element appears.

const NEAR_BOTTOM_PX = 80;
const SCROLL_KEYS = new Set([
  "PageUp",
  "PageDown",
  "Home",
  "End",
  "ArrowUp",
  "ArrowDown",
]);

let attachedEl = null;
let wasAtBottom = true;
let onUserScroll = null;
let onKeyDown = null;
let innerObserver = null;

function isNearBottom(el) {
  return el.scrollHeight - (el.scrollTop + el.clientHeight) <= NEAR_BOTTOM_PX;
}

function detach() {
  if (innerObserver) {
    innerObserver.disconnect();
    innerObserver = null;
  }
  if (attachedEl) {
    if (onUserScroll) {
      attachedEl.removeEventListener("wheel", onUserScroll);
      attachedEl.removeEventListener("touchmove", onUserScroll);
    }
    if (onKeyDown) attachedEl.removeEventListener("keydown", onKeyDown);
  }
  attachedEl = null;
  onUserScroll = null;
  onKeyDown = null;
}

function attach(el) {
  if (el === attachedEl) return;
  detach();

  attachedEl = el;
  wasAtBottom = true;
  el.scrollTop = el.scrollHeight;

  onUserScroll = () => {
    wasAtBottom = isNearBottom(el);
  };
  el.addEventListener("wheel", onUserScroll, { passive: true });
  el.addEventListener("touchmove", onUserScroll, { passive: true });

  onKeyDown = (event) => {
    if (SCROLL_KEYS.has(event.key)) {
      // Wait one frame for the key's scroll effect to land, then
      // re-check from the post-scroll geometry.
      requestAnimationFrame(() => {
        if (attachedEl === el) wasAtBottom = isNearBottom(el);
      });
    }
  };
  el.addEventListener("keydown", onKeyDown);

  innerObserver = new MutationObserver(() => {
    if (!document.contains(el)) {
      detach();
      return;
    }
    if (!wasAtBottom) return;
    // Defer one frame so layout has settled — otherwise scrollHeight
    // can be stale on the same tick the message lands.
    requestAnimationFrame(() => {
      if (attachedEl === el) el.scrollTop = el.scrollHeight;
    });
  });
  innerObserver.observe(el, { childList: true, subtree: true });
}

export function attachChatScrollAnchor() {
  const SELECTOR = ".scroll-area";
  const find = () => document.querySelector(SELECTOR);

  const initial = find();
  if (initial) attach(initial);

  const bodyObserver = new MutationObserver(() => {
    const cur = find();
    if (cur && cur !== attachedEl) attach(cur);
    if (!cur && attachedEl) detach();
  });
  bodyObserver.observe(document.body, { childList: true, subtree: true });
}
