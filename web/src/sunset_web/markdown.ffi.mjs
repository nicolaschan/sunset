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
// Dynamic import: in `gleam test` (Node, no WASM bundle built) the WASM
// module isn't present. Dynamic import lets the module load anyway and
// fall back to literal-text rendering. Unit tests for the renderer don't
// go through this path (they call `render_blocks` with hand-built AST);
// the full pipeline is covered by Playwright e2e.

let wasmModule = null;
try {
  wasmModule = await import("../../sunset_web_wasm.js");
} catch (_err) {
  // WASM bundle not built (test env or pre-build). parseMarkdown will
  // return the literal-text fallback below.
}

export function parseMarkdown(body) {
  if (wasmModule && wasmModule.parse_markdown) {
    try {
      return wasmModule.parse_markdown(body);
    } catch (err) {
      console.error("markdown.parseMarkdown WASM call failed:", err);
    }
  }
  return [{ Paragraph: [{ Text: body }] }];
}
