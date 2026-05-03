//// Render a message body string to a Lustre element by parsing it as
//// Discord-flavored markdown. The parser lives in the Rust
//// `sunset-markdown` crate and is reached through `markdown.ffi.mjs`.
////
//// The AST shape is an opaque JS value from Gleam's perspective. We
//// treat it as `dynamic.Dynamic` and decode it with the
//// `gleam/dynamic/decode` API used elsewhere in the codebase.

import gleam/dynamic.{type Dynamic}
import lustre/element.{type Element}
import lustre/element/html

import sunset_web/theme.{type Palette}

@external(javascript, "./markdown.ffi.mjs", "parseMarkdown")
fn parse_markdown_ffi(body: String) -> Dynamic

/// Render the body to a single block-container `Element`. Subsequent
/// tasks fill in the block/inline cases — for now everything renders
/// as plain text.
pub fn render(body: String, _p: Palette) -> Element(msg) {
  let _ast = parse_markdown_ffi(body)
  // Phase C1: stub. Replaced by Tasks C2..C5.
  html.div([], [html.text(body)])
}

/// Strip all formatting and return concatenated text. Useful for
/// notification bodies and `aria-label`s.
pub fn to_plain(body: String) -> String {
  // Phase C1: stub. Replaced by Task C6.
  body
}
