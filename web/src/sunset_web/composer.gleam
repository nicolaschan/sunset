//// Thin wrappers around `composer.ffi.mjs`. Used by main_panel.

@external(javascript, "./composer.ffi.mjs", "autoGrow")
pub fn auto_grow(element_id: String) -> Nil

@external(javascript, "./composer.ffi.mjs", "resetTextarea")
pub fn reset_textarea(element_id: String) -> Nil

@external(javascript, "./composer.ffi.mjs", "applyTemplate")
pub fn apply_template(
  element_id: String,
  before: String,
  between: String,
  after: String,
  caret_at_between: Bool,
) -> String

/// Insert `text` into the textarea at the current cursor / selection.
/// Returns the resulting textarea value so the caller can write it
/// back into model state. Used by the composer emoji picker.
@external(javascript, "./composer.ffi.mjs", "insertAtCursor")
pub fn insert_at_cursor(element_id: String, text: String) -> String

@external(javascript, "./composer.ffi.mjs", "attachShortcutPreventDefault")
pub fn attach_shortcut_prevent_default(element_id: String) -> Nil

@external(javascript, "./composer.ffi.mjs", "focusTextarea")
pub fn focus_textarea(element_id: String) -> Nil

/// Install a global `visibilitychange` listener that re-focuses the
/// textarea with id `element_id` whenever the page becomes visible
/// again *and* nothing else is currently focused. Idempotent — calling
/// it again just rebinds the target id. No-op when the listener fires
/// with focus already held by an interactive element (button, input,
/// etc.) so we never steal focus from the user's actual interaction.
@external(javascript, "./composer.ffi.mjs", "installReturnAutofocus")
pub fn install_return_autofocus(element_id: String) -> Nil

/// Install a global paste listener that dispatches `(mime, base64)`
/// tuples for every image pasted into the textarea with id
/// `element_id`. The handler is idempotent — calling it again just
/// rebinds the callback. Non-image pastes fall through to the
/// browser default so plain-text paste keeps working.
@external(javascript, "./composer.ffi.mjs", "installImagePasteHandler")
pub fn install_image_paste_handler(
  element_id: String,
  callback: fn(List(#(String, String))) -> Nil,
) -> Nil
