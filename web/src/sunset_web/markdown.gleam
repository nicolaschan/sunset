//// Render a message body string to a Lustre element by parsing it as
//// Discord-flavored markdown. The parser lives in the Rust
//// `sunset-markdown` crate and is reached through `markdown.ffi.mjs`.
////
//// The AST shape is an opaque JS value from Gleam's perspective. We
//// treat it as `dynamic.Dynamic` and decode it with the
//// `gleam/dynamic/decode` API used elsewhere in the codebase.

import gleam/dynamic.{type Dynamic}
import gleam/dynamic/decode
import gleam/int
import gleam/list
import gleam/option.{type Option}
import gleam/string
import lustre/attribute
import lustre/element.{type Element}
import lustre/element/html
import lustre/event

import sunset_web/theme.{type Palette}

@external(javascript, "./markdown.ffi.mjs", "parseMarkdown")
fn parse_markdown_ffi(body: String) -> Dynamic

/// Parse a body string to its block-level AST. Calls the Rust parser
/// over FFI; on any decode failure returns a single Paragraph wrapping
/// the body as literal text.
///
/// Exposed so tests can assert against parsed AST without re-rendering.
pub fn parse(body: String) -> List(Block) {
  let ast = parse_markdown_ffi(body)
  case decode.run(ast, decode.list(block_decoder())) {
    Ok(bs) -> bs
    Error(_) -> [Paragraph([Text(body)])]
  }
}

/// A key uniquely identifying a spoiler span within a message.
/// Used to track revealed state in the top-level Model.
pub type SpoilerKey {
  SpoilerKey(message_id: String, offset: Int)
}

/// Rendering context threaded through all render functions.
type Ctx(msg) {
  Ctx(
    palette: Palette,
    message_id: String,
    is_revealed: fn(SpoilerKey) -> Bool,
    on_toggle: fn(SpoilerKey) -> msg,
  )
}

pub fn render(
  body: String,
  message_id: String,
  is_spoiler_revealed: fn(SpoilerKey) -> Bool,
  on_toggle_spoiler: fn(SpoilerKey) -> msg,
  p: Palette,
) -> Element(msg) {
  render_blocks(parse(body), message_id, is_spoiler_revealed, on_toggle_spoiler, p)
}

/// Render a pre-parsed AST to a Lustre element. Used by `render` and
/// directly by tests that build AST values by hand to avoid the FFI
/// dependency in unit-test environments.
pub fn render_blocks(
  blocks: List(Block),
  message_id: String,
  is_spoiler_revealed: fn(SpoilerKey) -> Bool,
  on_toggle_spoiler: fn(SpoilerKey) -> msg,
  p: Palette,
) -> Element(msg) {
  let ctx =
    Ctx(
      palette: p,
      message_id: message_id,
      is_revealed: is_spoiler_revealed,
      on_toggle: on_toggle_spoiler,
    )
  html.div(
    [],
    list.index_map(blocks, fn(b, i) { render_block(b, ctx, i * 1_000_000) }),
  )
}

/// Strip all formatting and return concatenated text. Useful for
/// notification bodies and `aria-label`s. Delegates to
/// `sunset_markdown::to_plain` in the Rust core via WASM FFI.
@external(javascript, "./markdown.ffi.mjs", "toPlain")
pub fn to_plain(body: String) -> String

// ----- AST types -----
// Pub so tests can construct AST values directly without going through FFI.

pub type Block {
  Paragraph(content: List(Inline))
  Heading(level: Int, content: List(Inline))
  Quote(content: List(Block))
  UnorderedList(items: List(List(Block)))
  CodeBlock(language: Option(String), source: String)
}

pub type Inline {
  Text(value: String)
  Bold(children: List(Inline))
  Italic(children: List(Inline))
  Underline(children: List(Inline))
  Strikethrough(children: List(Inline))
  Spoiler(children: List(Inline))
  InlineCode(value: String)
  Link(label: List(Inline), url: String, autolink: Bool)
  LineBreak
}

// ----- Lazy decoder helpers -----
//
// The Block and Inline types are mutually recursive (Block → Inline → Block
// for Quote/UnorderedList). Gleam's `decode.list(inline_decoder())` would
// call `inline_decoder()` eagerly at construction time, causing infinite
// recursion before any data is decoded.
//
// We break the cycle by deferring decoder construction to decode-time via
// `decode.then`. `decode.dynamic` always succeeds and hands the raw value
// to the continuation. `decode.then(decode.dynamic, fn(_) { my_decoder() })`
// builds `my_decoder()` only when the outer decoder is actually run.

fn lazy_inline_list() -> decode.Decoder(List(Inline)) {
  decode.then(decode.dynamic, fn(_) { decode.list(inline_decoder()) })
}

fn lazy_block_list() -> decode.Decoder(List(Block)) {
  decode.then(decode.dynamic, fn(_) { decode.list(block_decoder()) })
}

