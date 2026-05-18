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

export function toPlain(body) {
  if (wasmModule && wasmModule.to_plain_markdown) {
    try {
      return wasmModule.to_plain_markdown(body);
    } catch (err) {
      console.error("markdown.toPlain WASM call failed:", err);
    }
  }
  return body;
}

// Thin bridge to `sunset_markdown::emoji_only_count` via the WASM
// bundle. The Rust function owns the policy (grapheme cluster
// iteration via `unicode-segmentation`, base-codepoint
// `EmojiStatus` discrimination via `unicode-properties`) so all
// clients see the same classification — the TUI / native shells will
// share this same logic when they read messages off the Rust core.
//
// When the WASM bundle isn't loaded (Node `gleam test` env), the
// function returns 0, which means "Normal" rendering — a safe
// fallback for the unit-test environment where the WASM bundle isn't
// present. The Playwright e2e suite exercises the real Rust → WASM
// → Gleam → DOM path.
export function emojiOnlyCount(body) {
  if (typeof body !== "string") return 0;
  if (wasmModule && wasmModule.emoji_only_count) {
    try {
      return wasmModule.emoji_only_count(body);
    } catch (err) {
      console.error("markdown.emojiOnlyCount WASM call failed:", err);
    }
  }
  return 0;
}
