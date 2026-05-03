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

@external(javascript, "./composer.ffi.mjs", "attachShortcutPreventDefault")
pub fn attach_shortcut_prevent_default(element_id: String) -> Nil