// ----- Decoders -----
//
// Externally-tagged enums from serde-wasm-bindgen come through as
// either:
//   - {"VariantName": payload}    (variants with data)
//   - "VariantName"               (unit variants like LineBreak)
//
// `decode.one_of` tries each branch in order.

fn block_decoder() -> decode.Decoder(Block) {
  decode.one_of(paragraph_decoder(), [
    heading_decoder(),
    quote_decoder(),
    unordered_list_decoder(),
    code_block_decoder(),
  ])
}

fn paragraph_decoder() -> decode.Decoder(Block) {
  use inlines <- decode.field("Paragraph", lazy_inline_list())
  decode.success(Paragraph(inlines))
}

fn heading_decoder() -> decode.Decoder(Block) {
  use payload <- decode.field("Heading", heading_payload_decoder())
  decode.success(payload)
}

fn heading_payload_decoder() -> decode.Decoder(Block) {
  use level <- decode.field("level", decode.string)
  use content <- decode.field("content", lazy_inline_list())
  let n = case level {
    "H1" -> 1
    "H2" -> 2
    _ -> 3
  }
  decode.success(Heading(n, content))
}

fn quote_decoder() -> decode.Decoder(Block) {
  use blocks <- decode.field("Quote", lazy_block_list())
  decode.success(Quote(blocks))
}

fn unordered_list_decoder() -> decode.Decoder(Block) {
  use items <- decode.field(
    "UnorderedList",
    decode.then(decode.dynamic, fn(_) {
      decode.list(lazy_block_list())
    }),
  )
  decode.success(UnorderedList(items))
}

fn code_block_decoder() -> decode.Decoder(Block) {
  use payload <- decode.field("CodeBlock", code_block_payload_decoder())
  decode.success(payload)
}

fn code_block_payload_decoder() -> decode.Decoder(Block) {
  use language <- decode.field("language", decode.optional(decode.string))
  use source <- decode.field("source", decode.string)
  decode.success(CodeBlock(language, source))
}

fn inline_decoder() -> decode.Decoder(Inline) {
  decode.one_of(line_break_decoder(), [
    text_decoder(),
    bold_decoder(),
    italic_decoder(),
    underline_decoder(),
    strikethrough_decoder(),
    spoiler_decoder(),
    inline_code_decoder(),
    link_decoder(),
  ])
}

fn line_break_decoder() -> decode.Decoder(Inline) {
  use s <- decode.then(decode.string)
  case s {
    "LineBreak" -> decode.success(LineBreak)
    _ -> decode.failure(LineBreak, "not LineBreak")
  }
}

fn text_decoder() -> decode.Decoder(Inline) {
  use s <- decode.field("Text", decode.string)
  decode.success(Text(s))
}

fn inline_code_decoder() -> decode.Decoder(Inline) {
  use s <- decode.field("InlineCode", decode.string)
  decode.success(InlineCode(s))
}

fn bold_decoder() -> decode.Decoder(Inline) {
  use xs <- decode.field("Bold", lazy_inline_list())
  decode.success(Bold(xs))
}

fn italic_decoder() -> decode.Decoder(Inline) {
  use xs <- decode.field("Italic", lazy_inline_list())
  decode.success(Italic(xs))
}

fn underline_decoder() -> decode.Decoder(Inline) {
  use xs <- decode.field("Underline", lazy_inline_list())
  decode.success(Underline(xs))
}

fn strikethrough_decoder() -> decode.Decoder(Inline) {
  use xs <- decode.field("Strikethrough", lazy_inline_list())
  decode.success(Strikethrough(xs))
}

fn spoiler_decoder() -> decode.Decoder(Inline) {
  use xs <- decode.field("Spoiler", lazy_inline_list())
  decode.success(Spoiler(xs))
}

fn link_decoder() -> decode.Decoder(Inline) {
  use payload <- decode.field("Link", link_payload_decoder())
  decode.success(payload)
}

fn link_payload_decoder() -> decode.Decoder(Inline) {
  use label <- decode.field("label", lazy_inline_list())
  use url <- decode.field("url", decode.string)
  use autolink <- decode.field("autolink", decode.bool)
  decode.success(Link(label, url, autolink))
}

// ----- Block rendering -----

fn render_block(b: Block, ctx: Ctx(msg), offset: Int) -> Element(msg) {
  case b {
    Paragraph(inlines) ->
      html.p([], render_inlines(inlines, ctx, offset))

    Heading(level, content) -> {
      let tag = case level {
        1 -> html.h1
        2 -> html.h2
        _ -> html.h3
      }
      tag([], render_inlines(content, ctx, offset))
    }

    Quote(blocks) ->
      html.blockquote(
        [
          attribute.attribute(
            "style",
            "border-left: 3px solid var(--border, #888); padding-left: 8px; color: var(--text-muted, inherit); margin: 0;",
          ),
        ],
        list.index_map(blocks, fn(b, i) { render_block(b, ctx, offset * 100 + i) }),
      )

    UnorderedList(items) ->
      html.ul(
        [],
        list.index_map(items, fn(item, i) {
          html.li(
            [],
            list.index_map(item, fn(b, j) {
              render_block(b, ctx, offset * 100 + i * 10 + j)
            }),
          )
        }),
      )

    CodeBlock(language, source) -> render_code_block(language, source, ctx.palette)
  }
}

