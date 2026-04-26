//// Small UI helpers shared across views.
////
//// Lustre's `attribute.style` takes `List(#(String, String))` (or a single
//// string in newer versions). We standardise on the list-of-tuples shape
//// and wrap a few common patterns.

import gleam/int
import gleam/list
import gleam/string
import lustre/attribute.{type Attribute}

/// Compose a `style` attribute from `(property, value)` pairs.
pub fn css(rules: List(#(String, String))) -> Attribute(msg) {
  attribute.attribute(
    "style",
    rules
      |> list.map(fn(pair) {
        let #(k, v) = pair
        k <> ": " <> v
      })
      |> string.join("; "),
  )
}

/// Pixel string from an int.
pub fn px(n: Int) -> String {
  int.to_string(n) <> "px"
}

/// Conditionally apply a list of style rules. The trailing list wins on
/// duplicate keys (kept ordering: base first, override second).
pub fn css_if(
  cond: Bool,
  base: List(#(String, String)),
  override: List(#(String, String)),
) -> Attribute(msg) {
  case cond {
    True -> css(list.append(base, override))
    False -> css(base)
  }
}
