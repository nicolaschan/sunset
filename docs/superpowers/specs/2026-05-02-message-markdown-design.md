# Message markdown — design

**Date:** 2026-05-02
**Status:** Draft

## Goal

Render formatted message bodies in sunset.chat using a Discord-flavored
markdown subset. Messages keep their existing wire shape (a single
`String` body); each client parses on render. Web ships first; the
shared parser is built so the TUI and Minecraft mod can adopt it
without redesign when those crates land.

## Non-goals

- Syntax highlighting in code blocks.
- `@mentions` (waits on the identity-resolution spec).
- Custom emoji `:shortcode:`, URL unfurls, link previews.
- TUI and Minecraft-mod renderers (parser ships; renderers wait for
  those crates).
- CommonMark / GFM compatibility — Discord's grammar diverges from
  both, and we match Discord's behavior, not the spec.

## Syntax subset

Inline:

| Syntax | Meaning |
| --- | --- |
| `**text**` | bold |
| `*text*` or `_text_` | italic |
| `__text__` | underline |
| `~~text~~` | strikethrough |
| `\|\|text\|\|` | spoiler |
| `` `text` `` | inline code |
| `[label](url)` | masked link |
| `https://…`, `http://…` | autolinked URL |

Block:

| Syntax | Meaning |
| --- | --- |
| ```` ```lang\ncode\n``` ```` | fenced code block, language optional |
| `> text` | single-line block quote |
| `>>> text` | block quote to end of message |
| `- item` | unordered list item |
| `# `, `## `, `### ` | h1/h2/h3 |

A single newline within a paragraph emits an `Inline::LineBreak`
node and renders as `<br>`. A blank line ends the current block.

Inside a fenced or inline code span, no markdown is parsed: literal
`**` shows as `**`.

## Wire format

The message body remains `body: String` in `domain.gleam` and in the
underlying signed entry. Markdown source is the wire format. No
schema migration. Content addressing is unaffected — the same body
always hashes the same regardless of how the rendering rules evolve.

## Components

### `crates/sunset-markdown` (new)

Pure parser library. No rendering, no WASM bindings, no I/O. Lints
inherit the workspace defaults (`unsafe_code = deny`, etc.). Compiles
to `wasm32-unknown-unknown`.

**Public API:**

```rust
pub fn parse(input: &str) -> Document;
pub fn to_plain(doc: &Document) -> String;
```

**AST:**

```rust
pub struct Document(pub Vec<Block>);

pub enum Block {
    Paragraph(Vec<Inline>),
    Heading { level: HeadingLevel, content: Vec<Inline> },
    Quote(Vec<Block>),
    UnorderedList(Vec<Vec<Block>>),
    CodeBlock { language: Option<String>, source: String },
}

pub enum HeadingLevel { H1, H2, H3 }

pub enum Inline {
    Text(String),
    Bold(Vec<Inline>),
    Italic(Vec<Inline>),
    Underline(Vec<Inline>),
    Strikethrough(Vec<Inline>),
    Spoiler(Vec<Inline>),
    InlineCode(String),
    Link { label: Vec<Inline>, url: String, autolink: bool },
    LineBreak,
}
```