fn render_code_block(
  language: Option(String),
  source: String,
  _p: Palette,
) -> Element(msg) {
  let pill = case language {
    option.Some(lang) -> [
      html.span(
        [
          attribute.attribute(
            "style",
            "position: absolute; top: 4px; right: 8px; font-size: 10px; text-transform: uppercase; opacity: 0.6;",
          ),
        ],
        [html.text(lang)],
      ),
    ]
    option.None -> []
  }
  html.div(
    [
      attribute.attribute(
        "style",
        "position: relative; background: rgba(0,0,0,0.06); border-radius: 6px; padding: 8px 12px; margin: 4px 0;",
      ),
    ],
    list.append(pill, [
      html.pre(
        [attribute.attribute("style", "margin: 0; white-space: pre-wrap;")],
        [
          html.code(
            [attribute.attribute("style", "font-family: " <> theme.font_mono <> ";")],
            [html.text(source)],
          ),
        ],
      ),
    ]),
  )
}

// ----- Inline rendering -----

fn render_inlines(
  is: List(Inline),
  ctx: Ctx(msg),
  offset_base: Int,
) -> List(Element(msg)) {
  list.index_map(is, fn(i, idx) { render_inline(i, ctx, offset_base + idx) })
  |> list.flatten()
}

fn render_inline(i: Inline, ctx: Ctx(msg), offset: Int) -> List(Element(msg)) {
  case i {
    Text(s) -> [html.text(s)]
    Bold(xs) -> [
      html.strong([], render_inlines(xs, ctx, offset * 100)),
    ]
    Italic(xs) -> [html.em([], render_inlines(xs, ctx, offset * 100))]
    Underline(xs) -> [
      html.u([], render_inlines(xs, ctx, offset * 100)),
    ]
    Strikethrough(xs) -> [
      html.s([], render_inlines(xs, ctx, offset * 100)),
    ]
    InlineCode(s) -> [
      html.code(
        [
          attribute.attribute(
            "style",
            "font-family: "
              <> theme.font_mono
              <> "; background: rgba(0,0,0,0.1); padding: 0 4px; border-radius: 3px;",
          ),
        ],
        [html.text(s)],
      ),
    ]
    LineBreak -> [html.br([])]
    Spoiler(xs) -> [render_spoiler(xs, ctx, offset)]
    Link(label, url, autolink) -> [render_link(label, url, autolink, ctx, offset)]
  }
}

fn render_link(
  label: List(Inline),
  url: String,
  autolink: Bool,
  ctx: Ctx(msg),
  offset: Int,
) -> Element(msg) {
  case allowed_scheme(url) {
    True -> {
      let base_attrs = [
        attribute.href(url),
        attribute.target("_blank"),
        attribute.rel("noopener noreferrer"),
      ]
      let attrs = case autolink {
        True -> base_attrs
        False -> [attribute.title(url), ..base_attrs]
      }
      html.a(attrs, render_inlines(label, ctx, offset * 100))
    }
    False -> {
      // Render as plain text: label + " (" + url + ")" so the user can
      // still see what was sent.
      html.span(
        [],
        list.append(
          render_inlines(label, ctx, offset * 100),
          [html.text(" (" <> url <> ")")],
        ),
      )
    }
  }
}

fn allowed_scheme(url: String) -> Bool {
  string.starts_with(url, "http://")
  || string.starts_with(url, "https://")
  || string.starts_with(url, "mailto:")
}

fn render_spoiler(
  xs: List(Inline),
  ctx: Ctx(msg),
  offset: Int,
) -> Element(msg) {
  let key = SpoilerKey(ctx.message_id, offset)
  let revealed = ctx.is_revealed(key)
  let style = case revealed {
    True -> "background: rgba(0,0,0,0.05); border-radius: 3px; padding: 0 2px;"
    False ->
      "background: var(--text-muted, #888); color: transparent; border-radius: 3px; padding: 0 2px; cursor: pointer; user-select: none;"
  }
  html.span(
    [
      attribute.class("spoiler"),
      attribute.attribute("data-msg-id", ctx.message_id),
      attribute.attribute("data-offset", int.to_string(offset)),
      attribute.attribute("style", style),
      event.on_click(ctx.on_toggle(key)),
    ],
    render_inlines(xs, ctx, offset * 100),
  )
}
