@external(javascript, "./dial.ffi.mjs", "dial_multiaddr")
pub fn dial_multiaddr(
  _addr: String,
  _callback: fn(Result(Nil, String)) -> Nil,
) -> Nil {
  Nil
}
