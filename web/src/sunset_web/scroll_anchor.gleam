//// Auto-scroll-to-bottom for the chat messages list. Implementation
//// lives in `scroll_anchor.ffi.mjs`; this module is a thin extern.

/// Attach a one-time global handler that keeps the chat scroll area
/// pinned to the bottom whenever new messages arrive — unless the
/// user has scrolled up to read history, in which case the view stays
/// put. Idempotent: call once at app startup.
@external(javascript, "./scroll_anchor.ffi.mjs", "attachChatScrollAnchor")
pub fn attach_chat_scroll_anchor() -> Nil