`autolink: true` distinguishes bare-URL detection from explicit
masked links — the renderer uses it for the tooltip rule (autolinks
don't need a `title=` since the visible text is already the URL).

**Parsing model:** hand-written single-pass tokenizer + recursive
inline parser. Total: malformed input degrades to literal text rather
than erroring. Expect ~600–800 lines including tests.

`pulldown-cmark` is rejected: its rules diverge from Discord's
(underline/bold collision on `__`, strikethrough syntax, list
parsing) in ways that would require a custom adapter big enough to
not be worth the dependency.

URL scheme filtering is **not** done in the parser — the AST stores
the raw URL string. Allowlist enforcement is the renderer's job
(keeps the AST round-trippable for tests).

**Cargo features:**

- `serde` (off by default) — derives `Serialize`/`Deserialize` on
  `Document`, `Block`, `Inline`, `HeadingLevel`. Enabled by
  `sunset-core-wasm` for the JS bridge. Not needed by native
  consumers (TUI, mod).

**Tests:**

- Golden-file tests under `crates/sunset-markdown/tests/golden/`:
  input string → expected debug-printed AST. Cover the tricky cases:
  `*foo_bar*`, `**a *b* c**`, `||spoiler with **bold**||`, fenced
  code containing backticks, autolinks adjacent to punctuation,
  nested quotes, list items spanning multiple paragraphs.
- Property tests (`proptest` or `quickcheck`): `parse` never panics
  on arbitrary UTF-8; `to_plain(parse(s)).len() <= s.len()`.

### `crates/sunset-core-wasm` (modify)

Adds one exported function:

```rust
#[wasm_bindgen]
pub fn parse_markdown(input: &str) -> JsValue {
    let doc = sunset_markdown::parse(input);
    serde_wasm_bindgen::to_value(&doc).unwrap()
}
```

New deps: `sunset-markdown` (with `serde` feature), `serde`,
`serde-wasm-bindgen`. The crate already targets WASM; no other
changes.

### `web/src/sunset_web/markdown.gleam` (new)

```gleam
pub fn render(body: String, p: Palette) -> Element(msg)
pub fn to_plain(body: String) -> String
```

`render` calls `parse_markdown` via FFI in `sunset.ffi.mjs`, walks the
returned JSON AST, and returns a single `html.div` containing the
rendered blocks. Replaces the `html.text(m.body)` call at
`web/src/sunset_web/views/main_panel.gleam:292`.

`to_plain` is a Gleam-side AST walk used for places that need stripped
text (notification bodies, tab titles, `aria-label` previews).

**Renderer rules:**

| AST node | Rendered as |
| --- | --- |
| `Bold` | `<strong>` |
| `Italic` | `<em>` |
| `Underline` | `<u>` |
| `Strikethrough` | `<s>` |
| `Spoiler` | `<span class="spoiler" data-msg-id=… data-offset=…>…</span>` with click handler |
| `InlineCode` | `<code>` with monospace font + subtle background |
| `CodeBlock` | `<pre><code>` panel; if `language` is set, a small uppercased pill in the top-right corner |
| `Link` | see below |
| `Quote` | `<blockquote>` with left border + indent |
| `Heading` | `<h1>`/`<h2>`/`<h3>` with scaled font; no anchors |
| `LineBreak` | `<br>` |

Semantic elements are deliberate (over `<span style=…>`): screen readers
get correct affordances for free, browsers' native `Cmd+F`-style search
behaves as expected, and the e2e test in §Testing matches.

**Link rendering:** scheme allowlist is `["http", "https", "mailto"]`.
Anything else renders as plain text showing the literal URL (so the
user can still see what was sent). For allowed schemes:

- `<a href="…" target="_blank" rel="noopener noreferrer">`
- `title=` attribute set to the resolved URL — masked links can't lie
  about destination
- Autolinks (`autolink: true` on the AST node) skip the `title=` since
  the visible text already is the URL.

No interstitial confirm dialog.

**Spoiler state:** revealed-spoiler IDs live on the top-level model in
`sunset_web` as `Set(#(message_id, offset))`. Click handler dispatches
`ToggleSpoiler(message_id, offset)`. Resets when the user navigates
away from the room (so re-entering hides spoilers again).

**Sanitization:** the renderer composes Lustre nodes directly. No
`innerHTML`, no raw HTML in the AST, no path through which parser
output can become attributes other than `href`/`title`/`data-*`.

### Composer (modify `views/main_panel.gleam`)

In the existing `composer` fn:

- `html.input` → `html.textarea` with `rows="1"`. Auto-grow hook lives
  in a new `composer.ffi.mjs`: on `input`, set `style.height` to
  `scrollHeight`, capped at 10 line-heights. Above the cap the
  textarea scrolls.
- `Enter` sends; `Shift+Enter` inserts a newline. Implemented in the
  existing keydown handling.
- `Cmd/Ctrl+B` / `Cmd/Ctrl+I` wrap the current selection with `**…**`
  / `*…*`. With no selection, insert the markers and place the caret
  between them.
- `Cmd/Ctrl+K` builds a link. With a selection, replaces it with
  `[selection](url)` and places the caret between the parens (so the
  user can paste/type the URL). With no selection, inserts `[](url)`
  and places the caret between the brackets.
- All three shortcuts go through a single FFI helper
  `composer_apply_template(textarea, before, between, after, caret)`
  that updates `value` and re-fires `input` so Gleam state stays in
  sync.
- Existing iOS no-zoom rule (`font-size: 16px`) and mobile-safe-area
  rules carry over — `textarea` is included in the `input, textarea,
  select { font-size: 16px; }` block at `shell.gleam:287`.

## Data flow

```
user types in composer
   → on_input -> Gleam model.draft (raw markdown string)
   → on submit, send_message(client, body, ts, …)
   → wire format: same as today, body is raw markdown

receiver renders:
   message stream calls markdown.render(m.body, p)
     → FFI: parse_markdown(body)
       → sunset-core-wasm exports
         → sunset-markdown::parse
       → returns AST as JSON (via serde-wasm-bindgen)
     → Gleam AST walk → Lustre Element
```

Each client re-parses every render. Parsing a typical chat message
(under a few hundred bytes) is microsecond-scale; no caching needed
for v1. If profiling later shows it's hot, memoize by message id at
the view layer.

## Error handling

- `parse` is total. Bad UTF-8 isn't a concern (input type is `&str`).
  Unclosed `**`, stray `||`, mismatched fences all degrade to literal
  text in the AST.
- `parse_markdown` WASM bridge: `serde-wasm-bindgen::to_value` on a
  pure-data AST cannot fail in practice; we `.unwrap()` it. (If it
  ever does, that's a bug in `serde-wasm-bindgen`, not user input.)
- Web FFI: if `parse_markdown` throws (shouldn't), Gleam side falls
  back to rendering the body as a single `Text` block. Logged to
  `console.error` so we notice in dev.
- Disallowed URL schemes don't throw — they render as literal text.

## Testing

**`sunset-markdown`:** unit + golden-file tests as above. Run with
`cargo nextest run -p sunset-markdown`. Property tests gated behind
the existing dev-deps shape.

**`sunset-core-wasm`:** add a `wasm-bindgen-test` that round-trips a
handful of inputs through `parse_markdown` and checks the JS-side
shape (this catches `serde` derive drift).

**Web:** new `web/test/markdown_test.gleam` exercising `render` on
representative inputs (each AST variant at least once, edge cases for
links + spoilers). Existing Playwright e2e suite gets one new test:
type a message containing `**bold** [link](https://example.com)
\`code\``, send it, assert the rendered DOM contains `<strong>`,
`<a href="https://example.com">`, and `<code>`.

**Composer:** Playwright covers `Enter` sends, `Shift+Enter` adds a
newline, `Cmd+B` wraps selection, textarea grows then scrolls.

## Migration

None — wire format is unchanged. Existing messages (plain text) parse
to a single `Paragraph(vec![Text(…)])` and render identically to
before.

## Open questions

None at design time. Implementation may surface small tokenizer
ambiguities (e.g. `***x***` — Discord renders as bold-italic; we'll
mirror that). These get resolved in the implementation plan with
golden-file tests, not by amending this spec.
