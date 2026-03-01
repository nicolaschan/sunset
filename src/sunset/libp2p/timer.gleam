@external(javascript, "./timer.ffi.mjs", "set_timeout")
pub fn set_timeout(_callback: fn() -> Nil, _ms: Int) -> Nil {
  Nil
}
