//// Touch-driven drag-drop helper. Wires `pointerdown`/`pointermove`/
//// `pointerup` against rows marked `data-room-row="<name>"`, with a
//// 400ms long-press to enter drag mode. Mouse pointers are ignored
//// (desktop already handles HTML5 drag events).

pub type Callbacks {
  Callbacks(
    on_start: fn(String) -> Nil,
    on_over: fn(String) -> Nil,
    on_drop: fn(String) -> Nil,
    on_end: fn() -> Nil,
  )
}

@external(javascript, "./touch_drag.ffi.mjs", "attach")
pub fn attach(callbacks: Callbacks) -> Nil
