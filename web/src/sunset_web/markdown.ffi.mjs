// Bridges sunset-web-wasm's parse_markdown to Gleam.
//
// `Document(pub Vec<Block>)` is a serde transparent newtype, so the
// JSON shape is just a JS array of blocks: [block, block, ...].
//
// Each Block is one of (externally tagged):
//
//   { Paragraph: [inline, ...] }
//   { Heading: { level: "H1"|"H2"|"H3", content: [inline, ...] } }
//   { Quote: [block, ...] }
//   { UnorderedList: [[block, ...], ...] }
//   { CodeBlock: { language: string|null, source: string } }
//
// Each Inline is one of:
//
//   { Text: "..." } | { Bold: [...] } | { Italic: [...] }
//   | { Underline: [...] } | { Strikethrough: [...] } | { Spoiler: [...] }
//   | { InlineCode: "..." }
//   | { Link: { label: [inline, ...], url: "...", autolink: true|false } }
//   | "LineBreak"   // unit variant — serialized as a bare string
//
// On any error (e.g. WASM not yet loaded), returns the body as a single
// Paragraph(Text(body)) so rendering still produces something useful.

import { parse_markdown } from "../../sunset_web_wasm.js";

export function parseMarkdown(body) {
  try {
    return parse_markdown(body);
  } catch (err) {
    console.error("markdown.parseMarkdown failed:", err);
    return [{ Paragraph: [{ Text: body }] }];
  }
}
