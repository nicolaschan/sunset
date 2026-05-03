//// Thin wrappers around `composer.ffi.mjs`. Used by main_panel.

@external(javascript, "./composer.ffi.mjs", "autoGrow")
pub fn auto_grow(element_id: String) -> Nil
